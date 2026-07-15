// 拆分后这些不再随 `super::*` 自动进入命名空间，测试体里仍直接引用完整类型/宏。
use std::sync::Arc;

use serde_json::{Value, json};

use qq_maid_common::identity_context::{
    ConversationKind, ExecutionActorContext, ExecutionConversationContext,
};
use qq_maid_llm::{
    error::LlmError,
    tool::{Tool, ToolContext, ToolOutput},
};

use crate::runtime::session::{SessionMeta, SessionStore};
use crate::runtime::tools::todo::{
    TodoItem, TodoItemDraft, TodoOwner, TodoPendingPayload, TodoStatus, TodoStore,
    TodoTimePrecision,
};

use super::scope::{SelectionScope, TodoToolScope};
use super::{
    CompleteTodoTool, CreateTodoTool, DeleteTodoTool, EditTodoTool, GetTodoTool, ListTodoTool,
    ManageRecurringReminderTool, MergeTodoTool, RestoreTodoTool, common, format,
};
use crate::storage::{APP_MIGRATIONS, database::SqliteDatabase};

use super::common::{
    TODO_TOOL_MAX_BATCH_CREATE_ITEMS, TODO_TOOL_MAX_NUMBERS, TodoReference, TodoSelectionRequest,
};

mod complete;
mod create;
mod delete;
mod edit;
mod list;
mod merge;
mod recurrence;
mod reminder;
mod schema;
mod selection;
mod support;
