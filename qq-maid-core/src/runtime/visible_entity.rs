//! 用户可见实体快照与引用绑定解析。
//!
//! Core 在这里定义“出站消息展示了哪些可被编号引用的业务实体”。具体业务域只负责
//! 把自己的查询结果投影为 visible entity；Gateway 只保存并按消息引用回填快照，不解析
//! `domain` / `entity_kind` 的业务含义。

use std::sync::Arc;

use crate::{
    runtime::{
        session::{LAST_QUERY_TTL_SECONDS, SessionMeta, SessionRecord, query_is_fresh},
        tools::SelectionScope,
        tools::todo::TodoOwner,
    },
    service::{VisibleEntityItem, VisibleEntitySnapshot},
};

const TODO_DOMAIN: &str = "todo";
const TODO_ENTITY_KIND: &str = "todo";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VisibleEntitySelectionScope {
    Scoped(Arc<[String]>),
    Blocked,
}

pub(crate) struct VisibleEntityRequestContext<'a> {
    pub(crate) platform: &'a str,
    pub(crate) account_id: Option<&'a str>,
    pub(crate) scope_key: &'a str,
    pub(crate) owner_key: Option<&'a str>,
    pub(crate) quoted_bot_lookup: bool,
}

/// 从消息引用回填的通用 visible snapshot 中解析某个业务域的可见编号作用域。
///
/// 有过机器人消息引用查找但快照缺失或不匹配时返回 `Blocked`，调用方必须禁止回退到
/// 当前 session 的旧编号快照，避免跨 owner / account / conversation 误操作。
pub(crate) fn selection_scope_from_visible_snapshot(
    snapshot: Option<&VisibleEntitySnapshot>,
    context: VisibleEntityRequestContext<'_>,
    domain: &str,
    entity_kind: &str,
) -> Option<VisibleEntitySelectionScope> {
    let Some(snapshot) = snapshot else {
        return context
            .quoted_bot_lookup
            .then_some(VisibleEntitySelectionScope::Blocked);
    };
    if snapshot.scope_key != context.scope_key
        || snapshot.platform != context.platform
        || snapshot.account_id.as_deref() != context.account_id
        || snapshot
            .owner_key
            .as_deref()
            .is_some_and(|key| Some(key) != context.owner_key)
        || !query_is_fresh(&snapshot.created_at, LAST_QUERY_TTL_SECONDS)
    {
        return Some(VisibleEntitySelectionScope::Blocked);
    }

    let mut items = snapshot
        .items
        .iter()
        .filter(|item| item.domain == domain && item.entity_kind == entity_kind)
        .collect::<Vec<_>>();
    if items.is_empty() {
        return context
            .quoted_bot_lookup
            .then_some(VisibleEntitySelectionScope::Blocked);
    }
    items.sort_by_key(|item| item.visible_number);
    if items
        .iter()
        .enumerate()
        .any(|(index, item)| item.visible_number != index + 1 || item.entity_id.trim().is_empty())
    {
        return Some(VisibleEntitySelectionScope::Blocked);
    }
    Some(VisibleEntitySelectionScope::Scoped(Arc::from(
        items
            .into_iter()
            .map(|item| item.entity_id.clone())
            .collect::<Vec<_>>(),
    )))
}

pub(crate) fn visible_snapshot_has_domain_items(
    snapshot: Option<&VisibleEntitySnapshot>,
    domain: &str,
    entity_kind: &str,
) -> bool {
    snapshot.is_some_and(|snapshot| {
        snapshot
            .items
            .iter()
            .any(|item| item.domain == domain && item.entity_kind == entity_kind)
    })
}

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

pub(crate) fn todo_selection_scope_from_visible_snapshot(
    snapshot: Option<&VisibleEntitySnapshot>,
    context: VisibleEntityRequestContext<'_>,
    _owner: &TodoOwner,
) -> Option<SelectionScope> {
    selection_scope_from_visible_snapshot(snapshot, context, TODO_DOMAIN, TODO_ENTITY_KIND).map(
        |scope| match scope {
            VisibleEntitySelectionScope::Scoped(ids) => SelectionScope::Scoped(ids),
            VisibleEntitySelectionScope::Blocked => SelectionScope::Blocked,
        },
    )
}

pub(crate) fn visible_snapshot_has_todo_items(snapshot: Option<&VisibleEntitySnapshot>) -> bool {
    visible_snapshot_has_domain_items(snapshot, TODO_DOMAIN, TODO_ENTITY_KIND)
}
