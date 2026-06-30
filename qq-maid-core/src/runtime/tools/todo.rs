//! Todo Tool。
//!
//! 这些 Tool 只把模型参数适配到现有 TodoStore、Session 快照和 pending 机制。
//! 内部 ID 不返回给模型；模型只能使用用户最近看到的列表编号继续操作。

use std::collections::HashSet;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::{
        pending::PendingOperation,
        session::{LastTodoQuery, SessionMeta, SessionStore, now_iso_cn},
        todo::{
            TodoItem, TodoItemDraft, TodoOwner, TodoStatus, TodoStore, TodoTimePrecision,
            display_draft_time, display_todo_time, enrich_draft_time_from_text,
        },
    },
    util::time_context::request_time_context,
};

const LIST_TODOS_TOOL_NAME: &str = "list_todos";
const CREATE_TODO_TOOL_NAME: &str = "create_todo";
const COMPLETE_TODOS_TOOL_NAME: &str = "complete_todos";
const CANCEL_TODO_TOOL_NAME: &str = "cancel_todo";
const RESTORE_TODOS_TOOL_NAME: &str = "restore_todos";
const DELETE_TODOS_TOOL_NAME: &str = "delete_todos";
const TODO_TOOL_MAX_NUMBERS: usize = 20;
const TODO_TOOL_MAX_TEXT_CHARS: usize = 500;
const TODO_REFERENCE_LAST: &str = "last";
const TODO_VISIBLE_NUMBERS_UNAVAILABLE_CODE: &str = "todo_visible_numbers_unavailable";
const TODO_REFERENCE_UNAVAILABLE_CODE: &str = "todo_reference_unavailable";
const TODO_REFERENCE_INVALID_STATE_CODE: &str = "todo_reference_invalid_state";
const TODO_SELECTION_NOT_FOUND_CODE: &str = "todo_selection_not_found";
const TODO_DELETE_INVALID_STATE_CODE: &str = "todo_delete_invalid_state";
const TODO_DELETE_MIXED_STATUS_CODE: &str = "todo_delete_mixed_status";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoReference {
    Last,
}

impl TodoReference {
    fn as_str(self) -> &'static str {
        match self {
            Self::Last => TODO_REFERENCE_LAST,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TodoSelectionRequest {
    Numbers(Vec<usize>),
    Reference(TodoReference),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TodoSelectionLabel {
    Number(usize),
    Reference(TodoReference),
}

/// 查询当前私聊用户的 Todo，并刷新用户可见编号快照。
#[derive(Clone)]
pub struct ListTodoTool {
    todo_store: TodoStore,
    session_store: SessionStore,
}

impl ListTodoTool {
    pub fn new(todo_store: TodoStore, session_store: SessionStore) -> Self {
        Self {
            todo_store,
            session_store,
        }
    }
}

/// 创建 Todo 草稿，并进入现有 pending 确认流程。
#[derive(Clone)]
pub struct CreateTodoTool {
    session_store: SessionStore,
}

impl CreateTodoTool {
    pub fn new(session_store: SessionStore) -> Self {
        Self { session_store }
    }
}

/// 按最近可见编号完成未完成 Todo。
#[derive(Clone)]
pub struct CompleteTodoTool {
    todo_store: TodoStore,
    session_store: SessionStore,
}

impl CompleteTodoTool {
    pub fn new(todo_store: TodoStore, session_store: SessionStore) -> Self {
        Self {
            todo_store,
            session_store,
        }
    }
}

/// 按最近可见编号发起取消 Todo，确认后只会标记为已取消。
#[derive(Clone)]
pub struct CancelTodoTool {
    todo_store: TodoStore,
    session_store: SessionStore,
}

impl CancelTodoTool {
    pub fn new(todo_store: TodoStore, session_store: SessionStore) -> Self {
        Self {
            todo_store,
            session_store,
        }
    }
}

/// 按最近可见编号恢复已完成或已取消 Todo 为未完成。
#[derive(Clone)]
pub struct RestoreTodoTool {
    todo_store: TodoStore,
    session_store: SessionStore,
}

impl RestoreTodoTool {
    pub fn new(todo_store: TodoStore, session_store: SessionStore) -> Self {
        Self {
            todo_store,
            session_store,
        }
    }
}

/// 按最近可见编号发起永久删除已完成或已取消 Todo。
#[derive(Clone)]
pub struct DeleteTodoTool {
    todo_store: TodoStore,
    session_store: SessionStore,
}

impl DeleteTodoTool {
    pub fn new(todo_store: TodoStore, session_store: SessionStore) -> Self {
        Self {
            todo_store,
            session_store,
        }
    }
}

#[async_trait]
impl Tool for ListTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: LIST_TODOS_TOOL_NAME.to_owned(),
            description: "查询当前私聊用户的待办列表，并刷新后续工具可使用的用户侧编号。不会返回数据库内部 ID。status=pending 查询未完成，completed 查询已完成，cancelled 查询已取消，all 查询全部。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["pending", "completed", "cancelled", "all"],
                        "description": "要查询的待办状态"
                    }
                },
                "required": ["status"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope = TodoToolScope::load(&self.session_store, &context)?;
        let status = todo_status_argument(&arguments, "status")?;
        let items = match status {
            TodoToolListStatus::Pending => self.todo_store.list_pending(&scope.owner),
            TodoToolListStatus::Completed => self.todo_store.list_completed(&scope.owner),
            TodoToolListStatus::Cancelled => self.todo_store.list_cancelled(&scope.owner),
            TodoToolListStatus::All => self.todo_store.list_all(&scope.owner),
        }
        .map_err(todo_tool_error)?;
        scope.remember(status.query_type(), status.condition(), &items);
        scope.save(&self.session_store)?;

        Ok(ToolOutput::json(json!({
            "status": status.as_str(),
            "items": todo_items_json(&items),
            "count": items.len(),
            "numbering": "visible_number 是用户可见编号，仅在当前会话最近一次 list_todos 结果中有效；未暴露数据库内部 ID。"
        })))
    }
}

#[async_trait]
impl Tool for CreateTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: CREATE_TODO_TOOL_NAME.to_owned(),
            description: "为当前私聊用户创建待办草稿。该工具只会生成待确认 pending，不会直接写入；用户确认后才保存。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "用户原始待办内容，例如“今晚检查机器人日志”"
                    },
                    "title": {
                        "type": ["string", "null"],
                        "description": "模型整理出的待办标题；不确定时传 null，系统使用 content"
                    },
                    "detail": {
                        "type": ["string", "null"],
                        "description": "补充详情；没有则传 null"
                    },
                    "due_date": {
                        "type": ["string", "null"],
                        "description": "YYYY-MM-DD 截止日期；没有则传 null"
                    },
                    "due_at": {
                        "type": ["string", "null"],
                        "description": "YYYY-MM-DD HH:MM:SS 或 RFC3339 截止时间；没有则传 null"
                    },
                    "time_precision": {
                        "type": ["string", "null"],
                        "enum": ["none", "date", "date_time", "inferred", null],
                        "description": "时间精度；不确定时传 null"
                    }
                },
                "required": ["content", "title", "detail", "due_date", "due_at", "time_precision"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope = TodoToolScope::load(&self.session_store, &context)?;
        let content = required_non_empty_text(&arguments, "content")?;
        let title = optional_text(&arguments, "title")?.unwrap_or_else(|| content.clone());
        let detail = optional_text(&arguments, "detail")?;
        let due_date = optional_text(&arguments, "due_date")?;
        let due_at = optional_text(&arguments, "due_at")?;
        let time_precision = optional_time_precision(&arguments, "time_precision")?;
        let mut draft = TodoItemDraft {
            title,
            detail,
            raw_text: Some(content.clone()),
            due_date,
            due_at,
            time_precision,
        };
        // Tool 创建仍复用本地时间推断；模型未传结构化时间时，保持 `/todo add` 的保守体验。
        enrich_draft_time_from_text(&mut draft, &content, &request_time_context());

        scope.ensure_no_pending()?;
        scope.session.last_todo_query = None;
        scope.session.pending_operation = Some(PendingOperation::TodoAdd {
            initiator_user_id: scope.owner.user_id.clone(),
            owner_key: scope.owner.key.clone(),
            draft: draft.clone(),
            allow_revision: true,
            created_at: now_iso_cn(),
        });
        scope.save(&self.session_store)?;

        Ok(ToolOutput::json(json!({
            "requires_confirmation": true,
            "pending_action": "create",
            "message": "已生成待确认待办草稿；必须等待用户确认后才会写入。",
            "draft": todo_draft_json(&draft),
        })))
    }
}

#[async_trait]
impl Tool for CompleteTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: COMPLETE_TODOS_TOOL_NAME.to_owned(),
            description: "将未完成待办标记为已完成。用户明确说“第 N 个”时只能传 numbers 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它 / 刚恢复的那个 / 刚完成的”时传 reference=\"last\"。不会接受数据库内部 ID。".to_owned(),
            parameters: number_list_or_reference_schema("要完成的 visible_number 列表"),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope = TodoToolScope::load(&self.session_store, &context)?;
        let selection = todo_selection_request(&arguments, true)?;
        let resolved = match scope.resolve_selection(&selection, &self.todo_store)? {
            TodoToolSelectionResolution::Resolved(resolved) => resolved,
            TodoToolSelectionResolution::Output(output) => return Ok(output),
        };
        let ids = resolved.ids();
        let outcome = self
            .todo_store
            .complete_by_ids(&scope.owner, &ids)
            .map_err(todo_tool_error)?;
        let completed = selected_items_for_result(&resolved, &outcome.completed);
        let missing = missing_selection_labels_for_result(&resolved, &outcome.skipped_ids);
        if !completed.is_empty() {
            // 状态变化后清空旧编号快照，避免模型继续沿用已变更的列表。
            scope.session.last_todo_query = None;
            scope.session.update_last_todo_action_from_items(
                &scope.owner.key,
                "completed",
                &outcome.completed,
            );
            scope.save(&self.session_store)?;
        }

        Ok(ToolOutput::json(json!({
            "ok": true,
            "completed": todo_selected_items_json(&completed),
            "missing_numbers": missing_numbers_json(&missing),
            "message": "已完成的条目已变更为 completed；missing_numbers 表示编号不存在、状态不是未完成或条目已变化。"
        })))
    }
}

#[async_trait]
impl Tool for CancelTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: CANCEL_TODO_TOOL_NAME.to_owned(),
            description: "发起取消未完成待办。用户明确说“第 N 个”时只能传 number 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它 / 恢复的那个”时传 reference=\"last\"。取消只是状态变更为已取消，不是永久删除；需要用户确认后才执行。".to_owned(),
            parameters: single_number_or_reference_schema("要取消的 visible_number"),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope = TodoToolScope::load(&self.session_store, &context)?;
        let selection = single_todo_selection_request(&arguments)?;
        let resolved = match scope.resolve_selection(&selection, &self.todo_store)? {
            TodoToolSelectionResolution::Resolved(resolved) => resolved,
            TodoToolSelectionResolution::Output(output) => return Ok(output),
        };
        let item = match resolved.single_item(&self.todo_store, &scope.owner)? {
            TodoToolSingleItemResolution::Item(item) => item,
            TodoToolSingleItemResolution::Output(output) => return Ok(output),
        };
        if item.status != TodoStatus::Pending {
            return Ok(todo_tool_error_output(
                TODO_REFERENCE_INVALID_STATE_CODE,
                "cancel_todo only accepts pending todos; use restore_todos or delete_todos for terminal states",
            ));
        }

        scope.ensure_no_pending()?;
        scope.session.pending_operation = Some(PendingOperation::TodoDelete {
            initiator_user_id: scope.owner.user_id.clone(),
            owner_key: scope.owner.key.clone(),
            item: item.clone(),
            created_at: now_iso_cn(),
        });
        scope.save(&self.session_store)?;

        Ok(ToolOutput::json(json!({
            "ok": true,
            "requires_confirmation": true,
            "pending_action": "cancel",
            "message": "已发起取消待办确认；用户确认后只会标记为已取消，不会永久删除。",
            "item": todo_selected_item_json(resolved.single_label(), &item),
        })))
    }
}

#[async_trait]
impl Tool for RestoreTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: RESTORE_TODOS_TOOL_NAME.to_owned(),
            description: "将已完成或已取消待办恢复为未完成。用户明确说“第 N 个”时只能传 numbers 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它 / 恢复的那个”时传 reference=\"last\"。不会接受数据库内部 ID。".to_owned(),
            parameters: number_list_or_reference_schema("要恢复的 visible_number 列表"),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope = TodoToolScope::load(&self.session_store, &context)?;
        let selection = todo_selection_request(&arguments, true)?;
        let resolved = match scope.resolve_selection(&selection, &self.todo_store)? {
            TodoToolSelectionResolution::Resolved(resolved) => resolved,
            TodoToolSelectionResolution::Output(output) => return Ok(output),
        };
        let ids = resolved.ids();
        let completed_outcome = self
            .todo_store
            .restore_completed_by_ids(&scope.owner, &ids)
            .map_err(todo_tool_error)?;
        let cancelled_outcome = self
            .todo_store
            .restore_cancelled_by_ids(&scope.owner, &ids)
            .map_err(todo_tool_error)?;
        let mut restored = selected_items_for_result(&resolved, &completed_outcome.restored);
        restored.extend(selected_items_for_result(
            &resolved,
            &cancelled_outcome.restored,
        ));
        let missing = missing_selection_labels_excluding_items(&resolved, &restored);
        if !restored.is_empty() {
            scope.session.last_todo_query = None;
            let mut combined = completed_outcome.restored.clone();
            combined.extend(cancelled_outcome.restored.clone());
            scope.session.update_last_todo_action_from_items(
                &scope.owner.key,
                "restored",
                &combined,
            );
            scope.save(&self.session_store)?;
        }

        Ok(ToolOutput::json(json!({
            "ok": true,
            "restored": todo_selected_items_json(&restored),
            "missing_numbers": missing_numbers_json(&missing),
            "message": "已恢复的条目已变更为 pending；missing_numbers 表示编号不存在、状态不是已完成/已取消或条目已变化。"
        })))
    }
}

#[async_trait]
impl Tool for DeleteTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: DELETE_TODOS_TOOL_NAME.to_owned(),
            description: "发起永久删除已完成或已取消待办。用户明确说“第 N 个”时只能传 numbers 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它 / 恢复的那个 / 刚完成的”时传 reference=\"last\"。未完成待办不能用本工具永久删除；用户说“不做了/取消/算了”时必须调用 cancel_todo。需要用户确认后才执行。".to_owned(),
            parameters: number_list_or_reference_schema("要永久删除的 visible_number 列表"),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope = TodoToolScope::load(&self.session_store, &context)?;
        let selection = todo_selection_request(&arguments, true)?;
        let resolved = match scope.resolve_selection(&selection, &self.todo_store)? {
            TodoToolSelectionResolution::Resolved(resolved) => resolved,
            TodoToolSelectionResolution::Output(output) => return Ok(output),
        };
        let ids = resolved.ids();
        if ids.is_empty() {
            return Ok(todo_tool_error_output(
                TODO_SELECTION_NOT_FOUND_CODE,
                "no visible numbers matched",
            ));
        }

        let mut items = Vec::new();
        for id in &ids {
            let Some(item) = self
                .todo_store
                .get_by_id(&scope.owner, id)
                .map_err(todo_tool_error)?
            else {
                continue;
            };
            items.push(item);
        }
        if items.is_empty() {
            return Ok(todo_tool_error_output(
                TODO_REFERENCE_UNAVAILABLE_CODE,
                "selected todos no longer exist",
            ));
        }
        if items.iter().any(|item| item.status == TodoStatus::Pending) {
            return Ok(todo_tool_error_output(
                TODO_DELETE_INVALID_STATE_CODE,
                "pending todos cannot be permanently deleted; use cancel_todo to mark them cancelled",
            ));
        }
        let status = items[0].status.clone();
        if items.iter().any(|item| item.status != status) {
            return Ok(todo_tool_error_output(
                TODO_DELETE_MIXED_STATUS_CODE,
                "delete_todos requires all selected todos to have the same terminal status",
            ));
        }

        scope.ensure_no_pending()?;
        let source_condition = format!(
            "{}编号 {}",
            status_label(&status),
            resolved
                .labels
                .iter()
                .map(todo_selection_label_text)
                .collect::<Vec<_>>()
                .join("、")
        );
        scope.session.pending_operation = Some(PendingOperation::TodoBulkDelete {
            initiator_user_id: scope.owner.user_id.clone(),
            owner_key: scope.owner.key.clone(),
            item_ids: items.iter().map(|item| item.id.clone()).collect(),
            matched_count: items.len(),
            status: status.clone(),
            summary: items
                .iter()
                .take(5)
                .map(|item| format!("- {}", item.title))
                .collect::<Vec<_>>()
                .join("\n"),
            source_condition: source_condition.clone(),
            created_at: now_iso_cn(),
        });
        scope.save(&self.session_store)?;

        Ok(ToolOutput::json(json!({
            "ok": true,
            "requires_confirmation": true,
            "pending_action": "delete",
            "message": "已发起永久删除确认；只针对已完成或已取消待办，用户确认后才会删除记录。",
            "source_condition": source_condition,
            "items": todo_items_json(&items),
        })))
    }
}

struct TodoToolScope {
    owner: TodoOwner,
    session: crate::runtime::session::SessionRecord,
}

impl TodoToolScope {
    fn load(session_store: &SessionStore, context: &ToolContext) -> Result<Self, LlmError> {
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
        Ok(Self { owner, session })
    }

    fn remember(&mut self, query_type: &str, condition: &str, items: &[TodoItem]) {
        self.session.last_todo_query = Some(LastTodoQuery {
            owner_key: self.owner.key.clone(),
            query_type: query_type.to_owned(),
            condition: condition.to_owned(),
            result_ids: items.iter().map(|item| item.id.clone()).collect(),
            created_at: now_iso_cn(),
        });
    }

    fn resolve_selection(
        &mut self,
        selection: &TodoSelectionRequest,
        todo_store: &TodoStore,
    ) -> Result<TodoToolSelectionResolution, LlmError> {
        match selection {
            // 用户明确说“第 N 个”时必须继续按最近列表快照解释；即使 session 里有最近对象，
            // 也不能偷偷降级成“刚才那个”，否则状态变化后会误操作。
            TodoSelectionRequest::Numbers(numbers) => Ok(TodoToolSelectionResolution::Resolved(
                self.resolve_numbers(numbers)?,
            )),
            TodoSelectionRequest::Reference(TodoReference::Last) => {
                self.resolve_last_reference(todo_store)
            }
        }
    }

    fn resolve_numbers(&self, numbers: &[usize]) -> Result<ResolvedTodoSelection, LlmError> {
        let query = self
            .session
            .last_todo_query
            .clone()
            .filter(|query| query.owner_key == self.owner.key);
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

    fn save(&mut self, session_store: &SessionStore) -> Result<(), LlmError> {
        session_store
            .save(&mut self.session)
            .map_err(session_tool_error)
    }

    fn ensure_no_pending(&self) -> Result<(), LlmError> {
        if self.session.pending_operation.is_some() {
            // 当前 pending 存储是单槽位；拒绝覆盖可避免模型连续写工具造成前一个确认静默丢失。
            return Err(LlmError::new(
                "pending_operation_exists",
                "current session already has a pending operation; ask the user to confirm or cancel it before creating another pending todo operation",
                "tool",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
enum TodoToolSelectionResolution {
    Resolved(ResolvedTodoSelection),
    Output(ToolOutput),
}

#[derive(Debug, Clone)]
enum TodoToolSingleItemResolution {
    Item(TodoItem),
    Output(ToolOutput),
}

#[derive(Debug, Clone)]
struct ResolvedTodoSelection {
    labels: Vec<TodoSelectionLabel>,
    matched: Vec<(TodoSelectionLabel, String)>,
    missing: Vec<TodoSelectionLabel>,
    error_output: Option<ToolOutput>,
}

impl ResolvedTodoSelection {
    fn single_reference(reference: TodoReference, item_id: String) -> Self {
        let label = TodoSelectionLabel::Reference(reference);
        Self {
            labels: vec![label.clone()],
            matched: vec![(label, item_id)],
            missing: Vec::new(),
            error_output: None,
        }
    }

    fn error(error_code: &str, message: &str) -> Self {
        Self {
            labels: Vec::new(),
            matched: Vec::new(),
            missing: Vec::new(),
            error_output: Some(todo_tool_error_output(error_code, message)),
        }
    }

    fn ids(&self) -> Vec<String> {
        self.matched.iter().map(|(_, id)| id.clone()).collect()
    }

    fn single_label(&self) -> TodoSelectionLabel {
        self.labels
            .first()
            .cloned()
            .unwrap_or(TodoSelectionLabel::Reference(TodoReference::Last))
    }

    fn single_item(
        &self,
        todo_store: &TodoStore,
        owner: &TodoOwner,
    ) -> Result<TodoToolSingleItemResolution, LlmError> {
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
        Ok(TodoToolSingleItemResolution::Item(item))
    }
}

#[derive(Debug, Clone, Copy)]
enum TodoToolListStatus {
    Pending,
    Completed,
    Cancelled,
    All,
}

impl TodoToolListStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::All => "all",
        }
    }

    fn query_type(self) -> &'static str {
        match self {
            Self::Pending => "list",
            Self::Completed => "completed-list",
            Self::Cancelled => "cancelled-list",
            Self::All => "all",
        }
    }

    fn condition(self) -> &'static str {
        match self {
            Self::Pending => "",
            Self::Completed => "已完成列表",
            Self::Cancelled => "已取消列表",
            Self::All => "全部待办",
        }
    }
}

fn todo_status_argument(arguments: &Value, key: &str) -> Result<TodoToolListStatus, LlmError> {
    match arguments.get(key).and_then(Value::as_str) {
        Some("pending") => Ok(TodoToolListStatus::Pending),
        Some("completed") => Ok(TodoToolListStatus::Completed),
        Some("cancelled") => Ok(TodoToolListStatus::Cancelled),
        Some("all") => Ok(TodoToolListStatus::All),
        _ => Err(bad_tool_arguments(
            "status must be pending/completed/cancelled/all",
        )),
    }
}

fn number_list_or_reference_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "numbers": {
                "type": "array",
                "description": description,
                "minItems": 1,
                "maxItems": TODO_TOOL_MAX_NUMBERS,
                "items": {
                    "type": "integer",
                    "minimum": 1
                }
            },
            "reference": {
                "type": ["string", "null"],
                "enum": [TODO_REFERENCE_LAST, null],
                "description": "当用户说“刚才那个 / 它 / 恢复的那个 / 刚完成的”时传 \"last\"；与 numbers 二选一。"
            }
        },
        "required": ["numbers", "reference"],
        "additionalProperties": false
    })
}

fn single_number_or_reference_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "number": {
                "type": ["integer", "null"],
                "minimum": 1,
                "description": description
            },
            "reference": {
                "type": ["string", "null"],
                "enum": [TODO_REFERENCE_LAST, null],
                "description": "当用户说“刚才那个 / 它 / 恢复的那个 / 刚完成的”时传 \"last\"；与 number 二选一。"
            }
        },
        "required": ["number", "reference"],
        "additionalProperties": false
    })
}

fn todo_selection_request(
    arguments: &Value,
    allow_many: bool,
) -> Result<TodoSelectionRequest, LlmError> {
    let numbers = optional_number_list(arguments, "numbers")?;
    let reference = optional_reference(arguments, "reference")?;
    match (numbers, reference) {
        (Some(numbers), None) => {
            if !allow_many && numbers.len() != 1 {
                return Err(bad_tool_arguments("numbers must contain exactly one item"));
            }
            Ok(TodoSelectionRequest::Numbers(numbers))
        }
        (None, Some(reference)) => Ok(TodoSelectionRequest::Reference(reference)),
        (Some(_), Some(_)) => Err(bad_tool_arguments(
            "numbers and reference are mutually exclusive",
        )),
        (None, None) => Err(bad_tool_arguments(
            "either numbers or reference is required",
        )),
    }
}

fn single_todo_selection_request(arguments: &Value) -> Result<TodoSelectionRequest, LlmError> {
    let number = optional_positive_usize(arguments, "number")?;
    let reference = optional_reference(arguments, "reference")?;
    match (number, reference) {
        (Some(number), None) => Ok(TodoSelectionRequest::Numbers(vec![number])),
        (None, Some(reference)) => Ok(TodoSelectionRequest::Reference(reference)),
        (Some(_), Some(_)) => Err(bad_tool_arguments(
            "number and reference are mutually exclusive",
        )),
        (None, None) => Err(bad_tool_arguments("either number or reference is required")),
    }
}

fn optional_number_list(arguments: &Value, key: &str) -> Result<Option<Vec<usize>>, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => Ok(Some(parse_number_list(values)?)),
        _ => Err(bad_tool_arguments(format!(
            "{key} must be an array or null"
        ))),
    }
}

fn parse_number_list(values: &[Value]) -> Result<Vec<usize>, LlmError> {
    if values.is_empty() || values.len() > TODO_TOOL_MAX_NUMBERS {
        return Err(bad_tool_arguments("numbers length is out of range"));
    }
    let mut numbers = Vec::new();
    for value in values {
        let number = value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| bad_tool_arguments("numbers must contain positive integers"))?;
        if !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    Ok(numbers)
}

fn optional_positive_usize(arguments: &Value, key: &str) -> Result<Option<usize>, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => arguments
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| bad_tool_arguments(format!("{key} must be a positive integer"))),
    }
}

fn optional_reference(arguments: &Value, key: &str) -> Result<Option<TodoReference>, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => match value.as_str() {
            TODO_REFERENCE_LAST => Ok(Some(TodoReference::Last)),
            _ => Err(bad_tool_arguments(format!(
                "{key} must be \"last\" or null"
            ))),
        },
        _ => Err(bad_tool_arguments(format!("{key} must be string or null"))),
    }
}

fn required_non_empty_text(arguments: &Value, key: &str) -> Result<String, LlmError> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bad_tool_arguments(format!("{key} must be a non-empty string")))?;
    if value.chars().count() > TODO_TOOL_MAX_TEXT_CHARS {
        return Err(bad_tool_arguments(format!("{key} is too long")));
    }
    Ok(value.to_owned())
}

fn optional_text(arguments: &Value, key: &str) -> Result<Option<String>, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else if value.chars().count() > TODO_TOOL_MAX_TEXT_CHARS {
                Err(bad_tool_arguments(format!("{key} is too long")))
            } else {
                Ok(Some(value.to_owned()))
            }
        }
        _ => Err(bad_tool_arguments(format!("{key} must be string or null"))),
    }
}

fn optional_time_precision(arguments: &Value, key: &str) -> Result<TodoTimePrecision, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(TodoTimePrecision::None),
        Some(Value::String(value)) => match value.as_str() {
            "none" => Ok(TodoTimePrecision::None),
            "date" => Ok(TodoTimePrecision::Date),
            "date_time" => Ok(TodoTimePrecision::DateTime),
            "inferred" => Ok(TodoTimePrecision::Inferred),
            _ => Err(bad_tool_arguments("invalid time_precision")),
        },
        _ => Err(bad_tool_arguments(format!("{key} must be string or null"))),
    }
}

fn todo_items_json(items: &[TodoItem]) -> Vec<Value> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| todo_numbered_item_json(index + 1, item))
        .collect()
}

fn todo_selected_items_json(items: &[(TodoSelectionLabel, TodoItem)]) -> Vec<Value> {
    items
        .iter()
        .map(|(label, item)| todo_selected_item_json(label.clone(), item))
        .collect()
}

fn todo_numbered_item_json(number: usize, item: &TodoItem) -> Value {
    todo_selected_item_json(TodoSelectionLabel::Number(number), item)
}

fn todo_selected_item_json(label: TodoSelectionLabel, item: &TodoItem) -> Value {
    let mut object = todo_item_json_object(item);
    match label {
        TodoSelectionLabel::Number(number) => {
            object.insert("visible_number".to_owned(), json!(number));
        }
        TodoSelectionLabel::Reference(reference) => {
            object.insert("reference".to_owned(), json!(reference.as_str()));
        }
    }
    Value::Object(object)
}

fn todo_item_json_object(item: &TodoItem) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("title".to_owned(), json!(item.title));
    object.insert("detail".to_owned(), json!(item.detail));
    object.insert("due_date".to_owned(), json!(item.due_date));
    object.insert("due_at".to_owned(), json!(item.due_at));
    object.insert("display_time".to_owned(), json!(display_todo_time(item)));
    object.insert("status".to_owned(), json!(todo_status_json(&item.status)));
    object.insert("created_at".to_owned(), json!(item.created_at));
    object.insert("updated_at".to_owned(), json!(item.updated_at));
    object.insert("completed_at".to_owned(), json!(item.completed_at));
    object.insert("cancelled_at".to_owned(), json!(item.cancelled_at));
    object
}

fn todo_draft_json(draft: &TodoItemDraft) -> Value {
    json!({
        "title": draft.title,
        "detail": draft.detail,
        "due_date": draft.due_date,
        "due_at": draft.due_at,
        "display_time": display_draft_time(draft),
        "time_precision": todo_time_precision_json(&draft.time_precision),
    })
}

fn todo_status_json(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::Completed => "completed",
        TodoStatus::Cancelled => "cancelled",
    }
}

fn todo_time_precision_json(precision: &TodoTimePrecision) -> &'static str {
    match precision {
        TodoTimePrecision::None => "none",
        TodoTimePrecision::Date => "date",
        TodoTimePrecision::DateTime => "date_time",
        TodoTimePrecision::Inferred => "inferred",
    }
}

fn status_label(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "未完成待办",
        TodoStatus::Completed => "已完成待办",
        TodoStatus::Cancelled => "已取消待办",
    }
}

fn selected_items_for_result(
    resolved: &ResolvedTodoSelection,
    items: &[TodoItem],
) -> Vec<(TodoSelectionLabel, TodoItem)> {
    let mut result = Vec::new();
    for (label, id) in &resolved.matched {
        if let Some(item) = items.iter().find(|item| &item.id == id) {
            result.push((label.clone(), item.clone()));
        }
    }
    result
}

fn missing_selection_labels_for_result(
    resolved: &ResolvedTodoSelection,
    skipped_ids: &[String],
) -> Vec<TodoSelectionLabel> {
    let mut missing = resolved.missing.clone();
    for (label, id) in &resolved.matched {
        if skipped_ids.iter().any(|skipped| skipped == id) && !missing.contains(label) {
            missing.push(label.clone());
        }
    }
    missing
}

fn missing_selection_labels_excluding_items(
    resolved: &ResolvedTodoSelection,
    items: &[(TodoSelectionLabel, TodoItem)],
) -> Vec<TodoSelectionLabel> {
    let restored_ids = items
        .iter()
        .map(|(_, item)| item.id.as_str())
        .collect::<HashSet<_>>();
    let mut missing = resolved.missing.clone();
    for (label, id) in &resolved.matched {
        if !restored_ids.contains(id.as_str()) && !missing.contains(label) {
            missing.push(label.clone());
        }
    }
    missing
}

fn missing_numbers_json(labels: &[TodoSelectionLabel]) -> Vec<Value> {
    labels
        .iter()
        .map(|label| match label {
            TodoSelectionLabel::Number(number) => json!(number),
            TodoSelectionLabel::Reference(reference) => json!(reference.as_str()),
        })
        .collect()
}

fn todo_selection_label_text(label: &TodoSelectionLabel) -> String {
    match label {
        TodoSelectionLabel::Number(number) => number.to_string(),
        TodoSelectionLabel::Reference(reference) => reference.as_str().to_owned(),
    }
}

fn todo_tool_error(err: crate::runtime::todo::TodoError) -> LlmError {
    LlmError::new(err.code().to_owned(), err.message().to_owned(), "todo_tool")
}

fn session_tool_error(err: crate::runtime::session::SessionError) -> LlmError {
    LlmError::new(err.code().to_owned(), err.message().to_owned(), "todo_tool")
}

fn bad_tool_arguments(message: impl Into<String>) -> LlmError {
    LlmError::new("bad_tool_arguments", message, "tool")
}

fn todo_tool_error_output(error_code: &str, message: &str) -> ToolOutput {
    ToolOutput::json(json!({
        "ok": false,
        "error_code": error_code,
        "message": message,
    }))
}
