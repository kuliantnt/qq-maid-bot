//! 群管理员 Todo 列表编号的作用域绑定与解析。
//!
//! 群管理编号不能复用普通个人 Todo 的 owner 快照。这里把操作者、平台、机器人账号
//! 和完整群 conversation scope 一起绑定到 `LastTodoQuery`；删除时还会由 storage 再次
//! 校验目标记录的群 scope，形成“快照授权 + 当前记录校验”两层边界。

use crate::runtime::{
    freshness::query_is_fresh,
    respond::common::CommandBody,
    session::{LAST_QUERY_TTL_SECONDS, SessionRecord},
    tools::todo::{TodoItem, format::TODO_LIST_VISIBLE_LIMIT},
};

const GROUP_ADMIN_QUERY_TYPE: &str = "group-admin-list";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupTodoAdminContext {
    actor_id: String,
    platform: String,
    account_id: Option<String>,
    scope_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupTodoSelectionError {
    InvalidNumber,
    SnapshotUnavailable,
}

impl GroupTodoAdminContext {
    pub(crate) fn new(
        actor_id: &str,
        platform: &str,
        account_id: Option<&str>,
        scope_key: &str,
    ) -> Option<Self> {
        let actor_id = clean_required(actor_id)?;
        let platform = clean_required(platform)?;
        let scope_key = clean_required(scope_key)?;
        Some(Self {
            actor_id,
            platform,
            account_id: account_id.and_then(clean_required),
            scope_key,
        })
    }

    fn snapshot_owner_key(&self) -> String {
        // 该键只在服务端 session 中比较，不向用户或模型展示。长度前缀避免不同字段中
        // 的冒号造成拼接歧义；完整 scope 已包含平台账号时仍保留独立字段做双重绑定。
        [
            "todo-group-admin-v1".to_owned(),
            length_prefixed(&self.actor_id),
            length_prefixed(&self.platform),
            length_prefixed(self.account_id.as_deref().unwrap_or("")),
            length_prefixed(&self.scope_key),
        ]
        .join(":")
    }
}

pub(crate) fn remember_group_todo_query(
    session: &mut SessionRecord,
    context: &GroupTodoAdminContext,
    items: &[TodoItem],
) {
    let owner_key = context.snapshot_owner_key();
    session.remember_last_todo_query(
        &owner_key,
        GROUP_ADMIN_QUERY_TYPE,
        "当前群未完成 Todo",
        items.iter().map(|item| item.id.clone()).collect(),
    );
}

pub(crate) fn visible_group_todo_items(items: &[TodoItem]) -> &[TodoItem] {
    &items[..items.len().min(TODO_LIST_VISIBLE_LIMIT)]
}

pub(crate) fn format_group_todo_list_reply(
    items: &[TodoItem],
    creator_names: &[String],
    total_count: usize,
) -> CommandBody {
    if items.is_empty() {
        return CommandBody::plain("当前群没有未完成的 Todo 或提醒。");
    }

    let mut lines = vec!["👥 当前群 Todo / 提醒".to_owned(), String::new()];
    for (index, item) in items.iter().enumerate() {
        lines.push(super::format::format_todo_natural_list_item(
            index,
            item,
            super::format::todo_due_chip(item),
            false,
            None,
        ));
        let creator = creator_names
            .get(index)
            .map(String::as_str)
            .unwrap_or("群成员");
        lines.push(format!("   创建者：{creator}"));
        if index + 1 < items.len() {
            lines.push(String::new());
        }
    }
    if total_count > items.len() {
        lines.push(String::new());
        lines.push(format!(
            "当前共 {total_count} 条，仅展示前 {} 条。",
            items.len()
        ));
    }
    lines.push(String::new());
    lines.push("群主或管理员可发送 /todo group delete <编号> 删除。".to_owned());
    CommandBody::plain(lines.join("\n"))
}

pub(crate) fn format_group_todo_deleted_reply(item: &TodoItem) -> CommandBody {
    CommandBody::plain(format!(
        "已删除当前群 Todo：{}。对应提醒已取消。",
        item.title
    ))
}

pub(crate) fn format_group_todo_usage_reply() -> CommandBody {
    CommandBody::plain("用法：/todo group；/todo group delete <编号>。")
}

pub(crate) fn resolve_group_todo_number(
    session: &mut SessionRecord,
    context: &GroupTodoAdminContext,
    number: usize,
) -> Result<String, GroupTodoSelectionError> {
    if number == 0 {
        return Err(GroupTodoSelectionError::InvalidNumber);
    }
    let Some(query) = session.last_todo_query.clone() else {
        return Err(GroupTodoSelectionError::SnapshotUnavailable);
    };
    if query.query_type != GROUP_ADMIN_QUERY_TYPE
        || query.owner_key != context.snapshot_owner_key()
        || !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS)
    {
        session.last_todo_query = None;
        return Err(GroupTodoSelectionError::SnapshotUnavailable);
    }
    query
        .result_ids
        .get(number - 1)
        .cloned()
        .ok_or(GroupTodoSelectionError::InvalidNumber)
}

fn clean_required(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn length_prefixed(value: &str) -> String {
    format!("{}:{value}", value.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::session::{SessionMeta, SessionStore};
    use crate::storage::{database::SqliteDatabase, session::SESSION_MIGRATIONS};

    fn session() -> SessionRecord {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-group-todo-snapshot-{}.db",
            uuid::Uuid::new_v4()
        ));
        let database = SqliteDatabase::open(path, SESSION_MIGRATIONS).unwrap();
        SessionStore::new(database)
            .create(
                &SessionMeta::new(
                    "group:g1",
                    Some("admin-1".to_owned()),
                    Some("g1".to_owned()),
                    None,
                    None,
                    "qq_official",
                ),
                "",
                true,
            )
            .unwrap()
    }

    fn item(id: &str) -> TodoItem {
        TodoItem {
            id: id.to_owned(),
            user_id: Some("creator".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "群待办".to_owned(),
            detail: None,
            raw_text: None,
            due_date: None,
            due_at: None,
            reminder_at: None,
            time_precision: Default::default(),
            recurrence_kind: Default::default(),
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: Default::default(),
            status: Default::default(),
            created_at: String::new(),
            updated_at: String::new(),
            completed_at: None,
        }
    }

    #[test]
    fn snapshot_is_bound_to_actor_platform_account_and_group_scope() {
        let mut session = session();
        let original =
            GroupTodoAdminContext::new("admin-1", "qq_official", Some("bot-1"), "group:g1")
                .unwrap();
        remember_group_todo_query(&mut session, &original, &[item("7")]);
        assert_eq!(
            resolve_group_todo_number(&mut session, &original, 1).unwrap(),
            "7"
        );

        for changed in [
            GroupTodoAdminContext::new("admin-2", "qq_official", Some("bot-1"), "group:g1")
                .unwrap(),
            GroupTodoAdminContext::new("admin-1", "onebot", Some("bot-1"), "group:g1").unwrap(),
            GroupTodoAdminContext::new("admin-1", "qq_official", Some("bot-2"), "group:g1")
                .unwrap(),
            GroupTodoAdminContext::new("admin-1", "qq_official", Some("bot-1"), "group:g2")
                .unwrap(),
        ] {
            let mut candidate = session.clone();
            assert_eq!(
                resolve_group_todo_number(&mut candidate, &changed, 1),
                Err(GroupTodoSelectionError::SnapshotUnavailable)
            );
        }
    }

    #[test]
    fn expired_or_out_of_range_snapshot_number_is_rejected() {
        let mut session = session();
        let context =
            GroupTodoAdminContext::new("admin-1", "qq_official", None, "group:g1").unwrap();
        remember_group_todo_query(&mut session, &context, &[item("7")]);
        assert_eq!(
            resolve_group_todo_number(&mut session, &context, 2),
            Err(GroupTodoSelectionError::InvalidNumber)
        );

        session.last_todo_query.as_mut().unwrap().created_at =
            "2000-01-01T00:00:00+08:00".to_owned();
        assert_eq!(
            resolve_group_todo_number(&mut session, &context, 1),
            Err(GroupTodoSelectionError::SnapshotUnavailable)
        );
        assert!(session.last_todo_query.is_none());
    }
}
