//! Todo Tool 的会话/owner 作用域与可见编号解析。
//!
//! `TodoToolScope` 封装私聊鉴权、session 加载与保存、最近列表快照与最近对象
//! 引用解析。prepare 与 execute 都通过它统一与 session 交互，避免各 Tool 自行
//! 手抄 owner 构造和快照校验。

use serde_json::Value;

use qq_maid_llm::tool::{ToolContext, ToolOutput};

use crate::{
    error::LlmError,
    runtime::{
        session::{SessionMeta, SessionStore, valid_last_visible_todo_query},
        todo::{TodoItem, TodoOwner, TodoStore},
    },
};

use super::common::{
    TODO_DEDUP_HISTORY_KEY, TODO_DEDUP_HISTORY_LIMIT, TODO_REFERENCE_UNAVAILABLE_CODE,
    TODO_VISIBLE_NUMBERS_UNAVAILABLE_CODE, TodoReference, TodoSelectionLabel, TodoSelectionRequest,
    TodoToolDedupEntry, session_tool_error, todo_tool_error, todo_tool_error_output,
};

/// 一次工具调用的 session + owner 作用域。
///
/// 持有 `SessionStore` 的克隆以支持内部 `save()`；session 在 Tool 调用期间可被
/// 修改（pending、last_todo_query、last_todo_action、extra dedup history）。
pub(in crate::runtime::tools::todo) struct TodoToolScope {
    pub owner: TodoOwner,
    pub session: crate::runtime::session::SessionRecord,
    pub session_store: SessionStore,
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
    /// 从 ToolContext 加载私聊 session 与 owner。
    ///
    /// 这里只接受 `private:` 作用域，避免群聊里误开 Todo Tool 把共享 session
    /// 写满 pending。鉴权失败抛 Err，让 Tool Loop 直接报错而非降级。
    pub(in crate::runtime::tools::todo) fn load(
        session_store: &SessionStore,
        context: &ToolContext,
    ) -> Result<Self, LlmError> {
        let user_id = context
            .user_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                LlmError::new(
                    "permission_denied",
                    "todo tools require authenticated private user",
                    "tool",
                )
            })?;
        if !context.scope_id.starts_with("private:") {
            return Err(LlmError::new(
                "permission_denied",
                "todo tools are only available in private chat scope",
                "tool",
            ));
        }
        let meta = SessionMeta::new(
            context.scope_id.clone(),
            Some(user_id.to_owned()),
            None,
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
        })
    }

    /// 记录最近展示给用户的列表快照，供后续编号续指。
    pub(in crate::runtime::tools::todo) fn remember(
        &mut self,
        query_type: &str,
        condition: &str,
        items: &[TodoItem],
    ) {
        self.session.remember_last_todo_query(
            &self.owner.key,
            query_type,
            condition,
            items.iter().map(|item| item.id.clone()).collect(),
        );
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
                self.resolve_numbers(numbers)?,
            )),
            TodoSelectionRequest::Reference(TodoReference::Last) => {
                self.resolve_last_reference(todo_store)
            }
        }
    }

    fn resolve_numbers(&mut self, numbers: &[usize]) -> Result<ResolvedTodoSelection, LlmError> {
        let query = valid_last_visible_todo_query(&mut self.session, &self.owner.key);
        let Some(query) = query else {
            return Ok(ResolvedTodoSelection::error(
                TODO_VISIBLE_NUMBERS_UNAVAILABLE_CODE,
                "visible numbers are unavailable; call list_todos first in this private chat",
            ));
        };
        let mut matched = Vec::new();
        let mut missing = Vec::new();
        let mut labels = Vec::new();
        for number in numbers {
            let label = TodoSelectionLabel::Number(*number);
            labels.push(label.clone());
            if let Some(id) = query
                .result_ids
                .get(number.saturating_sub(1))
                .filter(|_| *number > 0)
            {
                matched.push((label, id.clone()));
            } else {
                missing.push(label);
            }
        }
        Ok(ResolvedTodoSelection {
            labels,
            matched,
            missing,
            error_output: None,
        })
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
    pub(in crate::runtime::tools::todo) fn ensure_no_pending(&self) -> Result<(), LlmError> {
        if self.session.pending_operation.is_some() {
            return Err(LlmError::new(
                "pending_operation_exists",
                "current session already has a pending operation; ask the user to confirm or cancel it before creating another pending todo operation",
                "tool",
            ));
        }
        Ok(())
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
