//! 确定性 Todo 生命周期操作的短路执行。
//!
//! Issue #361 验收目标：`complete_todos` / `restore_todos` 这类确定性操作
//! 不应默认携带完整聊天历史（含旧知识证据、联网结果、长历史）进入 LLM Tool
//! Loop。这里在服务端从可见快照直接解析真实 todo_id 后执行工具，只有以下
//! 条件**全部**满足时才短路：
//!
//! - 私聊（群聊的 actor / 确认语义保持不变）；
//! - 用户文本是高置信 Todo 确认意图且只包含一种动作（完成或恢复；删除必须
//!   二次确认，仍走现有流程）；
//! - 文本可严格解析出显式可见编号（中文数字、日期混写、歧义表述一律不短路）；
//! - 请求级可见快照或 interaction session 的最近可见列表存在且新鲜；
//! - 所有编号都落在快照范围内（每个编号唯一对应一条快照条目）；
//! - 对应工具在服务端白名单中。
//!
//! 任一条件不满足（歧义、缺快照、权限不足、需要确认）都返回 `None`，保持
//! 现有 Agent Runtime / Tool Loop 流程。本模块不改变编号到真实 ID 的解析语义：
//! 解析、pending、回执和成功验真仍复用 Todo Tool 与 flow 既有实现。

use std::sync::Arc;

use qq_maid_common::identity_context::ConversationKind;
use qq_maid_llm::{
    agent_loop::{AgentRunDiagnostics, AgentStopReason, ToolExecutionAttempt, ToolExecutionResult},
    tool::DynTool,
};
use serde_json::json;

use crate::{
    error::LlmError,
    runtime::{
        respond::{
            RespondRequest, RespondResponse, RustRespondService,
            agent_composition::AgentReplySource,
            common::{session_error, tool_context_from_request, tool_conversation_from_request},
            interaction_state::respond_interaction_meta,
            llm_service::{RespondOutput, response_from_output},
        },
        session::{SessionMeta, SessionRecord},
        tools::{
            agent_turn::{
                ToolTurnPostprocessContext, agent_turn_diagnostics, postprocess_tool_turn,
            },
            todo::{CompleteTodoTool, RestoreTodoTool, TodoStore, valid_last_visible_todo_query},
        },
        visible_entity::{VisibleEntityRequestContext, VisibleEntitySelectionScope},
    },
    util::metrics::LlmMetrics,
};

use super::super::{
    common::{COMPLETE_TODOS_TOOL_NAME, RESTORE_TODOS_TOOL_NAME},
    scope::SelectionScope,
    visible_entity::todo_selection_scope_from_visible_snapshot,
};

/// 确定性 Todo 短路的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeterministicTodoAction {
    Complete,
    Restore,
}

impl DeterministicTodoAction {
    const fn tool_name(self) -> &'static str {
        match self {
            Self::Complete => COMPLETE_TODOS_TOOL_NAME,
            Self::Restore => RESTORE_TODOS_TOOL_NAME,
        }
    }
}

/// 短路计划：动作 + 显式可见编号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeterministicTodoPlan {
    action: DeterministicTodoAction,
    numbers: Vec<usize>,
}

/// 请求级可见快照或 interaction session 最近列表解析出的编号作用域。
enum DeterministicScope {
    /// 编号 -> 真实 todo_id；编号从 1 开始。
    Scoped(Arc<[String]>),
    /// 快照存在但校验失败（owner/account/scope 不匹配或过期），禁止回退。
    Blocked,
}

/// 解析确定性 Todo 短路的执行计划。
///
/// `active_interaction_session` 用于读取 interaction session 的最近可见列表
/// （`last_todo_query`）；`enabled_tool_names` 必须是服务端白名单过滤后的结果。
pub(crate) fn plan_deterministic_todo(
    req: &RespondRequest,
    active_interaction_session: Option<&SessionRecord>,
    enabled_tool_names: &[&str],
) -> Result<Option<DeterministicTodoPlan>, LlmError> {
    // 与 Agent 路径共用统一 conversation kind 解析，不能只看 group_id：
    // 只有明确解析为 Private / ServiceAccount 才短路；Channel、Group、Unknown
    // （含事件类型缺失、频道元数据等无法确认的场景）一律保持现有 Tool Loop 流程。
    let (kind, _) = tool_conversation_from_request(req);
    if !matches!(
        kind,
        ConversationKind::Private | ConversationKind::ServiceAccount
    ) {
        return Ok(None);
    }
    let user_text = req.effective_user_text();
    let Some(action) = classify_deterministic_action(&user_text) else {
        return Ok(None);
    };
    if !enabled_tool_names.contains(&action.tool_name()) {
        return Ok(None);
    }
    let Some(numbers) = extract_deterministic_numbers(&user_text) else {
        return Ok(None);
    };
    let scope = resolve_deterministic_scope(req, active_interaction_session)?;
    let ids = match scope {
        DeterministicScope::Scoped(ids) => ids,
        // 快照校验失败或缺失都不短路，保持现有流程。
        DeterministicScope::Blocked => return Ok(None),
    };
    if numbers.is_empty()
        || numbers
            .iter()
            .any(|number| *number == 0 || *number > ids.len())
    {
        return Ok(None);
    }
    Ok(Some(DeterministicTodoPlan { action, numbers }))
}

/// 从请求可见快照或 interaction session 最近列表解析编号作用域。
fn resolve_deterministic_scope(
    req: &RespondRequest,
    active_interaction_session: Option<&SessionRecord>,
) -> Result<DeterministicScope, LlmError> {
    let owner = TodoStore::owner(req.user_id.as_deref(), &req.scope_key);
    if let Some(scope) = todo_selection_scope_from_visible_snapshot(
        req.visible_entity_snapshot.as_ref(),
        VisibleEntityRequestContext {
            platform: &req.platform,
            account_id: req.account_id.as_deref(),
            scope_key: &req.scope_key,
            owner_key: Some(owner.key.as_str()),
            quoted_bot_lookup: req
                .quoted
                .as_ref()
                .is_some_and(|quoted| quoted.lookup_found && quoted.from_bot == Some(true)),
        },
    ) {
        return Ok(match scope {
            VisibleEntitySelectionScope::Scoped(ids) => DeterministicScope::Scoped(ids),
            VisibleEntitySelectionScope::Blocked => DeterministicScope::Blocked,
        });
    }
    let Some(session) = active_interaction_session else {
        return Ok(DeterministicScope::Blocked);
    };
    let mut snapshot = session.clone();
    let Some(query) = valid_last_visible_todo_query(&mut snapshot, &owner.key) else {
        return Ok(DeterministicScope::Blocked);
    };
    if query.result_ids.is_empty() {
        return Ok(DeterministicScope::Blocked);
    }
    Ok(DeterministicScope::Scoped(Arc::from(query.result_ids)))
}

/// 判定文本是否属于单一的确定性 Todo 确认动作。
fn classify_deterministic_action(text: &str) -> Option<DeterministicTodoAction> {
    use crate::runtime::tools::todo::route::{TodoIntentAction, todo_intent_action};
    if todo_intent_action(text) != TodoIntentAction::Confirm {
        return None;
    }
    let complete = ["完成", "做完"]
        .iter()
        .filter(|word| text.contains(**word))
        .count();
    let restore = ["恢复", "撤销", "撤消"]
        .iter()
        .filter(|word| text.contains(**word))
        .count();
    let delete = ["删除", "删掉", "移除", "取消", "作废", "清了", "清掉"]
        .iter()
        .filter(|word| text.contains(**word))
        .count();
    let actions = usize::from(complete > 0) + usize::from(restore > 0) + usize::from(delete > 0);
    if actions != 1 {
        // 混合动作（如“完成1恢复2”）或纯删除（需要二次确认）都不短路。
        return None;
    }
    if complete > 0 {
        Some(DeterministicTodoAction::Complete)
    } else if restore > 0 {
        Some(DeterministicTodoAction::Restore)
    } else {
        None
    }
}

/// 严格提取显式可见编号；任何无法归一的文本片段都返回 `None`。
///
/// 只接受“动作词 + 编号表达式”的最小文本，例如 `完成第1条`、`把1、2删除`、
/// `恢复1到3`。`第一条`（中文数字）、`删除1号的待办`（日期混写）等一律不短路。
fn extract_deterministic_numbers(text: &str) -> Option<Vec<usize>> {
    let compact = text
        .split_whitespace()
        .collect::<String>()
        .replace(['，', '、', '和', '与'], ",")
        .replace(['～', '—', '－'], "-")
        .replace("到", "-")
        .replace("至", "-");
    let mut rest = compact;
    for word in [
        "把",
        "请",
        "帮我",
        "帮忙",
        "一下",
        "待办",
        "任务",
        "事项",
        "第",
        "个",
        "项",
        "条",
        "都",
        "全部",
        "全",
        "完成",
        "做完",
        "恢复",
        "撤销",
        "撤消",
        "删除",
        "删掉",
        "移除",
        "取消",
        "作废",
        "清了",
        "清掉",
        "改为",
        "改成",
        "标为",
        "未完成",
        "已完成",
    ] {
        rest = rest.replace(word, "");
    }
    if rest.trim().is_empty() {
        return None;
    }
    let rest = rest.trim_matches(|ch: char| ch == ',' || ch == '-');
    if rest.is_empty() {
        return None;
    }
    const MAX_NUMBERS: usize = 20;
    if rest.contains('-') && !rest.contains(',') {
        let parts = rest.split('-').collect::<Vec<_>>();
        if parts.len() != 2 {
            return None;
        }
        let start = parts[0].parse::<usize>().ok()?;
        let end = parts[1].parse::<usize>().ok()?;
        if start == 0 || end < start || end.saturating_sub(start) + 1 > MAX_NUMBERS {
            return None;
        }
        return Some((start..=end).collect());
    }
    let mut numbers = Vec::new();
    for part in rest.split(',') {
        let number = part.parse::<usize>().ok()?;
        if number == 0 {
            return None;
        }
        if !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    if numbers.is_empty() || numbers.len() > MAX_NUMBERS {
        return None;
    }
    Some(numbers)
}

/// 用请求级可见快照作用域构造 scoped 工具，保证编号解析与 Agent Loop 一致。
fn scoped_tool_for_action(
    action: DeterministicTodoAction,
    todo_store: &TodoStore,
    session_store: &crate::runtime::session::SessionStore,
    notification_store: &crate::storage::notification::NotificationOutboxStore,
    ids: Arc<[String]>,
) -> DynTool {
    let scope = SelectionScope::Scoped(ids);
    match action {
        DeterministicTodoAction::Complete => Arc::new(
            CompleteTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .with_selection_scope(scope),
        ),
        DeterministicTodoAction::Restore => Arc::new(
            RestoreTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .with_selection_scope(scope),
        ),
    }
}

/// 直接执行确定性 Todo 工具，并组装成与 Agent Loop 同构的轨迹与 `RespondOutput`。
///
/// 执行语义（编号解析、pending、回执数据）与模型 Tool Loop 完全一致；
/// 这里的 `RespondOutput.agent` 只用于后续通用 Tool Turn 后处理消费。
async fn execute_deterministic_todo(
    todo_store: &TodoStore,
    session_store: &crate::runtime::session::SessionStore,
    notification_store: &crate::storage::notification::NotificationOutboxStore,
    plan: &DeterministicTodoPlan,
    req: &RespondRequest,
    ids: Arc<[String]>,
) -> Result<(RespondOutput, Vec<String>), LlmError> {
    let tool = scoped_tool_for_action(
        plan.action,
        todo_store,
        session_store,
        notification_store,
        ids,
    );
    // 确定性短路没有上游模型调用，因此没有 provider 下发的 call_id；这里派生
    // 稳定 synthetic tool_call_id 并同时写入 ToolContext 与 ToolExecutionAttempt，
    // 否则 TodoToolScope 的 dedup（task_id:tool_call_id）不生效，同一消息重发会
    // 把重复待办推进多个周期。
    let call_id = deterministic_tool_call_id(req, plan.action, &plan.numbers);
    let mut context = tool_context_from_request(req);
    context.tool_call_id = Some(call_id.clone());
    let arguments = json!({ "numbers": plan.numbers });
    let preparation = tool.prepare(&context, arguments)?;
    let output = tool.execute(context.clone(), preparation.arguments).await?;
    let succeeded = output
        .value
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let result = ToolExecutionResult {
        name: plan.action.tool_name().to_owned(),
        output: output.value,
        succeeded,
    };
    let attempt = ToolExecutionAttempt {
        result_index: 0,
        call_id,
        round: 0,
        retry_of: None,
    };
    let executed_tools = if succeeded {
        vec![plan.action.tool_name().to_owned()]
    } else {
        Vec::new()
    };
    let agent = AgentRunDiagnostics {
        model_rounds: 0,
        emitted_tools: vec![plan.action.tool_name().to_owned()],
        tool_execution_attempted: true,
        executed_tools: executed_tools.clone(),
        side_effecting_tools_started: vec![plan.action.tool_name().to_owned()],
        tool_results: vec![result.clone()],
        tool_attempts: vec![attempt],
        final_candidate_tool_result_start: None,
        tools_with_unknown_result: Vec::new(),
        streaming_fallback_used: false,
        stop_reason: Some(if succeeded {
            AgentStopReason::ToolUsed
        } else {
            AgentStopReason::Failed
        }),
    };
    Ok((
        RespondOutput {
            reply: String::new(),
            text: String::new(),
            markdown: None,
            parts: Vec::new(),
            metrics: LlmMetrics {
                provider: "rust".to_owned(),
                model: format!("{}:deterministic-short-circuit", plan.action.tool_name()),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 0,
            },
            usage: None,
            agent,
            display_contract: Default::default(),
            model_reply_empty: true,
        },
        executed_tools,
    ))
}

/// 生成确定性 Todo 短路的稳定 synthetic tool_call_id。
///
/// 至少由入站 message_id（与 `tool_context_from_request` 的 task_id 同源）、
/// 动作和规范化编号派生；同一 message_id + 动作 + 编号的重复请求会命中
/// `TodoToolScope` 的 dedup 历史，避免重复推进重复待办周期，而不同消息 /
/// 动作 / 编号必然产生不同 call id，不会跨请求串号。
fn deterministic_tool_call_id(
    req: &RespondRequest,
    action: DeterministicTodoAction,
    numbers: &[usize],
) -> String {
    let message_key = req
        .message_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("no-message-id");
    let action_key = match action {
        DeterministicTodoAction::Complete => "complete",
        DeterministicTodoAction::Restore => "restore",
    };
    let numbers_key = numbers
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("det-{action_key}:{message_key}:{numbers_key}")
}

impl RustRespondService {
    /// 尝试确定性 Todo 短路执行；不满足条件时返回 `None` 保持现有流程。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn try_deterministic_todo_confirm(
        &self,
        req: &RespondRequest,
        user_text: &str,
        meta: &SessionMeta,
        conversation_session: &mut SessionRecord,
        active_interaction_session: Option<&SessionRecord>,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let policy = self.resolve_agent_policy(req)?;
        let enabled_tool_names =
            crate::runtime::tools::todo::tool_policy::enabled_tool_names_for_request(
                &policy.enabled_tools,
                user_text,
            );
        let Some(plan) =
            plan_deterministic_todo(req, active_interaction_session, &enabled_tool_names)?
        else {
            return Ok(None);
        };
        let scope = resolve_deterministic_scope(req, active_interaction_session)?;
        let ids = match scope {
            DeterministicScope::Scoped(ids) => ids,
            DeterministicScope::Blocked => return Ok(None),
        };
        let (output, executed_tools) = execute_deterministic_todo(
            &self.task_store,
            &self.session_store,
            &self.notification_store,
            &plan,
            req,
            ids,
        )
        .await?;
        let interaction_meta = respond_interaction_meta(req);
        // Tool 内部通过自己的 session 记录写入了 last_todo_action / 快照；
        // 与 handle_chat 一致，postprocess 前必须从存储重读最新会话，否则
        // 回执可见快照会读到旧记录。
        let latest_session = self
            .session_store
            .get(&conversation_session.session_id)
            .map_err(session_error)?
            .ok_or_else(|| {
                LlmError::new(
                    "session_missing",
                    format!(
                        "session `{}` disappeared after deterministic todo short circuit",
                        conversation_session.session_id
                    ),
                    "session",
                )
            })?;
        // conversation session 只承载公开聊天历史；把工具已更新的领域状态合并回
        // 最新记录，避免旧 SessionRecord 覆盖 last_todo_query / last_todo_action。
        let mut latest_session = latest_session;
        latest_session.state = conversation_session.state.clone();
        *conversation_session = latest_session;
        let postprocess = postprocess_tool_turn(
            ToolTurnPostprocessContext {
                session_store: &self.session_store,
                task_store: &self.task_store,
                conversation_session,
                meta,
                interaction_meta: &interaction_meta,
                req,
                reply_source: AgentReplySource::DeterministicCommand,
            },
            output,
        )?;
        let reply = postprocess.output.reply.clone();
        self.session_store
            .append_exchange(conversation_session, user_text, &reply)
            .map_err(session_error)?;

        let mut response = response_from_output(postprocess.output);
        response.session_id = Some(conversation_session.session_id.clone());
        response.command = postprocess.outcome.primary_command();
        response.handled = Some(true);
        let agent_diagnostics = agent_turn_diagnostics(Some(&postprocess.outcome));
        let mut diagnostics = json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": false,
            "used_knowledge": false,
            "used_search": false,
            "respond_route": "agent_runtime",
            "route_reason": "deterministic_todo_short_circuit",
            "tool_calling_available": true,
            "tool_call_emitted": true,
            "tool_execution_attempted": true,
            "tool_calling_used": true,
            "agent_result": "tool_used",
            "stop_reason": "tool_used",
            "tool_calling_enabled": true,
            "agent_executed_tools": executed_tools,
            "agent_turn_status": agent_diagnostics["agent_turn_status"].clone(),
            "tool_outcomes": agent_diagnostics["tool_outcomes"].clone(),
        });
        if let Some(fields) = diagnostics.as_object_mut() {
            postprocess.diagnostics.extend_response_diagnostics(fields);
        }
        response.diagnostics = Some(diagnostics);
        response.visible_entity_snapshot = postprocess.outcome.visible_entity_snapshot.clone();
        Ok(Some(response))
    }
}
