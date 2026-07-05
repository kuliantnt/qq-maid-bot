//! Todo Tool 的会话/owner 作用域与可见编号解析。
//!
//! `TodoToolScope` 封装用户鉴权、session 加载与保存、最近列表快照与最近对象
//! 引用解析。prepare 与 execute 都通过它统一与 session 交互，避免各 Tool 自行
//! 手抄 owner 构造和快照校验。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use std::sync::Arc;

use qq_maid_llm::tool::{ToolContext, ToolOutput};

use crate::{
    error::LlmError,
    identity::{group_raw_target_from_scope_key, scope_target_type},
    runtime::{
        pending::{PendingOperation, PendingTodoClarification},
        session::{
            LAST_QUERY_TTL_SECONDS, LastTodoQuery, SessionMeta, SessionStore, now_iso_cn,
            query_is_fresh, valid_last_visible_todo_query,
        },
        todo::{TodoItem, TodoOwner, TodoStatus, TodoStore},
    },
};

use super::common::{
    PREBOUND_EDIT_DRAFT_KEY, PREBOUND_ERROR_OUTPUT_KEY, PREBOUND_SELECTION_KEY,
    PREBOUND_SINGLE_ID_KEY, PREBOUND_SINGLE_LABEL_KEY, TODO_DEDUP_HISTORY_KEY,
    TODO_DEDUP_HISTORY_LIMIT, TODO_REFERENCE_UNAVAILABLE_CODE, TODO_SELECTION_NOT_FOUND_CODE,
    TODO_VISIBLE_NUMBERS_UNAVAILABLE_CODE, TodoReference, TodoSelectionLabel, TodoSelectionRequest,
    TodoToolDedupEntry, session_tool_error, todo_tool_error, todo_tool_error_output,
};

/// 受限 Tool Loop / 引用消息专属的请求级选择作用域。
///
/// 该作用域是运行时内存态覆盖，由澄清恢复或引用消息路径在构造 TodoTool 实例时注入，
/// **不写入 Session / 数据库 / 全局共享状态**。`resolve_numbers` 优先用它把模型
/// 给的 `numbers=[n]` 映射到本次可见快照；引用快照存在但校验失败时会注入
/// `Blocked`，明确禁止 fallback 到 `last_todo_query`，避免跨 owner/account 误操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectionScope {
    Scoped(Arc<[String]>),
    Blocked,
}

const TODO_TASK_QUERY_HISTORY_KEY: &str = "tool_todo_task_query_history";
const TODO_TASK_QUERY_HISTORY_LIMIT: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TodoTaskQueryEntry {
    task_id: String,
    owner_key: String,
    query_type: String,
    condition: String,
    result_ids: Vec<String>,
    created_at: String,
}

impl TodoTaskQueryEntry {
    fn to_last_query(&self) -> LastTodoQuery {
        LastTodoQuery {
            owner_key: self.owner_key.clone(),
            query_type: self.query_type.clone(),
            condition: self.condition.clone(),
            result_ids: self.result_ids.clone(),
            created_at: self.created_at.clone(),
        }
    }
}

/// 一次工具调用的 session + owner 作用域。
///
/// 持有 `SessionStore` 的克隆以支持内部 `save()`；session 在 Tool 调用期间可被
/// 修改（pending、last_todo_query、last_todo_action、extra dedup history）。
pub(in crate::runtime::tools::todo) struct TodoToolScope {
    pub owner: TodoOwner,
    pub session: crate::runtime::session::SessionRecord,
    pub session_store: SessionStore,
    /// 当前 Tool Loop 的任务 ID；内部 list_todos 结果只在同一 task 内复用。
    task_id: String,
    /// 本次调用可选的请求级选择作用域覆盖；`None` 走默认 `last_todo_query` 解析。
    selection_scope: Option<SelectionScope>,
}

/// 可见编号 / 最近对象引用解析后出现错误时，用一条结构化输出替代抛 Err。
#[derive(Debug, Clone)]
pub(in crate::runtime::tools::todo) enum TodoToolSelectionResolution {
    Resolved(ResolvedTodoSelection),
    Output(ToolOutput),
}

/// 单条 item 解析结果；装箱 `TodoItem` 避免 enum 被大体量变体撑大。
#[derive(Debug, Clone)]
pub(in crate::runtime::tools::todo) enum TodoToolSingleItemResolution {
    Item(Box<TodoItem>),
    Output(ToolOutput),
}

/// 编号/引用解析的成功结果。
#[derive(Debug, Clone)]
pub(in crate::runtime::tools::todo) struct ResolvedTodoSelection {
    pub labels: Vec<TodoSelectionLabel>,
    pub matched: Vec<(TodoSelectionLabel, String)>,
    pub missing: Vec<TodoSelectionLabel>,
    pub error_output: Option<ToolOutput>,
}

impl ResolvedTodoSelection {
    pub(in crate::runtime::tools::todo) fn single_reference(
        reference: TodoReference,
        item_id: String,
    ) -> Self {
        let label = TodoSelectionLabel::Reference(reference);
        Self {
            labels: vec![label.clone()],
            matched: vec![(label, item_id)],
            missing: Vec::new(),
            error_output: None,
        }
    }

    pub(in crate::runtime::tools::todo) fn error(error_code: &str, message: &str) -> Self {
        Self {
            labels: Vec::new(),
            matched: Vec::new(),
            missing: Vec::new(),
            error_output: Some(todo_tool_error_output(error_code, message)),
        }
    }

    pub(in crate::runtime::tools::todo) fn single_label(&self) -> TodoSelectionLabel {
        self.labels
            .first()
            .cloned()
            .unwrap_or(TodoSelectionLabel::Reference(TodoReference::Last))
    }

    /// 取单条 item；错误统一落成结构化输出，避免把语义错误升级成重试 Err。
    pub(in crate::runtime::tools::todo) fn single_item(
        &self,
        todo_store: &TodoStore,
        owner: &TodoOwner,
    ) -> Result<TodoToolSingleItemResolution, LlmError> {
        use super::common::TODO_SELECTION_NOT_FOUND_CODE;

        if let Some(output) = self.error_output.clone() {
            return Ok(TodoToolSingleItemResolution::Output(output));
        }
        let Some((label, id)) = self.matched.first() else {
            let error_code = match self.missing.first() {
                Some(TodoSelectionLabel::Reference(TodoReference::Last)) => {
                    TODO_REFERENCE_UNAVAILABLE_CODE
                }
                _ => TODO_SELECTION_NOT_FOUND_CODE,
            };
            return Ok(TodoToolSingleItemResolution::Output(
                todo_tool_error_output(error_code, "selected todo is unavailable"),
            ));
        };
        let item = todo_store.get_by_id(owner, id).map_err(todo_tool_error)?;
        let Some(item) = item else {
            let output = match label {
                TodoSelectionLabel::Reference(TodoReference::Last) => todo_tool_error_output(
                    TODO_REFERENCE_UNAVAILABLE_CODE,
                    "selected todo no longer exists",
                ),
                TodoSelectionLabel::Number(_) => todo_tool_error_output(
                    TODO_SELECTION_NOT_FOUND_CODE,
                    "visible number not found",
                ),
            };
            return Ok(TodoToolSingleItemResolution::Output(output));
        };
        Ok(TodoToolSingleItemResolution::Item(Box::new(item)))
    }
}

impl TodoToolScope {
    /// 从 ToolContext 加载当前会话 session 与个人 owner。
    ///
    /// private 和 group Tool Loop 都必须绑定真实 user_id；owner 由当前业务 scope
    /// 与 actor 一起推导，因此群聊中执行 Todo Tool 仍写入个人待办，不写入群共享待办。
    /// session 则继续使用当前请求 scope，保留用户在该聊天上下文中看到的编号快照。
    /// 未验证的 scope 类型继续拒绝，避免把频道等场景误纳入 Todo 写入口。
    ///
    /// `selection_scope` 为受限 Tool Loop 注入的请求级选择作用域；普通调用传 `None`，
    /// 编号解析走会话默认 `last_todo_query` 快照。
    pub(in crate::runtime::tools::todo) fn load(
        session_store: &SessionStore,
        context: &ToolContext,
        selection_scope: Option<SelectionScope>,
    ) -> Result<Self, LlmError> {
        let user_id = context
            .user_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                LlmError::new(
                    "permission_denied",
                    "todo tools require authenticated user",
                    "tool",
                )
            })?;
        let group_id = todo_tool_group_id(&context.scope_id).ok_or_else(|| {
            LlmError::new(
                "permission_denied",
                "todo tools are only available in private or group chat scope",
                "tool",
            )
        })?;
        let meta = SessionMeta::new(
            context.scope_id.clone(),
            Some(user_id.to_owned()),
            group_id,
            None,
            None,
            "qq_official",
        );
        let session = session_store
            .get_or_create_active(&meta)
            .map_err(session_tool_error)?;
        let owner = TodoStore::owner(Some(user_id), &context.scope_id);
        Ok(Self {
            owner,
            session,
            session_store: session_store.clone(),
            task_id: context.task_id.clone(),
            selection_scope,
        })
    }

    /// 记录当前 Tool Loop 内部查询快照，供同一轮后续工具按编号绑定。
    ///
    /// 该快照写入 `session.extra` 且以 `task_id` 隔离，不覆盖跨轮次的
    /// `last_todo_query`。因此模型内部为了推理调用 `list_todos` 时，不会改变
    /// 用户真正看见的“第 N 条”含义。
    pub(in crate::runtime::tools::todo) fn remember_internal_query(
        &mut self,
        query_type: &str,
        condition: &str,
        items: &[TodoItem],
    ) -> Result<(), LlmError> {
        let query = LastTodoQuery {
            owner_key: self.owner.key.clone(),
            query_type: query_type.to_owned(),
            condition: condition.to_owned(),
            result_ids: items.iter().map(|item| item.id.clone()).collect(),
            created_at: now_iso_cn(),
        };
        self.remember_task_query(&query)
    }

    /// 按编号或最近对象引用解析；编号路径绝不偷偷降级为 reference，
    /// 否则状态变化后会误操作。
    pub(in crate::runtime::tools::todo) fn resolve_selection(
        &mut self,
        selection: &TodoSelectionRequest,
        todo_store: &TodoStore,
    ) -> Result<TodoToolSelectionResolution, LlmError> {
        match selection {
            TodoSelectionRequest::Numbers(numbers) => Ok(TodoToolSelectionResolution::Resolved(
                self.resolve_numbers(numbers, todo_store)?,
            )),
            TodoSelectionRequest::Reference(TodoReference::Last) => {
                self.resolve_last_reference(todo_store)
            }
        }
    }

    fn resolve_numbers(
        &mut self,
        numbers: &[usize],
        todo_store: &TodoStore,
    ) -> Result<ResolvedTodoSelection, LlmError> {
        // 优先级：quoted visible snapshot / pending clarification candidate scope
        // > Tool Loop 本轮临时 list_todos scope > session.last_todo_query > last_todo_action。
        // 引用快照存在但 scope/owner/account 校验失败时，必须显式阻断 fallback。
        if matches!(self.selection_scope, Some(SelectionScope::Blocked)) {
            return Ok(ResolvedTodoSelection::error(
                TODO_VISIBLE_NUMBERS_UNAVAILABLE_CODE,
                "quoted visible snapshot is unavailable for this request",
            ));
        }
        if let Some(SelectionScope::Scoped(scoped_ids)) = self.selection_scope.as_ref() {
            let mut matched = Vec::new();
            let mut missing = Vec::new();
            let mut labels = Vec::new();
            for number in numbers {
                let label = TodoSelectionLabel::Number(*number);
                labels.push(label.clone());
                if let Some(id) = scoped_ids
                    .get(number.saturating_sub(1))
                    .filter(|_| *number > 0)
                    .cloned()
                {
                    matched.push((label, id));
                } else {
                    missing.push(label);
                }
            }
            return Ok(ResolvedTodoSelection {
                labels,
                matched,
                missing,
                error_output: None,
            });
        }

        // 同一 Tool Loop 内刚由 `list_todos` 产生的编号优先级高于旧用户可见快照。
        // 否则模型在同轮工具链里根据当前查询结果传入 numbers 时，可能被上一轮
        // `last_todo_query` 静默映射到旧列表，造成完成/恢复/删除错待办。
        let (query, validate_current_status) = if let Some(query) = self.valid_task_todo_query()? {
            (Some(query), true)
        } else {
            let visible_query = valid_last_visible_todo_query(&mut self.session, &self.owner.key);
            if let Some(query) = visible_query.as_ref() {
                // 同一句用户请求可能被模型拆成多个工具轮次。第一次按用户可见列表
                // 解析编号后，把该列表固定到当前 task，避免前一个写操作清空
                // last_todo_query 后，后续“第 4 条”失去编号上下文。
                self.remember_task_query(query)?;
            }
            // 用户跨轮实际看见的快照可能因外部状态变化失效；这里仍保留编号绑定，
            // 由具体 Tool 返回“状态不允许”或 missing_numbers 等更精确业务结果。
            (visible_query, false)
        };
        let Some(query) = query else {
            return Ok(ResolvedTodoSelection::error(
                TODO_VISIBLE_NUMBERS_UNAVAILABLE_CODE,
                "visible numbers are unavailable; call list_todos first in this chat",
            ));
        };
        let mut matched = Vec::new();
        let mut missing = Vec::new();
        let mut labels = Vec::new();
        let mut stale = false;
        for number in numbers {
            let label = TodoSelectionLabel::Number(*number);
            labels.push(label.clone());
            if let Some(id) = query
                .result_ids
                .get(number.saturating_sub(1))
                .filter(|_| *number > 0)
            {
                if !validate_current_status
                    || self.query_item_still_matches(todo_store, &query.query_type, id)?
                {
                    matched.push((label, id.clone()));
                } else {
                    // 该编号来自旧列表，但条目状态已被同轮或外部写操作改变。
                    // 直接要求刷新列表，避免把“已完成列表第 1 条”恢复后又按第 1 条取消。
                    stale = true;
                    missing.push(label);
                }
            } else {
                missing.push(label);
            }
        }
        let error_output = if stale {
            Some(todo_tool_error_output(
                TODO_VISIBLE_NUMBERS_UNAVAILABLE_CODE,
                "visible numbers are stale; call list_todos again before using numbers",
            ))
        } else {
            None
        };
        Ok(ResolvedTodoSelection {
            labels,
            matched: if error_output.is_some() {
                Vec::new()
            } else {
                matched
            },
            missing,
            error_output,
        })
    }

    fn query_item_still_matches(
        &self,
        todo_store: &TodoStore,
        query_type: &str,
        id: &str,
    ) -> Result<bool, LlmError> {
        let Some(item) = todo_store
            .get_by_id(&self.owner, id)
            .map_err(todo_tool_error)?
        else {
            return Ok(false);
        };
        Ok(match expected_status_for_query_type(query_type) {
            Some(status) => item.status == status,
            None => true,
        })
    }

    fn task_query_entries(&self) -> Result<Vec<TodoTaskQueryEntry>, LlmError> {
        self.session
            .extra
            .get(TODO_TASK_QUERY_HISTORY_KEY)
            .cloned()
            .map(serde_json::from_value::<Vec<TodoTaskQueryEntry>>)
            .transpose()
            .map_err(|err| {
                LlmError::new(
                    "session_decode_error",
                    format!("failed to decode todo task query history: {err}"),
                    "todo_tool",
                )
            })
            .map(|entries| entries.unwrap_or_default())
    }

    fn remember_task_query(&mut self, query: &LastTodoQuery) -> Result<(), LlmError> {
        let mut entries = self.task_query_entries()?;
        entries.retain(|entry| entry.task_id != self.task_id);
        entries.push(TodoTaskQueryEntry {
            task_id: self.task_id.clone(),
            owner_key: query.owner_key.clone(),
            query_type: query.query_type.clone(),
            condition: query.condition.clone(),
            result_ids: query.result_ids.clone(),
            created_at: query.created_at.clone(),
        });
        if entries.len() > TODO_TASK_QUERY_HISTORY_LIMIT {
            let keep_from = entries.len() - TODO_TASK_QUERY_HISTORY_LIMIT;
            entries.drain(..keep_from);
        }
        self.session.extra.insert(
            TODO_TASK_QUERY_HISTORY_KEY.to_owned(),
            serde_json::to_value(entries).map_err(|err| {
                LlmError::new(
                    "session_encode_error",
                    format!("failed to encode todo task query history: {err}"),
                    "todo_tool",
                )
            })?,
        );
        self.save()
    }

    fn valid_task_todo_query(&self) -> Result<Option<LastTodoQuery>, LlmError> {
        let entries = self.task_query_entries()?;
        Ok(entries.into_iter().rev().find_map(|entry| {
            if entry.task_id == self.task_id
                && entry.owner_key == self.owner.key
                && query_is_fresh(&entry.created_at, LAST_QUERY_TTL_SECONDS)
            {
                Some(entry.to_last_query())
            } else {
                None
            }
        }))
    }

    fn resolve_last_reference(
        &self,
        todo_store: &TodoStore,
    ) -> Result<TodoToolSelectionResolution, LlmError> {
        let Some(last_action) = self
            .session
            .last_todo_action
            .clone()
            .filter(|action| action.owner_key == self.owner.key)
        else {
            return Ok(TodoToolSelectionResolution::Output(todo_tool_error_output(
                TODO_REFERENCE_UNAVAILABLE_CODE,
                "last todo reference is unavailable",
            )));
        };
        let Some(item) = todo_store
            .get_by_id(&self.owner, &last_action.item_id)
            .map_err(todo_tool_error)?
        else {
            return Ok(TodoToolSelectionResolution::Output(todo_tool_error_output(
                TODO_REFERENCE_UNAVAILABLE_CODE,
                "last referenced todo no longer exists",
            )));
        };
        Ok(TodoToolSelectionResolution::Resolved(
            ResolvedTodoSelection::single_reference(TodoReference::Last, item.id),
        ))
    }

    pub(in crate::runtime::tools::todo) fn save(&mut self) -> Result<(), LlmError> {
        self.session_store
            .save(&mut self.session)
            .map_err(session_tool_error)
    }

    /// 受限澄清恢复 Loop 中，原工具已经真实完成副作用时清除原 `TodoClarify`。
    ///
    /// 只在请求级选择作用域存在且当前 pending 仍是同 owner 的 `TodoClarify` 时生效；
    /// 如果工具已替换成确认 pending（如 `TodoBulkDelete`），这里不会覆盖它。
    pub(in crate::runtime::tools::todo) fn clear_clarification_if_scoped(&mut self) {
        if self.selection_scope.is_none() {
            return;
        }
        if matches!(
            self.session.pending_operation.as_ref(),
            Some(PendingOperation::TodoClarify { owner_key, .. }) if owner_key == &self.owner.key
        ) {
            self.session.pending_operation = None;
        }
    }

    /// 同一 call_id + 相同参数二次执行时直接复用上一次输出，避免重复 pending。
    pub(in crate::runtime::tools::todo) fn take_dedup_output(
        &self,
        context: &ToolContext,
        arguments: &Value,
    ) -> Result<Option<ToolOutput>, LlmError> {
        let Some(call_id) = dedup_call_key(context) else {
            return Ok(None);
        };
        let Some(entries_value) = self.session.extra.get(TODO_DEDUP_HISTORY_KEY) else {
            return Ok(None);
        };
        let entries = serde_json::from_value::<Vec<TodoToolDedupEntry>>(entries_value.clone())
            .map_err(|err| {
                LlmError::new(
                    "session_decode_error",
                    format!("failed to decode todo dedup history: {err}"),
                    "todo_tool",
                )
            })?;
        let Some(entry) = entries.into_iter().find(|entry| entry.call_id == call_id) else {
            return Ok(None);
        };
        if entry.arguments == *arguments {
            return Ok(Some(ToolOutput::json(entry.output)));
        }
        Ok(None)
    }

    pub(in crate::runtime::tools::todo) fn remember_dedup_output(
        &mut self,
        context: &ToolContext,
        arguments: &Value,
        output: &ToolOutput,
    ) -> Result<(), LlmError> {
        let Some(call_id) = dedup_call_key(context) else {
            return Ok(());
        };
        let mut entries = self
            .session
            .extra
            .get(TODO_DEDUP_HISTORY_KEY)
            .cloned()
            .map(serde_json::from_value::<Vec<TodoToolDedupEntry>>)
            .transpose()
            .map_err(|err| {
                LlmError::new(
                    "session_decode_error",
                    format!("failed to decode todo dedup history: {err}"),
                    "todo_tool",
                )
            })?
            .unwrap_or_default();
        entries.retain(|entry| entry.call_id != call_id);
        entries.push(TodoToolDedupEntry {
            call_id,
            arguments: arguments.clone(),
            output: output.value.clone(),
        });
        if entries.len() > TODO_DEDUP_HISTORY_LIMIT {
            let keep_from = entries.len() - TODO_DEDUP_HISTORY_LIMIT;
            entries.drain(..keep_from);
        }
        self.session.extra.insert(
            TODO_DEDUP_HISTORY_KEY.to_owned(),
            serde_json::to_value(entries).map_err(|err| {
                LlmError::new(
                    "session_encode_error",
                    format!("failed to encode todo dedup history: {err}"),
                    "todo_tool",
                )
            })?,
        );
        self.save()?;
        Ok(())
    }

    /// 当前对话已有 pending 时拒绝覆盖，避免模型连续写工具静默丢失前一个确认。
    ///
    /// 受限澄清恢复 Loop 会把原 `TodoClarify` 保留在 Session 中运行原工具；当工具
    /// 成功推进到 `TodoDelete` / `TodoBulkDelete` 等确认 pending 时，需要允许同 owner
    /// 的 `TodoClarify` 被原工具原子替换。普通 Tool Loop 没有 `selection_scope`，仍保持
    /// 严格拒绝，避免多个工具互相覆盖 pending。
    pub(in crate::runtime::tools::todo) fn ensure_no_pending(&self) -> Result<(), LlmError> {
        if self.selection_scope.is_some()
            && matches!(
                self.session.pending_operation.as_ref(),
                Some(PendingOperation::TodoClarify { owner_key, .. }) if owner_key == &self.owner.key
            )
        {
            return Ok(());
        }
        if self.session.pending_operation.is_some() {
            return Err(LlmError::new(
                "pending_operation_exists",
                "current session already has a pending operation; ask the user to confirm or cancel it before creating another pending todo operation",
                "tool",
            ));
        }
        Ok(())
    }

    /// 将选择失败保存为可恢复的澄清 pending。
    ///
    /// 这里会剥离 prepare 阶段写入的内部预绑定字段，pending 中只保留原工具名、
    /// 原始参数、澄清原因和本次候选集；用户补充后必须重新读取 TodoStore 校验当前目标。
    /// 候选集按 `tool_name` 可接受的状态从 `todo_store` 查出，并在 pending 中保存为
    /// 精简结构（不持久化完整 `TodoItem`），恢复时用作请求级选择作用域。
    pub(in crate::runtime::tools::todo) fn save_clarification(
        &mut self,
        todo_store: &TodoStore,
        tool_name: &str,
        arguments: &Value,
        allow_many: bool,
        error_code: &str,
        message: &str,
    ) -> Result<ToolOutput, LlmError> {
        use super::common::{
            CANCEL_TODO_TOOL_NAME, COMPLETE_TODOS_TOOL_NAME, DELETE_TODOS_TOOL_NAME,
            EDIT_TODO_TOOL_NAME, RESTORE_TODOS_TOOL_NAME,
        };

        if self.selection_scope.is_some() {
            let question = clarification_question(error_code, message, &[]);
            return Ok(ToolOutput::json(serde_json::json!({
                "ok": false,
                "requires_clarification": true,
                "pending_action": "clarify",
                "error_code": error_code,
                "message": message,
                "question": question,
            })));
        }

        self.ensure_no_pending()?;
        let candidates = match tool_name {
            COMPLETE_TODOS_TOOL_NAME | EDIT_TODO_TOOL_NAME | CANCEL_TODO_TOOL_NAME => {
                clarification_candidates_from(todo_store, &self.owner, false)
                    .map_err(todo_tool_error)?
            }
            RESTORE_TODOS_TOOL_NAME => clarification_candidates_from(todo_store, &self.owner, true)
                .map_err(todo_tool_error)?,
            DELETE_TODOS_TOOL_NAME => {
                clarification_candidates_all_statuses_from(todo_store, &self.owner)
                    .map_err(todo_tool_error)?
            }
            _ => Vec::new(),
        };
        self.save_clarification_with_candidates(
            tool_name, arguments, allow_many, error_code, message, candidates,
        )
    }

    /// 使用调用方提供的候选集保存澄清 pending。
    ///
    /// `delete_todos` 按标题匹配出多个终态候选时会走这里，避免退回“全部终态
    /// 候选”导致用户需要在无关项目中再筛选；候选编号仍只在澄清 pending 内有效。
    pub(in crate::runtime::tools::todo) fn save_clarification_with_candidates(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        allow_many: bool,
        error_code: &str,
        message: &str,
        candidates: Vec<crate::runtime::pending::ClarificationCandidate>,
    ) -> Result<ToolOutput, LlmError> {
        if self.selection_scope.is_some() {
            let question = clarification_question(error_code, message, &[]);
            return Ok(ToolOutput::json(serde_json::json!({
                "ok": false,
                "requires_clarification": true,
                "pending_action": "clarify",
                "error_code": error_code,
                "message": message,
                "question": question,
            })));
        }

        self.ensure_no_pending()?;
        let question = clarification_question(error_code, message, &candidates);
        let created_at = now_iso_cn();
        self.session.pending_operation = Some(PendingOperation::TodoClarify {
            initiator_user_id: self.owner.user_id.clone(),
            owner_key: self.owner.key.clone(),
            request: PendingTodoClarification {
                tool_name: tool_name.to_owned(),
                arguments: sanitize_clarification_arguments(arguments),
                allow_many,
                error_code: error_code.to_owned(),
                question: question.clone(),
                candidates,
                created_at: created_at.clone(),
            },
            created_at,
        });
        self.save()?;
        Ok(ToolOutput::json(serde_json::json!({
            "ok": false,
            "requires_clarification": true,
            "pending_action": "clarify",
            "error_code": error_code,
            "message": message,
            "question": question,
        })))
    }
}

fn todo_tool_group_id(scope_id: &str) -> Option<Option<String>> {
    // Todo Tool 只接受私聊或群聊业务 scope；stable scope 下仍要取出原始群 ID
    // 写入 SessionMeta，不能把 namespaced scope 当成平台投递目标。
    match scope_target_type(scope_id) {
        Some("private") => Some(None),
        Some("group") => group_raw_target_from_scope_key(scope_id).map(Some),
        _ => None,
    }
}

pub(in crate::runtime::tools::todo) fn dedup_call_key(context: &ToolContext) -> Option<String> {
    let tool_call_id = context
        .tool_call_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(format!("{}:{tool_call_id}", context.task_id))
}

pub(in crate::runtime::tools::todo) fn clarification_error_fields(
    output: &ToolOutput,
) -> (&str, &str) {
    let error_code = output
        .value
        .get("error_code")
        .and_then(Value::as_str)
        .unwrap_or(TODO_SELECTION_NOT_FOUND_CODE);
    let message = output
        .value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("todo target is unavailable");
    (error_code, message)
}

fn clarification_question(
    error_code: &str,
    message: &str,
    candidates: &[crate::runtime::pending::ClarificationCandidate],
) -> String {
    use crate::runtime::pending::ClarificationCandidate;
    let candidate_lines = |items: &[ClarificationCandidate]| -> String {
        if items.is_empty() {
            return String::new();
        }
        let body = items
            .iter()
            .map(|item| format!("{}. {}", item.display_number, item.title))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n可选待办：\n{body}")
    };
    let list = candidate_lines(candidates);
    match error_code {
        TODO_VISIBLE_NUMBERS_UNAVAILABLE_CODE => format!(
            "我现在没有可用的待办列表编号。请告诉我要操作哪条待办的标题关键词，或回复编号选择这次列出的候选。{list}"
        ),
        TODO_REFERENCE_UNAVAILABLE_CODE => format!(
            "我现在不能唯一确定“刚才那个/它”指哪条待办。请补充这条待办的标题关键词，或回复编号选择下面列出的候选。{list}"
        ),
        TODO_SELECTION_NOT_FOUND_CODE => format!(
            "这次选择的待办已经不可用或编号不存在。请补充这条待办的标题关键词，或回复编号选择下面列出的候选。{list}"
        ),
        _ => format!("请补充要操作哪条待办。{message}{list}"),
    }
}

/// 按 `terminal_only` 选择候选集：完成/编辑/取消看未完成，恢复看已完成+已取消。
/// 候选数量与单项标题长度有上限，避免持久化过大的 pending。
fn expected_status_for_query_type(query_type: &str) -> Option<TodoStatus> {
    match query_type {
        "list" | "search" | "due-date" => Some(TodoStatus::Pending),
        "completed-list" | "completed-time" => Some(TodoStatus::Completed),
        "cancelled-list" => Some(TodoStatus::Cancelled),
        // `all` 看板同时包含三类状态，只校验条目仍存在。
        _ => None,
    }
}

fn clarification_candidates_from(
    todo_store: &TodoStore,
    owner: &crate::runtime::todo::TodoOwner,
    terminal_only: bool,
) -> Result<Vec<crate::runtime::pending::ClarificationCandidate>, crate::runtime::todo::TodoError> {
    let mut items = if terminal_only {
        let mut items = todo_store.list_completed(owner)?;
        items.extend(todo_store.list_cancelled(owner)?);
        items
    } else {
        todo_store.list_pending(owner)?
    };
    const MAX_CANDIDATES: usize = 20;
    items.truncate(MAX_CANDIDATES);
    Ok(clarification_candidates_for_items(&items))
}

/// 删除工具可永久删除任意状态，澄清候选也必须覆盖进行中、已完成和已取消。
fn clarification_candidates_all_statuses_from(
    todo_store: &TodoStore,
    owner: &crate::runtime::todo::TodoOwner,
) -> Result<Vec<crate::runtime::pending::ClarificationCandidate>, crate::runtime::todo::TodoError> {
    let mut items = todo_store.list_all_for_board(owner)?;
    const MAX_CANDIDATES: usize = 20;
    items.truncate(MAX_CANDIDATES);
    Ok(clarification_candidates_for_items(&items))
}

pub(in crate::runtime::tools::todo) fn clarification_candidates_for_items(
    items: &[TodoItem],
) -> Vec<crate::runtime::pending::ClarificationCandidate> {
    use crate::runtime::pending::ClarificationCandidate;
    const MAX_CANDIDATES: usize = 20;
    const MAX_TITLE_CHARS: usize = 80;
    items
        .iter()
        .take(MAX_CANDIDATES)
        .enumerate()
        .map(|(index, item)| ClarificationCandidate {
            id: item.id.clone(),
            display_number: index + 1,
            title: item.title.chars().take(MAX_TITLE_CHARS).collect::<String>(),
            status: item.status.clone(),
        })
        .collect()
}

fn sanitize_clarification_arguments(arguments: &Value) -> Value {
    let Value::Object(object) = arguments else {
        return arguments.clone();
    };
    let mut sanitized = serde_json::Map::new();
    for (key, value) in object {
        if matches!(
            key.as_str(),
            PREBOUND_SELECTION_KEY
                | PREBOUND_SINGLE_ID_KEY
                | PREBOUND_SINGLE_LABEL_KEY
                | PREBOUND_EDIT_DRAFT_KEY
                | PREBOUND_ERROR_OUTPUT_KEY
        ) || key.starts_with('_')
        {
            continue;
        }
        sanitized.insert(key.clone(), value.clone());
    }
    Value::Object(sanitized)
}
