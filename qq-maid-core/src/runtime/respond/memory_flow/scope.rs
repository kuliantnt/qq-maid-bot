//! Memory 命名空间、稳定 target 与最近列表序号边界。

use crate::runtime::{
    command::ParsedCommand,
    respond::common::{LAST_QUERY_TTL_SECONDS, query_is_fresh},
    session::{LastMemoryQuery, SessionMeta, SessionRecord, now_iso_cn},
    tools::memory::{MemoryActor, MemoryKind, MemoryRecord, MemoryTarget, infer_group_memory_kind},
};

use super::command::{MemoryNamespace, memory_namespace};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MemoryTargetResolution {
    ResolvedId(String),
    MissingListIndex(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemoryCommandScope {
    pub(super) target: MemoryTarget,
    pub(super) label: &'static str,
    pub(super) command_prefix: &'static str,
}

impl MemoryCommandScope {
    pub(super) fn kind(&self) -> MemoryKind {
        self.target.memory_kind()
    }
}

pub(super) fn memory_command_scope(
    command: &ParsedCommand,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    let namespace = memory_namespace(command);
    let mut scope =
        memory_scope_for_namespace(namespace.unwrap_or(MemoryNamespace::Personal), meta)?;
    if namespace.is_none() {
        scope.command_prefix = "/memory";
    }
    Some(scope)
}

pub(super) fn memory_scope_for_namespace(
    namespace: MemoryNamespace,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    match namespace {
        MemoryNamespace::Personal => Some(MemoryCommandScope {
            target: MemoryTarget::personal(meta.personal_scope_id()?),
            label: "个人",
            command_prefix: "/memory personal",
        }),
        MemoryNamespace::GroupProfile => Some(MemoryCommandScope {
            target: MemoryTarget::group_profile(meta.group_scope_id()?, meta.personal_scope_id()?),
            label: "当前群画像",
            command_prefix: "/memory profile",
        }),
        MemoryNamespace::Group => Some(MemoryCommandScope {
            target: MemoryTarget::group(meta.group_scope_id()?),
            label: "当前群组",
            command_prefix: "/memory group",
        }),
    }
}

/// 群聊裸写入只在明显属于个人偏好、当前群画像或群公共语境时自动定域。
/// 不可靠时返回 None，由调用方保存结构化澄清 Pending。
pub(super) fn infer_group_memory_namespace(text: &str) -> Option<MemoryNamespace> {
    match infer_group_memory_kind(text)? {
        MemoryKind::Personal => Some(MemoryNamespace::Personal),
        MemoryKind::GroupProfile => Some(MemoryNamespace::GroupProfile),
        MemoryKind::Group => Some(MemoryNamespace::Group),
        MemoryKind::LegacyUnassigned => None,
    }
}

pub(super) fn memory_actor(
    meta: &SessionMeta,
    req: &crate::runtime::respond::RespondRequest,
) -> Option<MemoryActor> {
    MemoryActor::from_context(
        meta.user_id.clone(),
        meta.personal_scope_id(),
        meta.group_scope_id(),
        crate::runtime::group_role::group_management_allowed(
            meta.group_id.as_deref(),
            &meta.scope_key,
            req.group_member_role.as_deref(),
        ),
    )
}

pub(super) fn resolve_memory_target(
    session: &mut SessionRecord,
    command_scope: &MemoryCommandScope,
    actor: &MemoryActor,
    target: &str,
) -> MemoryTargetResolution {
    let target = target.split_whitespace().next().unwrap_or("").trim();
    if target.chars().all(|ch| ch.is_ascii_digit())
        && let Ok(index) = target.parse::<usize>()
        && let Some(query) = valid_last_memory_query(session, command_scope, actor)
        && let Some(id) = query
            .result_ids
            .get(index.saturating_sub(1))
            .filter(|_| index > 0)
    {
        return MemoryTargetResolution::ResolvedId(id.clone());
    }
    MemoryTargetResolution::MissingListIndex(target.to_owned())
}

fn valid_last_memory_query(
    session: &mut SessionRecord,
    command_scope: &MemoryCommandScope,
    actor: &MemoryActor,
) -> Option<LastMemoryQuery> {
    let query = session.last_memory_query.clone()?;
    if !matches!(query.query_type.as_str(), "list" | "search")
        || query.actor_id.as_deref() != Some(actor.personal_scope_id.as_str())
        || query.scope_type.as_deref() != Some(command_scope.target.scope_type().as_str())
        || query.scope_id.as_deref() != Some(command_scope.target.scope_id())
        || query.memory_kind.as_deref() != Some(command_scope.target.memory_kind().as_str())
        || query.subject_id.as_deref() != command_scope.target.subject_id()
        || !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS)
    {
        session.last_memory_query = None;
        return None;
    }
    Some(query)
}

pub(super) fn remember_memory_query(
    session: &mut SessionRecord,
    actor: &MemoryActor,
    query_type: impl Into<String>,
    condition: impl Into<String>,
    command_scope: &MemoryCommandScope,
    records: &[MemoryRecord],
) {
    session.last_memory_query = Some(LastMemoryQuery {
        actor_id: Some(actor.personal_scope_id.clone()),
        query_type: query_type.into(),
        condition: condition.into(),
        scope_type: Some(command_scope.target.scope_type().as_str().to_owned()),
        scope_id: Some(command_scope.target.scope_id().to_owned()),
        memory_kind: Some(command_scope.target.memory_kind().as_str().to_owned()),
        subject_id: command_scope.target.subject_id().map(str::to_owned),
        result_ids: records.iter().map(|record| record.id.clone()).collect(),
        created_at: now_iso_cn(),
    });
}
