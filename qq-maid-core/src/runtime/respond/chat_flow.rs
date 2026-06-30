//! 普通聊天流程。
//!
//! 承担 `RustRespondService` 中"兜底聊天"路径的实现：
//! 组装 LLM 请求、发起调用、保存对话记录、自动生成会话标题等。

use std::{future::Future, pin::Pin, sync::OnceLock};

use regex::Regex;
use serde_json::{Value, json};

use crate::{
    error::LlmError,
    provider::types::{ChatMessage, ChatRole},
    runtime::{
        prompt::{MemberIdMatch, build_member_identity_context, unknown_member_id_reply},
        session::{DEFAULT_SESSION_TITLE, SessionMeta, SessionRecord, redact_sensitive_text},
    },
};

use super::{
    RespondPurpose, RespondRequest, RespondResponse, RustRespondService,
    common::{
        SESSION_HISTORY_MESSAGE_LIMIT, SESSION_STATE_SHORT_TEXT_LIMIT, command_response,
        empty_respond_request, memory_error, merge_metadata, session_error, state_string,
        truncate_chars,
    },
    llm_service::{ChatService, LlmChatService, response_from_output},
    session_flow::build_session_context,
    title::{context_session_title, generate_session_title},
};

impl RustRespondService {
    /// 处理普通聊天请求。
    ///
    /// 1. 空消息直接返回提示。
    /// 2. 更新会话状态（话题、场景、模式等）。
    /// 3. 处理成员编号 @ 提及。
    /// 4. 构建会话上下文与记忆上下文。
    /// 5. 调用 LLM 获取回复。
    /// 6. 保存对话记录。
    /// 7. 尝试自动生成会话标题。
    pub(super) async fn handle_chat(
        &self,
        req: RespondRequest,
        user_text: String,
        meta: SessionMeta,
        mut session: SessionRecord,
    ) -> Result<RespondResponse, LlmError> {
        if user_text.trim().is_empty() {
            let reply = "唔，小女仆在。可以直接说要我看哪一块。";
            self.session_store
                .append_exchange(&mut session, &user_text, reply)
                .map_err(session_error)?;
            return Ok(command_response(
                reply,
                Some(session.session_id),
                Some("empty_chat"),
            ));
        }

        update_session_state_from_user(&mut session, &user_text);
        let is_group_chat = meta
            .group_id
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        // 群聊里不要求用户先带成员编号；成员映射仍保留给私聊或明确编号的场景，
        // 避免群里普通三位数字被误判成身份切换或触发未知编号追问。
        let member_matches = if is_group_chat {
            Vec::new()
        } else {
            self.prompt_config.find_member_id_mentions(&user_text)?
        };
        if !is_group_chat
            && let Some(unknown) = member_matches.iter().find(|item| item.name.is_none())
        {
            let mapping = self.prompt_config.load_member_id_mapping()?;
            let reply = unknown_member_id_reply(&unknown.member_id, &mapping);
            self.session_store
                .append_exchange(&mut session, &user_text, &reply)
                .map_err(session_error)?;
            return Ok(command_response(
                reply,
                Some(session.session_id),
                Some("member_id_unknown"),
            ));
        }
        update_session_speaker_hint(&mut session, &member_matches);

        let mut session_context = build_session_context(&session);
        if let Some(identity_context) = build_member_identity_context(&member_matches) {
            session_context.push_str("\n\n");
            session_context.push_str(&identity_context);
        }

        let knowledge_context = self.knowledge_index.search_context(&user_text)?;
        let used_knowledge = !knowledge_context.text.trim().is_empty();
        let memory_context = self.build_memory_context(&meta)?;
        let used_memory = !memory_context.trim().is_empty();
        let system_prompts = if is_group_chat {
            self.prompt_config.load_static_prompts_only()?
        } else {
            self.prompt_config.load_system_prompts()?
        };
        let chat_req = RespondRequest {
            session_id: session.session_id.clone(),
            purpose: RespondPurpose::Chat,
            user_text: user_text.clone(),
            system_prompts,
            memory_context,
            knowledge_context: knowledge_context.text.clone(),
            session_context,
            history_messages: recent_session_messages(&session, SESSION_HISTORY_MESSAGE_LIMIT),
            scope_key: meta.scope_key.clone(),
            user_id: meta.user_id.clone(),
            group_id: meta.group_id.clone(),
            guild_id: meta.guild_id.clone(),
            channel_id: meta.channel_id.clone(),
            message_id: req.message_id.clone(),
            timestamp: req.timestamp.clone(),
            platform: meta.platform.clone(),
            event_type: req.event_type.clone(),
            metadata: merge_metadata(
                req.metadata,
                &[
                    ("purpose", "chat"),
                    ("platform", meta.platform.as_str()),
                    ("scope_key", meta.scope_key.as_str()),
                ],
            ),
            ..empty_respond_request()
        };
        let service = LlmChatService::new(self.provider.clone());
        let use_tool_loop =
            self.tool_calling_enabled && !is_group_chat && service.supports_tool_calling(None);
        let todo_requirement = if use_tool_loop {
            required_todo_tool_kind(&user_text, &session)
        } else {
            None
        };
        let (output, tool_retry_count, required_tool_called) = if use_tool_loop {
            // 明显的 Todo 写操作必须真的执行对应 Tool；否则模型可能只口头声称
            // “已生成草稿/已完成”，但 pending/session 实际没有发生状态变更。
            let (output, retry_count, required_tool_called) = self
                .respond_with_required_todo_tool(&service, chat_req, todo_requirement)
                .await?;
            (output, retry_count, required_tool_called)
        } else {
            (service.respond(chat_req).await?, 0, false)
        };

        let reply = output.reply.clone();
        let executed_tools = output.executed_tools.clone();
        if use_tool_loop {
            let mut latest_session = self
                .session_store
                .get(&session.session_id)
                .map_err(session_error)?
                .ok_or_else(|| {
                    LlmError::new(
                        "session_missing",
                        format!(
                            "session `{}` disappeared after tool loop",
                            session.session_id
                        ),
                        "session",
                    )
                })?;
            // Tool 执行会基于同一 active session 保存 pending/最近 Todo 查询等字段；
            // 这里只把本轮聊天在调用模型前更新的状态合并回最新记录，避免旧 SessionRecord 覆盖工具结果。
            latest_session.state = session.state.clone();
            session = latest_session;
        }
        self.session_store
            .append_exchange(&mut session, &user_text, &reply)
            .map_err(session_error)?;
        self.schedule_auto_title(session.clone());

        let mut response = response_from_output(output);
        response.session_id = Some(session.session_id);
        response.command = None;
        response.handled = Some(true);
        response.diagnostics = Some(json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": used_memory,
            "used_knowledge": used_knowledge,
            "knowledge_hit_count": knowledge_context.hit_count,
            "used_search": false,
            "tool_calling_enabled": use_tool_loop,
            "tool_loop_executed_tools": executed_tools,
            "required_tool_kind": todo_requirement.map(TodoMutationToolKind::as_str),
            "required_tool_called": required_tool_called,
            "tool_retry_count": tool_retry_count,
            "error_code": if use_tool_loop && todo_requirement.is_some() && !required_tool_called {
                json!("required_tool_not_called")
            } else {
                Value::Null
            },
        }));
        Ok(response)
    }

    async fn respond_with_required_todo_tool(
        &self,
        service: &LlmChatService,
        chat_req: RespondRequest,
        required_tool_kind: Option<TodoMutationToolKind>,
    ) -> Result<(super::llm_service::RespondOutput, usize, bool), LlmError> {
        let mut retry_count = 0;
        let mut request = chat_req.clone();
        let mut output = service
            .respond_with_tools(
                request.clone(),
                self.tool_registry.clone(),
                self.tool_calling_max_rounds,
            )
            .await?;
        let mut required_called = required_tool_kind
            .as_ref()
            .is_none_or(|kind| kind.matches_executed_tools(&output.executed_tools));

        if required_called {
            return Ok((output, retry_count, true));
        }

        retry_count = 1;
        // 只做一次受控重试；仍未调用 Tool 时直接返回中文失败提示，禁止透传假成功。
        request.system_prompts.push(
            "本轮用户是在执行待办写操作。你必须调用对应的 Todo Tool；在 Tool 返回成功前，禁止回复“已生成草稿”“已记录”“已完成”“已取消”“已恢复”“已删除”等成功性文案。".to_owned(),
        );
        request.metadata = merge_metadata(
            request.metadata,
            &[("tool_retry_reason", "required_todo_tool_not_called")],
        );
        output = service
            .respond_with_tools(
                request,
                self.tool_registry.clone(),
                self.tool_calling_max_rounds,
            )
            .await?;
        required_called = required_tool_kind
            .as_ref()
            .is_none_or(|kind| kind.matches_executed_tools(&output.executed_tools));
        if required_called {
            return Ok((output, retry_count, true));
        }

        let reply = todo_required_tool_not_called_reply(required_tool_kind);
        let response = super::llm_service::RespondOutput {
            reply: reply.clone(),
            text: reply.clone(),
            markdown: None,
            chat: super::types::ChatResponse::ok(
                reply,
                crate::util::metrics::LlmMetrics {
                    provider: "rust".to_owned(),
                    model: "tool-loop-guard".to_owned(),
                    stream: false,
                    ttfe_ms: None,
                    ttft_ms: None,
                    total_latency_ms: 0,
                },
                None,
            ),
            executed_tools: output.executed_tools.clone(),
        };
        Ok((response, retry_count, false))
    }

    /// 普通聊天真流式路径：复用非流式聊天的上下文构造和后处理，只替换 LLM 调用方式。
    pub async fn handle_chat_stream<F>(
        &self,
        req: RespondRequest,
        on_delta: F,
    ) -> Result<RespondResponse, LlmError>
    where
        F: FnMut(String) -> Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send>> + Send,
    {
        let user_text = req.effective_user_text();
        let meta = SessionMeta::new(
            req.scope_key.clone(),
            req.user_id.clone(),
            req.group_id.clone(),
            req.guild_id.clone(),
            req.channel_id.clone(),
            req.platform.clone(),
        );
        let mut session = self
            .session_store
            .get_or_create_active(&meta)
            .map_err(session_error)?;
        if user_text.trim().is_empty() {
            return self.handle_chat(req, user_text, meta, session).await;
        }

        update_session_state_from_user(&mut session, &user_text);
        let is_group_chat = meta
            .group_id
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        let member_matches = if is_group_chat {
            Vec::new()
        } else {
            self.prompt_config.find_member_id_mentions(&user_text)?
        };
        if !is_group_chat
            && let Some(unknown) = member_matches.iter().find(|item| item.name.is_none())
        {
            let mapping = self.prompt_config.load_member_id_mapping()?;
            let reply = unknown_member_id_reply(&unknown.member_id, &mapping);
            self.session_store
                .append_exchange(&mut session, &user_text, &reply)
                .map_err(session_error)?;
            return Ok(command_response(
                reply,
                Some(session.session_id),
                Some("member_id_unknown"),
            ));
        }
        update_session_speaker_hint(&mut session, &member_matches);

        let mut session_context = build_session_context(&session);
        if let Some(identity_context) = build_member_identity_context(&member_matches) {
            session_context.push_str("\n\n");
            session_context.push_str(&identity_context);
        }

        let knowledge_context = self.knowledge_index.search_context(&user_text)?;
        let used_knowledge = !knowledge_context.text.trim().is_empty();
        let memory_context = self.build_memory_context(&meta)?;
        let used_memory = !memory_context.trim().is_empty();
        let system_prompts = if is_group_chat {
            self.prompt_config.load_static_prompts_only()?
        } else {
            self.prompt_config.load_system_prompts()?
        };
        let service = LlmChatService::new(self.provider.clone());
        let output = service
            .stream_respond(
                RespondRequest {
                    session_id: session.session_id.clone(),
                    purpose: RespondPurpose::Chat,
                    user_text: user_text.clone(),
                    system_prompts,
                    memory_context,
                    knowledge_context: knowledge_context.text.clone(),
                    session_context,
                    history_messages: recent_session_messages(
                        &session,
                        SESSION_HISTORY_MESSAGE_LIMIT,
                    ),
                    scope_key: meta.scope_key.clone(),
                    user_id: meta.user_id.clone(),
                    group_id: meta.group_id.clone(),
                    guild_id: meta.guild_id.clone(),
                    channel_id: meta.channel_id.clone(),
                    message_id: req.message_id.clone(),
                    timestamp: req.timestamp.clone(),
                    platform: meta.platform.clone(),
                    event_type: req.event_type.clone(),
                    metadata: merge_metadata(
                        req.metadata,
                        &[
                            ("purpose", "chat"),
                            ("platform", meta.platform.as_str()),
                            ("scope_key", meta.scope_key.as_str()),
                        ],
                    ),
                    ..empty_respond_request()
                },
                on_delta,
            )
            .await?;

        let reply = output.reply.clone();
        self.session_store
            .append_exchange(&mut session, &user_text, &reply)
            .map_err(session_error)?;
        self.schedule_auto_title(session.clone());

        let mut response = response_from_output(output);
        response.session_id = Some(session.session_id);
        response.command = None;
        response.handled = Some(true);
        response.diagnostics = Some(json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": used_memory,
            "used_knowledge": used_knowledge,
            "knowledge_hit_count": knowledge_context.hit_count,
            "used_search": false,
        }));
        Ok(response)
    }

    /// 从长期记忆存储中读取当前请求可访问的最近记录，组装为系统提示上下文。
    ///
    /// 个人和群记忆先在 SQL 中限定各自合法作用域，再沿用原有 `row_id DESC LIMIT 12`
    /// 合并排序；这里不做固定配额，避免低排序记忆挤掉原本更靠前的合法记忆。
    pub(super) fn build_memory_context(&self, meta: &SessionMeta) -> Result<String, LlmError> {
        let records = self
            .memory_store
            .list_accessible_for_context(meta.user_id.as_deref(), meta.group_id.as_deref(), 12)
            .map_err(memory_error)?;
        let rows = records
            .iter()
            .filter(|record| !record.content.trim().is_empty())
            .map(|record| format!("- [{}] {}", record.ts, record.content))
            .collect::<Vec<_>>();
        if rows.is_empty() {
            Ok(String::new())
        } else {
            let mut context = format!(
                "以下是用户明确要求记录的本地记忆，只作为参考，不要机械复述：\n{}",
                rows.join("\n")
            );
            if meta
                .group_id
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                context.push_str(
                    "\n群聊隐私约束：个人记忆只用于理解当前发言者，不要主动披露、列举或转述个人记忆。",
                );
            }
            Ok(context)
        }
    }

    /// 如果会话标题还是默认值，且用户消息轮数在 2~4 之间，则后台尝试生成标题。
    ///
    /// 主聊天回复已经完成落库，标题只是展示增强；不能让标题模型的慢响应、
    /// 失败或取消影响本轮 `Completed`。后台任务只允许条件更新标题，不能保存
    /// 旧的完整会话快照，否则会覆盖期间继续写入的历史、pending 或手工重命名。
    fn schedule_auto_title(&self, session: SessionRecord) {
        let Some(title_model) = self.title_model.clone() else {
            return;
        };
        if session.title != DEFAULT_SESSION_TITLE {
            return;
        }
        let user_message_count = session
            .history
            .iter()
            .filter(|message| message.role == "user" && !message.content.trim().is_empty())
            .count();
        if !(2..=4).contains(&user_message_count) {
            return;
        }

        let provider = self.provider.clone();
        let session_store = self.session_store.clone();
        let session_id = session.session_id.clone();
        let history = session.history.clone();
        tokio::spawn(async move {
            match generate_session_title(provider.as_ref(), &title_model, &history, false).await {
                Ok(title) => {
                    match session_store.update_title_if_current(
                        &session_id,
                        DEFAULT_SESSION_TITLE,
                        &title,
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::debug!(
                                session_id = %session_id,
                                "generated session title ignored because current title changed"
                            );
                        }
                        Err(err) => {
                            tracing::warn!(
                                error = %err.message(),
                                session_id = %session_id,
                                "failed to save generated session title"
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        session_id = %session_id,
                        "session auto title generation failed"
                    );
                }
            }
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoMutationToolKind {
    Create,
    Complete,
    Cancel,
    Restore,
    Delete,
}

impl TodoMutationToolKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Complete => "complete",
            Self::Cancel => "cancel",
            Self::Restore => "restore",
            Self::Delete => "delete",
        }
    }

    fn required_tool_name(self) -> &'static str {
        match self {
            Self::Create => "create_todo",
            Self::Complete => "complete_todos",
            Self::Cancel => "cancel_todo",
            Self::Restore => "restore_todos",
            Self::Delete => "delete_todos",
        }
    }

    fn matches_executed_tools(self, executed_tools: &[String]) -> bool {
        executed_tools
            .iter()
            .any(|tool| tool == self.required_tool_name())
    }
}

/// 判断本轮是否需要强制调用某个 Todo 写操作 Tool，防止模型不调 Tool 却发假成功文案。
///
/// 设计约束：
/// - 查询意图始终最优先排除，永返回 `None`。
/// - 创建意图单独识别，不依赖已有 session 状态，但必须同时存在明确创建动词和创建目标。
/// - 完成/取消/恢复/删除只在存在 Todo 目标上下文时才强制：
///   明确提到待办/任务/todo，或有编号引用（依赖 `last_todo_query`），
///   或有最近对象引用（依赖 `last_todo_action`）。
/// - 编号引用仅在 session 存在 `last_todo_query` 时才算目标；最近对象引用仅在
///   `last_todo_action` 时才算目标。这样可避免“完成这个项目/取消明天会议/删除服务器上的旧日志”
///   等普通聊天被误判成 Todo 写操作。
fn required_todo_tool_kind(
    user_text: &str,
    session: &SessionRecord,
) -> Option<TodoMutationToolKind> {
    let text = user_text.trim();
    if text.is_empty() {
        return None;
    }
    // 查询意图始终最优先排除，不得进入 Todo 写操作受控重试。
    if looks_like_todo_query_text(text) {
        return None;
    }

    // 创建意图单独识别：不依赖已有 session 状态，但必须同时存在明确创建动词和创建目标，
    // 避免仅出现“待办”就误判为创建（如“待办功能怎么用”）。
    if let Some(kind) = detect_create_todo_kind(text) {
        return Some(kind);
    }

    // 完成/取消/恢复/删除必须同时存在 Todo 目标上下文，否则普通聊天里的
    // “完成项目/取消会议/删除日志”会被误强制成 Todo Tool，甚至真的修改用户待办。
    let has_todo_noun = contains_any(text, &["待办", "任务", "todo"]);
    let has_visible_reference =
        contains_visible_number_reference(text) && session.last_todo_query.is_some();
    let has_last_reference =
        contains_last_todo_reference(text) && session.last_todo_action.is_some();
    if !(has_todo_noun || has_visible_reference || has_last_reference) {
        return None;
    }

    // 检测写操作动词时屏蔽参照/状态子串，避免“已完成/刚恢复的那个”里的动词误被当成主操作。
    let operative = strip_todo_reference_and_status(text);
    // 顺序很关键：删除/恢复优先检测，避免“永久删除已完成待办”被“完成”抢先、“取消完成”（=恢复）被“取消”抢先。
    if contains_any(&operative, &["删除", "删掉", "移除", "永久删除"]) {
        return Some(TodoMutationToolKind::Delete);
    }
    if contains_any(&operative, &["恢复", "撤销完成", "恢复完成", "取消完成"]) {
        return Some(TodoMutationToolKind::Restore);
    }
    if contains_any(&operative, &["完成", "做完", "标记完成", "搞定"]) {
        return Some(TodoMutationToolKind::Complete);
    }
    if contains_any(&operative, &["取消", "不做了", "算了", "作废"]) {
        return Some(TodoMutationToolKind::Cancel);
    }
    None
}

/// 识别明确的待办创建意图。
///
/// 必须同时满足创建动词和创建目标：
/// - 创建动词：记一个 / 记个 / 帮我记 / 新增 / 添加 / 提醒我
/// - 创建目标：明确出现待办 / 任务，或“提醒我 + 具体事项”。
///
/// 例如“待办功能怎么用 / 这个待办是不是有 bug”只出现待办名词、没有创建动词，不会被误判为创建。
fn detect_create_todo_kind(text: &str) -> Option<TodoMutationToolKind> {
    let has_create_verb = contains_any(
        text,
        &["记一个", "记个", "帮我记", "新增", "添加", "提醒我"],
    );
    if !has_create_verb {
        return None;
    }
    let has_explicit_target = contains_any(text, &["待办", "任务"]);
    let has_reminder_target = text.contains("提醒我") && has_text_after_reminder(text);
    if has_explicit_target || has_reminder_target {
        Some(TodoMutationToolKind::Create)
    } else {
        None
    }
}

/// “提醒我 + 具体事项”判定：“提醒我”之后还有非空内容才算创建目标。
fn has_text_after_reminder(text: &str) -> bool {
    let Some((_, rest)) = text.split_once("提醒我") else {
        return false;
    };
    rest.trim().chars().count() >= 2
}

/// 检测明确的待办编号引用：“第 1 个 / 第一个 / 编号 2 / 第 3 条”等。
///
/// 只检测是否存在编号引用句式本身，不检测孤立数字，避免普通文本里出现数字就误强制 Todo Tool。
fn contains_visible_number_reference(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"第\s*[0-9一二三四五六七八九十]+\s*[个条项]|编号\s*[0-9]+")
            .expect("todo number reference regex")
    });
    re.is_match(text)
}

/// 检测最近待办对象引用：“刚才那个 / 刚恢复的那个 / 它 / 把它…”等。
///
/// 不包含裸“那个”（会误命中“那个项目我完成了”这类非 Todo 语句）；“那个待办”已通过 `has_todo_noun` 覆盖。
/// 是否算作目标还需要 `session.last_todo_action` 存在，该约束在 `required_todo_tool_kind` 中处理。
fn contains_last_todo_reference(text: &str) -> bool {
    contains_any(
        text,
        &[
            "刚才那个",
            "刚恢复的那个",
            "刚完成的那个",
            "刚取消的那个",
            "刚删除的那个",
            "刚创建的那个",
            "刚新建的那个",
            "把它",
            "它",
        ],
    )
}

/// 屏蔽参照与状态子串（“已完成 / 已取消 / 刚恢复的那个 / 第 N 个”），返回只保留主操作动词的文本。
///
/// 这些子串里的“完成 / 取消 / 恢复”是状态/参照描述，不是本轮主操作，检测写操作动词时需先移除。
fn strip_todo_reference_and_status(text: &str) -> String {
    let mut out = text.to_owned();
    for phrase in [
        "刚恢复的那个",
        "刚完成的那个",
        "刚取消的那个",
        "刚删除的那个",
        "刚创建的那个",
        "刚新建的那个",
        "刚才那个",
        "已完成的",
        "已取消的",
        "已完成",
        "已取消",
        "把它",
        "它",
    ] {
        out = out.replace(phrase, " ");
    }
    // 移除编号引用，避免“第 N 个”干扰动词检测。
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"第\s*[0-9一二三四五六七八九十]+\s*[个条项]|编号\s*[0-9]+")
            .expect("todo number reference strip regex")
    });
    out = re.replace_all(&out, " ").to_string();
    out
}

fn looks_like_todo_query_text(text: &str) -> bool {
    let compact = text.trim();
    let mentions_todo = contains_any(compact, &["待办", "任务"]);
    let asks_list = contains_any(
        compact,
        &["看看", "看下", "看一下", "列出", "有哪些", "查看"],
    );
    mentions_todo
        && (asks_list
            || compact == "我的待办"
            || compact == "待办列表"
            || compact == "已完成的待办"
            || compact == "已取消的待办"
            || compact == "看看已完成"
            || compact == "看看已取消")
}

fn todo_required_tool_not_called_reply(required_tool_kind: Option<TodoMutationToolKind>) -> String {
    let action = match required_tool_kind {
        Some(TodoMutationToolKind::Create) => "新增待办",
        Some(TodoMutationToolKind::Complete) => "完成待办",
        Some(TodoMutationToolKind::Cancel) => "取消待办",
        Some(TodoMutationToolKind::Restore) => "恢复待办",
        Some(TodoMutationToolKind::Delete) => "删除待办",
        None => "处理待办",
    };
    format!("我这次没有真正执行到{action}操作。请再说一次，我会先调用待办工具，再告诉你结果。")
}

/// 从会话历史中截取最近的 N 条消息，转换为 LLM `ChatMessage` 格式。
///
/// 仅保留 user 和 assistant 角色，按时间正序返回。
pub(super) fn recent_session_messages(session: &SessionRecord, limit: usize) -> Vec<ChatMessage> {
    session
        .history
        .iter()
        .rev()
        .filter_map(|message| match message.role.as_str() {
            "user" => Some(ChatMessage {
                role: ChatRole::User,
                content: message.content.clone(),
            }),
            "assistant" => Some(ChatMessage {
                role: ChatRole::Assistant,
                content: message.content.clone(),
            }),
            _ => None,
        })
        .filter(|message| !message.content.trim().is_empty())
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}
/// 根据用户输入更新会话状态（话题、场景、模式、焦点等）。
fn update_session_state_from_user(session: &mut SessionRecord, user_text: &str) {
    let text = user_text.trim();
    if text.is_empty() {
        return;
    }
    let current_topic = state_string(session, "current_topic")
        .or_else(|| context_session_title(Some(session.title.as_str())));
    if current_topic.is_none() && !is_short_followup(text) {
        let topic = compact_topic(text, 32);
        if !topic.is_empty() {
            session
                .state
                .insert("current_topic".to_owned(), Value::String(topic.clone()));
        }
    }
    session
        .state
        .entry("active_scene")
        .or_insert_with(|| Value::String("默认会话".to_owned()));
    let mode = infer_expected_mode(text, state_string(session, "expected_mode").as_deref());
    session
        .state
        .insert("expected_mode".to_owned(), Value::String(mode));
    if let Some(focus) = infer_recent_session_focus(text) {
        set_short_state(session, "recent_session_focus", focus);
    }
    if current_topic.is_some() && looks_like_correction(text) {
        set_short_state(session, "last_user_correction", compact_topic(text, 48));
    }
}

/// 根据成员编号匹配结果更新会话中的说话者提示。
fn update_session_speaker_hint(session: &mut SessionRecord, matches: &[MemberIdMatch]) {
    let rows = matches
        .iter()
        .filter_map(|item| {
            let name = item.name.as_deref()?.trim();
            if name.is_empty() {
                None
            } else {
                Some(format!("{} {}", item.member_id, name))
            }
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return;
    }
    set_short_state(
        session,
        "current_speaker_hint",
        format!("本轮明确编号：{}", rows.join(" / ")),
    );
}

/// 将短文本写入会话状态，自动脱敏并截断。
fn set_short_state(session: &mut SessionRecord, key: &str, value: impl AsRef<str>) {
    let value = redact_sensitive_text(value.as_ref());
    let value = truncate_chars(&value, SESSION_STATE_SHORT_TEXT_LIMIT);
    if value.trim().is_empty() {
        return;
    }
    session
        .state
        .insert(key.to_owned(), Value::String(value.trim().to_owned()));
}

/// 从用户输入推断最近会话焦点类别（身份、场景、设定、记忆边界等）。
fn infer_recent_session_focus(text: &str) -> Option<&'static str> {
    if contains_any(text, &["前台", "身份", "切换", "编号", "成员", "说话者"]) {
        return Some("身份/成员识别");
    }
    if contains_any(text, &["场景", "背景", "上下文"]) {
        return Some("会话场景");
    }
    if contains_any(text, &["设定", "剧情", "世界观", "档案", "角色"]) {
        return Some("设定整理");
    }
    if contains_any(text, &["记忆", "记一下", "/memory", "/记忆", "/记"]) {
        return Some("长期记忆边界");
    }
    None
}

/// 从用户输入推断期望的对话模式（书记官整理 / 方案讨论 / 低电量陪伴 / 继续上一轮等）。
fn infer_expected_mode(text: &str, current_mode: Option<&str>) -> String {
    let lowered = text.to_ascii_lowercase();
    if [
        "codex",
        "readme",
        "wiki",
        "整理",
        "确认",
        "出版本",
        "存档",
        "归档",
        "文档",
        "修改说明",
    ]
    .iter()
    .any(|keyword| lowered.contains(&keyword.to_ascii_lowercase()))
    {
        return "书记官整理".to_owned();
    }
    if [
        "怎么定",
        "怎么改",
        "怎么处理",
        "选哪个",
        "要不要",
        "给几个方案",
        "方案",
    ]
    .iter()
    .any(|keyword| text.contains(keyword))
    {
        return "方案讨论".to_owned();
    }
    if ["累", "困", "焦虑", "睡不着", "不想动", "低电量"]
        .iter()
        .any(|keyword| text.contains(keyword))
    {
        return "低电量陪伴".to_owned();
    }
    if text.contains("继续") {
        return current_mode.unwrap_or("继续上一轮").to_owned();
    }
    current_mode.unwrap_or("陪聊 + 轻量整理").to_owned()
}

/// 判断用户输入是否包含修正性用语（"不是""应该是""补充"等）。
fn looks_like_correction(text: &str) -> bool {
    [
        "不是",
        "不对",
        "我的意思是",
        "我是说",
        "应该是",
        "其实",
        "补充",
        "还有",
        "漏了",
        "改成",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// 检查文本是否包含关键字列表中的任意一个。
fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// 判断是否为短接续句（字符数 <= 24）。
fn is_short_followup(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && text.chars().count() <= 24
}

/// 将用户输入压缩为简短话题词，去除首尾标点和"小女仆"称谓。
fn compact_topic(text: &str, max_length: usize) -> String {
    let mut topic = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(&[' ', '：', ':', '，', ',', '。', '.', '!', '！', '?', '？'][..])
        .replace("小女仆", "");
    topic = topic
        .trim_matches(&[' ', '：', ':', '，', ',', '。', '.', '!', '！', '?', '？'][..])
        .to_owned();
    truncate_chars(&topic, max_length)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::runtime::session::{LastTodoAction, LastTodoQuery, SessionRecord};
    use crate::runtime::todo::TodoStatus;

    use super::{TodoMutationToolKind, required_todo_tool_kind};

    /// 通过反序列化构造一个全默认字段的 session，便于隔离测试 Todo 意图判定。
    fn empty_session() -> SessionRecord {
        serde_json::from_value(json!({})).unwrap()
    }

    fn session_with_last_query() -> SessionRecord {
        let mut session = empty_session();
        session.last_todo_query = Some(LastTodoQuery {
            owner_key: "private:u1".to_owned(),
            query_type: "list".to_owned(),
            condition: String::new(),
            result_ids: vec!["item-1".to_owned()],
            created_at: "2026-06-30T00:00:00+08:00".to_owned(),
        });
        session
    }

    fn session_with_last_action() -> SessionRecord {
        let mut session = empty_session();
        session.last_todo_action = Some(LastTodoAction {
            owner_key: "private:u1".to_owned(),
            item_id: "item-1".to_owned(),
            title: "示例待办".to_owned(),
            action: "completed".to_owned(),
            resulting_status: TodoStatus::Completed,
            created_at: "2026-06-30T00:00:00+08:00".to_owned(),
        });
        session
    }

    fn assert_kind(text: &str, session: &SessionRecord, expected: Option<TodoMutationToolKind>) {
        assert_eq!(
            required_todo_tool_kind(text, session),
            expected,
            "text = {text:?}"
        );
    }

    // ----- 创建意图：不依赖 session 状态，但必须有明确创建动词和创建目标 -----

    #[test]
    fn create_intent_recognized_without_session_state() {
        let session = empty_session();
        assert_kind(
            "帮我记一个待办，今晚检查日志",
            &session,
            Some(TodoMutationToolKind::Create),
        );
        assert_kind(
            "新增一个任务，明天交报告",
            &session,
            Some(TodoMutationToolKind::Create),
        );
        assert_kind(
            "提醒我明天下午三点开会",
            &session,
            Some(TodoMutationToolKind::Create),
        );
    }

    #[test]
    fn create_intent_not_forced_for_todo_mentions_without_create_verb() {
        let session = empty_session();
        // 有待办但没有创建动词，不应被误判为创建。
        assert_kind("待办功能怎么用", &session, None);
        assert_kind("我们聊聊待办设计", &session, None);
        assert_kind("这个待办是不是有 bug", &session, None);
        assert_kind("为什么我的待办没显示", &session, None);
    }

    // ----- 非 Todo 普通聊天：不应强制任何 mutation Tool -----
    #[test]
    fn non_todo_chat_is_not_forced() {
        let session = empty_session();
        assert_kind("我终于完成这个项目了", &session, None);
        assert_kind("取消明天的会议", &session, None);
        assert_kind("删除服务器上的旧日志", &session, None);
        assert_kind("这个方案算了，不做了", &session, None);
        assert_kind("帮我恢复刚才删除的文档", &session, None);
    }

    // ----- 状态修改依赖 Todo 目标上下文：名词 / 编号 / 最近对象引用 -----

    #[test]
    fn mutation_with_todo_noun_recognized_without_session_state() {
        let session = empty_session();
        assert_kind(
            "完成第 1 个待办",
            &session,
            Some(TodoMutationToolKind::Complete),
        );
        assert_kind(
            "取消第 2 个任务",
            &session,
            Some(TodoMutationToolKind::Cancel),
        );
        // “已完成”是状态描述，不应被“完成”抢先；主操作是删除。
        assert_kind(
            "永久删除已完成待办第 3 个",
            &session,
            Some(TodoMutationToolKind::Delete),
        );
    }

    #[test]
    fn mutation_with_number_reference_requires_last_todo_query() {
        // last_todo_query 存在时，编号引用算 Todo 目标。
        let session = session_with_last_query();
        assert_kind(
            "完成第 1 个",
            &session,
            Some(TodoMutationToolKind::Complete),
        );
        assert_kind("取消第 1 个", &session, Some(TodoMutationToolKind::Cancel));

        // last_todo_query 缺失时，裸编号引用不应强制 Tool。
        let session = empty_session();
        assert_kind("完成第 1 个", &session, None);
        assert_kind("删除第 2 个", &session, None);
    }

    #[test]
    fn mutation_with_last_reference_requires_last_todo_action() {
        // last_todo_action 存在时，“刚才那个 / 刚恢复的那个 / 它”算 Todo 目标。
        let session = session_with_last_action();
        assert_kind(
            "把刚才那个完成",
            &session,
            Some(TodoMutationToolKind::Complete),
        );
        assert_kind(
            "取消刚恢复的那个",
            &session,
            Some(TodoMutationToolKind::Cancel),
        );
        assert_kind("删除它", &session, Some(TodoMutationToolKind::Delete));
        assert_kind("把它取消掉", &session, Some(TodoMutationToolKind::Cancel));
        assert_kind(
            "删除刚取消的那个",
            &session,
            Some(TodoMutationToolKind::Delete),
        );

        // last_todo_action 缺失时，最近对象引用不应强制 Tool。
        let session = empty_session();
        assert_kind("删除它", &session, None);
        assert_kind("取消刚才那个", &session, None);
    }

    #[test]
    fn mutation_restore_phrases_override_complete_and_cancel() {
        let session = empty_session();
        // “取消完成 / 恢复完成”表示撤销完成（=恢复），不应被“取消 / 完成”抢先。
        assert_kind(
            "取消完成第 1 个待办",
            &session,
            Some(TodoMutationToolKind::Restore),
        );
        assert_kind(
            "恢复完成第 2 个任务",
            &session,
            Some(TodoMutationToolKind::Restore),
        );
    }

    // ----- 查询意图最优先排除，永返回 None -----

    #[test]
    fn query_intent_is_always_excluded() {
        let session = session_with_last_query();
        assert_kind("看看我的待办", &session, None);
        assert_kind("有哪些任务", &session, None);
        assert_kind("列出已完成的待办", &session, None);
        assert_kind("看看已完成", &session, None);
        assert_kind("看看已取消", &session, None);
        assert_kind("我的待办", &session, None);
        assert_kind("待办列表", &session, None);
    }
}
