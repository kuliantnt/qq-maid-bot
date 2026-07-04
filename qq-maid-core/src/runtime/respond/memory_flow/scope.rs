//! 记忆命令的作用域（scope）判定与待确认 pending 的作用域还原。
//!
//! - `MemoryCommandScope` 描述当前命令作用于“个人”还是“群”记忆，以及群命令标记；
//! - `memory_command_scope` 根据命令参数与 `SessionMeta` 决定 scope，群命令必须在群聊；
//! - `pending_*_scope` 把待确认草稿里记录的 scope 反向还原成 `MemoryCommandScope`，
//!   避免跨会话或跨成员复用 pending；
//! - `resolve_memory_target` 与 `remember_memory_query` 维护“最近列表序号”快照，
//!   管理命令只接受当前 scope 下最近列表里的序号，不再回退到 ID 前缀。
//!
//! 安全边界：写入/更新/删除长期记忆都需要稳定用户标识（`memory_actor`），
//! 且 pending 的 scope 必须与当前会话一致，否则按 scope 不匹配拒绝。

use crate::runtime::{
    command::ParsedCommand,
    memory::{MemoryActor, MemoryRecord, MemoryScopeType},
    pending::{PendingMemory, PendingMemoryDelete, PendingMemoryUpdate},
    respond::common::{LAST_QUERY_TTL_SECONDS, clean_string, query_is_fresh},
    session::{LastMemoryQuery, SessionMeta, SessionRecord, now_iso_cn},
};

/// 记忆操作目标：只允许通过最近列表序号解析出的真实 ID 或无效序号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MemoryTarget {
    /// 已解析为真实记忆 ID
    ResolvedId(String),
    /// 列表序号缺失或超出范围，记录原始输入用于错误提示
    MissingListIndex(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemoryCommandScope {
    pub(super) scope_type: MemoryScopeType,
    pub(super) scope_id: String,
    pub(super) label: &'static str,
    pub(super) group_command: bool,
}

pub(super) fn memory_command_scope(
    command: &ParsedCommand,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    let group_command = memory_command_targets_group(command);
    if group_command {
        let group_id = meta.group_scope_id()?;
        return Some(MemoryCommandScope {
            scope_type: MemoryScopeType::Group,
            scope_id: group_id,
            label: "群",
            group_command: true,
        });
    }
    let user_id = meta.personal_scope_id()?;
    Some(MemoryCommandScope {
        scope_type: MemoryScopeType::Personal,
        scope_id: user_id,
        label: "个人",
        group_command: false,
    })
}

fn memory_command_targets_group(command: &ParsedCommand) -> bool {
    let argument = command.argument.trim_start();
    argument == "group"
        || argument == "群"
        || argument.starts_with("group ")
        || argument.starts_with("群 ")
}

pub(super) fn pending_memory_scope(
    memory: &PendingMemory,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    pending_scope(
        memory.target_scope_type.as_deref(),
        memory.target_scope_id.as_deref(),
        meta,
    )
}

pub(super) fn pending_update_scope(
    update: &PendingMemoryUpdate,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    pending_scope(
        update.target_scope_type.as_deref(),
        update.target_scope_id.as_deref(),
        meta,
    )
}

pub(super) fn pending_delete_scope(
    delete: &PendingMemoryDelete,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    pending_scope(
        delete.target_scope_type.as_deref(),
        delete.target_scope_id.as_deref(),
        meta,
    )
}

fn pending_scope(
    scope_type: Option<&str>,
    scope_id: Option<&str>,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    let scope_type = match scope_type? {
        "personal" => MemoryScopeType::Personal,
        "group" => MemoryScopeType::Group,
        _ => return None,
    };
    let scope_id = scope_id?.trim();
    if scope_id.is_empty() {
        return None;
    }
    match scope_type {
        MemoryScopeType::Personal if meta.personal_scope_id().as_deref() == Some(scope_id) => {
            Some(MemoryCommandScope {
                scope_type,
                scope_id: scope_id.to_owned(),
                label: "个人",
                group_command: false,
            })
        }
        MemoryScopeType::Group if meta.group_scope_id().as_deref() == Some(scope_id) => {
            Some(MemoryCommandScope {
                scope_type,
                scope_id: scope_id.to_owned(),
                label: "群",
                group_command: true,
            })
        }
        _ => None,
    }
}

pub(super) fn memory_actor(meta: &SessionMeta) -> Option<MemoryActor> {
    clean_string(meta.user_id.clone()?).map(|user_id| MemoryActor { user_id })
}

pub(super) fn resolve_memory_target(
    session: &mut SessionRecord,
    command_scope: &MemoryCommandScope,
    target: &str,
) -> MemoryTarget {
    let target = target.split_whitespace().next().unwrap_or("").trim();
    if target.chars().all(|ch| ch.is_ascii_digit())
        && let Ok(index) = target.parse::<usize>()
        && let Some(query) = valid_last_memory_query(session, command_scope)
        && let Some(id) = query
            .result_ids
            .get(index.saturating_sub(1))
            .filter(|_| index > 0)
    {
        return MemoryTarget::ResolvedId(id.clone());
    }
    // 与 Todo 保持一致：管理命令只接受最近列表中的序号。
    // 不再把短 ID 当目标，避免 UUID 前缀全数字时和列表序号产生歧义。
    MemoryTarget::MissingListIndex(target.to_owned())
}

fn valid_last_memory_query(
    session: &mut SessionRecord,
    command_scope: &MemoryCommandScope,
) -> Option<LastMemoryQuery> {
    let query = session.last_memory_query.clone()?;
    if !matches!(query.query_type.as_str(), "list" | "search") {
        return None;
    }
    // 旧会话快照没有 scope_type/scope_id。缺字段时强制重新列表，避免跨作用域复用序号。
    if query.scope_type.as_deref() != Some(command_scope.scope_type.as_str())
        || query.scope_id.as_deref() != Some(command_scope.scope_id.as_str())
    {
        session.last_memory_query = None;
        return None;
    }
    if !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS) {
        session.last_memory_query = None;
        return None;
    }
    Some(query)
}

pub(super) fn remember_memory_query(
    session: &mut SessionRecord,
    query_type: impl Into<String>,
    condition: impl Into<String>,
    command_scope: &MemoryCommandScope,
    records: &[MemoryRecord],
) {
    session.last_memory_query = Some(LastMemoryQuery {
        query_type: query_type.into(),
        condition: condition.into(),
        scope_type: Some(command_scope.scope_type.as_str().to_owned()),
        scope_id: Some(command_scope.scope_id.clone()),
        result_ids: records.iter().map(|record| record.id.clone()).collect(),
        created_at: now_iso_cn(),
    });
}
