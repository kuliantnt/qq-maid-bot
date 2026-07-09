//! Todo 域的可见实体快照适配。
//!
//! 通用 `runtime::visible_entity` 只负责校验和按 domain/kind 解析；Todo 的
//! domain 名、实体 kind、列表/单条回执/提醒快照构造都收在本模块，避免 respond
//! 或通知通用层理解 Todo 业务字段。

use crate::{
    error::LlmError,
    runtime::{
        session::{SessionMeta, SessionRecord, now_iso_cn},
        visible_entity::{
            VisibleEntityRequestContext, VisibleEntitySelectionScope,
            selection_scope_from_visible_snapshot, visible_snapshot_has_domain_items,
        },
    },
    service::{VisibleEntityItem, VisibleEntitySnapshot},
    storage::notification::NotificationOutboxStore,
};
use qq_maid_llm::tool::ToolRegistry;

use super::{
    CompleteTodoTool, DeleteTodoTool, EditTodoTool, GetTodoTool, ManageRecurringReminderTool,
    MergeTodoTool, RestoreTodoTool, TodoItem, TodoOwner, TodoStore,
};

const TODO_DOMAIN: &str = "todo";
const TODO_ENTITY_KIND: &str = "todo";

pub(crate) fn todo_visible_entity_snapshot(
    session: &SessionRecord,
    meta: Option<&SessionMeta>,
) -> Option<VisibleEntitySnapshot> {
    let query = session.last_todo_query.as_ref()?;
    if query.result_ids.is_empty() {
        return None;
    }
    Some(VisibleEntitySnapshot {
        platform: meta
            .map(|meta| meta.platform.clone())
            .unwrap_or_else(|| session.platform.clone()),
        account_id: meta.and_then(|meta| meta.account_id.clone()),
        scope_key: meta
            .map(|meta| meta.scope_key.clone())
            .unwrap_or_else(|| session.scope_key.clone()),
        owner_key: Some(query.owner_key.clone()),
        created_at: query.created_at.clone(),
        items: query
            .result_ids
            .iter()
            .enumerate()
            .map(|(index, id)| VisibleEntityItem {
                domain: TODO_DOMAIN.to_owned(),
                entity_kind: TODO_ENTITY_KIND.to_owned(),
                entity_id: id.clone(),
                visible_number: index + 1,
                label: None,
                status: Some(query.query_type.clone()),
            })
            .collect(),
    })
}

pub(crate) fn todo_last_action_visible_entity_snapshot(
    session: &SessionRecord,
    meta: Option<&SessionMeta>,
) -> Option<VisibleEntitySnapshot> {
    let action = session.last_todo_action.as_ref()?;
    todo_single_visible_entity_snapshot(TodoSingleVisibleEntityInput {
        platform: meta
            .map(|meta| meta.platform.as_str())
            .unwrap_or(session.platform.as_str()),
        account_id: meta.and_then(|meta| meta.account_id.as_deref()),
        scope_key: meta
            .map(|meta| meta.scope_key.as_str())
            .unwrap_or(session.scope_key.as_str()),
        owner_key: &action.owner_key,
        item_id: &action.item_id,
        label: Some(action.title.as_str()),
        status: Some(action.action.as_str()),
        created_at: action.created_at.as_str(),
    })
}

pub(crate) fn todo_item_visible_entity_snapshot(
    platform: &str,
    account_id: Option<&str>,
    scope_key: &str,
    owner: &TodoOwner,
    item: &TodoItem,
    status: Option<&str>,
) -> Option<VisibleEntitySnapshot> {
    todo_single_visible_entity_snapshot(TodoSingleVisibleEntityInput {
        platform,
        account_id,
        scope_key,
        owner_key: &owner.key,
        item_id: &item.id,
        label: Some(item.title.as_str()),
        status,
        created_at: &now_iso_cn(),
    })
}

pub(crate) fn todo_selection_scope_from_visible_snapshot(
    snapshot: Option<&VisibleEntitySnapshot>,
    context: VisibleEntityRequestContext<'_>,
) -> Option<VisibleEntitySelectionScope> {
    selection_scope_from_visible_snapshot(snapshot, context, TODO_DOMAIN, TODO_ENTITY_KIND)
}

pub(crate) struct TodoScopedToolInputs<'a> {
    pub registry: &'a mut ToolRegistry,
    pub enabled_tools: &'a [String],
    pub todo_store: &'a TodoStore,
    pub session_store: &'a crate::runtime::session::SessionStore,
    pub notification_store: &'a NotificationOutboxStore,
    pub snapshot: Option<&'a VisibleEntitySnapshot>,
    pub platform: &'a str,
    pub account_id: Option<&'a str>,
    pub scope_key: &'a str,
    pub user_id: Option<&'a str>,
    pub quoted_bot_lookup: bool,
}

pub(crate) fn replace_scoped_todo_tools_from_visible_snapshot(
    input: TodoScopedToolInputs<'_>,
) -> Result<(), LlmError> {
    let owner = TodoStore::owner(input.user_id, input.scope_key);
    let Some(scope) = todo_selection_scope_from_visible_snapshot(
        input.snapshot,
        VisibleEntityRequestContext {
            platform: input.platform,
            account_id: input.account_id,
            scope_key: input.scope_key,
            owner_key: Some(owner.key.as_str()),
            quoted_bot_lookup: input.quoted_bot_lookup,
        },
    ) else {
        return Ok(());
    };
    replace_scoped_todo_tools(
        input.registry,
        input.enabled_tools,
        input.todo_store,
        input.session_store,
        input.notification_store,
        scope,
    )
}

pub(crate) fn visible_snapshot_has_todo_items(snapshot: Option<&VisibleEntitySnapshot>) -> bool {
    visible_snapshot_has_domain_items(snapshot, TODO_DOMAIN, TODO_ENTITY_KIND)
}

struct TodoSingleVisibleEntityInput<'a> {
    platform: &'a str,
    account_id: Option<&'a str>,
    scope_key: &'a str,
    owner_key: &'a str,
    item_id: &'a str,
    label: Option<&'a str>,
    status: Option<&'a str>,
    created_at: &'a str,
}

fn todo_single_visible_entity_snapshot(
    input: TodoSingleVisibleEntityInput<'_>,
) -> Option<VisibleEntitySnapshot> {
    if input.item_id.trim().is_empty()
        || input.owner_key.trim().is_empty()
        || input.scope_key.trim().is_empty()
    {
        return None;
    }
    Some(VisibleEntitySnapshot {
        platform: input.platform.to_owned(),
        account_id: input.account_id.map(str::to_owned),
        scope_key: input.scope_key.to_owned(),
        owner_key: Some(input.owner_key.to_owned()),
        created_at: input.created_at.to_owned(),
        items: vec![VisibleEntityItem {
            domain: TODO_DOMAIN.to_owned(),
            entity_kind: TODO_ENTITY_KIND.to_owned(),
            entity_id: input.item_id.to_owned(),
            visible_number: 1,
            label: input.label.map(str::to_owned),
            status: input.status.map(str::to_owned),
        }],
    })
}

fn replace_scoped_todo_tools(
    registry: &mut ToolRegistry,
    enabled_tools: &[String],
    todo_store: &TodoStore,
    session_store: &crate::runtime::session::SessionStore,
    notification_store: &NotificationOutboxStore,
    scope: VisibleEntitySelectionScope,
) -> Result<(), LlmError> {
    let enabled = |name: &str| enabled_tools.iter().any(|tool| tool == name);
    if enabled("get_todo") {
        registry.replace(std::sync::Arc::new(
            GetTodoTool::new(todo_store.clone(), session_store.clone())
                .with_selection_scope(scope.clone()),
        ))?;
    }
    if enabled("complete_todos") {
        registry.replace(std::sync::Arc::new(
            CompleteTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .with_selection_scope(scope.clone()),
        ))?;
    }
    if enabled("edit_todo") {
        registry.replace(std::sync::Arc::new(
            EditTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .with_selection_scope(scope.clone()),
        ))?;
    }
    if enabled("restore_todos") {
        registry.replace(std::sync::Arc::new(
            RestoreTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .with_selection_scope(scope.clone()),
        ))?;
    }
    if enabled("delete_todos") {
        registry.replace(std::sync::Arc::new(
            DeleteTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .with_selection_scope(scope.clone()),
        ))?;
    }
    if enabled("merge_todos") {
        registry.replace(std::sync::Arc::new(
            MergeTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .with_selection_scope(scope.clone()),
        ))?;
    }
    if enabled("manage_recurring_reminder") {
        registry.replace(std::sync::Arc::new(
            ManageRecurringReminderTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .with_selection_scope(scope),
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::visible_entity::VisibleEntityRequestContext;

    fn todo_item(entity_id: &str, visible_number: usize) -> VisibleEntityItem {
        VisibleEntityItem {
            domain: "todo".to_owned(),
            entity_kind: "todo".to_owned(),
            entity_id: entity_id.to_owned(),
            visible_number,
            label: None,
            status: Some("pending".to_owned()),
        }
    }

    fn snapshot(
        scope_key: &str,
        owner_key: Option<String>,
        account_id: Option<&str>,
        items: Vec<VisibleEntityItem>,
    ) -> VisibleEntitySnapshot {
        VisibleEntitySnapshot {
            platform: "qq_official".to_owned(),
            account_id: account_id.map(str::to_owned),
            scope_key: scope_key.to_owned(),
            owner_key,
            created_at: crate::runtime::session::now_iso_cn(),
            items,
        }
    }

    #[test]
    fn quoted_snapshot_account_mismatch_blocks_without_fallback() {
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let snapshot = snapshot(
            "private:u1",
            Some(owner.key.clone()),
            Some("app-b"),
            vec![todo_item("todo-a-1", 1)],
        );

        assert!(matches!(
            todo_selection_scope_from_visible_snapshot(
                Some(&snapshot),
                VisibleEntityRequestContext {
                    platform: "qq_official",
                    account_id: Some("app-a"),
                    scope_key: "private:u1",
                    owner_key: Some(owner.key.as_str()),
                    quoted_bot_lookup: true,
                },
            ),
            Some(VisibleEntitySelectionScope::Blocked)
        ));
    }

    #[test]
    fn quoted_snapshot_group_owner_mismatch_blocks_without_fallback() {
        let group_scope = "platform:qq_official:account:app-1:group:g1";
        let owner_user_a = TodoStore::owner(Some("u1"), group_scope);
        let owner_user_b = TodoStore::owner(Some("u2"), group_scope);
        let snapshot = snapshot(
            group_scope,
            Some(owner_user_a.key.clone()),
            Some("app-1"),
            vec![todo_item("todo-a-1", 1)],
        );

        assert!(matches!(
            todo_selection_scope_from_visible_snapshot(
                Some(&snapshot),
                VisibleEntityRequestContext {
                    platform: "qq_official",
                    account_id: Some("app-1"),
                    scope_key: group_scope,
                    owner_key: Some(owner_user_b.key.as_str()),
                    quoted_bot_lookup: true,
                },
            ),
            Some(VisibleEntitySelectionScope::Blocked)
        ));
    }

    #[test]
    fn quoted_snapshot_group_scope_mismatch_blocks_without_fallback() {
        let group_a = "platform:qq_official:account:app-1:group:g1";
        let group_b = "platform:qq_official:account:app-1:group:g2";
        let owner_in_b = TodoStore::owner(Some("u1"), group_b);
        let snapshot = snapshot(
            group_a,
            Some(owner_in_b.key.clone()),
            Some("app-1"),
            vec![todo_item("todo-g1-1", 1)],
        );

        assert!(matches!(
            todo_selection_scope_from_visible_snapshot(
                Some(&snapshot),
                VisibleEntityRequestContext {
                    platform: "qq_official",
                    account_id: Some("app-1"),
                    scope_key: group_b,
                    owner_key: Some(owner_in_b.key.as_str()),
                    quoted_bot_lookup: true,
                },
            ),
            Some(VisibleEntitySelectionScope::Blocked)
        ));
    }
}
