//! Memory 领域请求、操作者上下文与结构化真实结果。

use super::storage::{
    MemoryCategory, MemoryRecord, MemorySourceType, MemoryTarget, MemoryVisibility,
};

/// 已由平台接入层归一化的操作者身份。
///
/// 权限判断只使用带平台和机器人账号命名空间的 personal/group scope；`user_id`
/// 仅用于兼容旧持久化字段，绝不能作为 v3 授权依据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryActor {
    pub user_id: String,
    pub personal_scope_id: String,
    pub group_scope_id: Option<String>,
    pub can_manage_group_memory: bool,
}

impl MemoryActor {
    pub(crate) fn from_context(
        user_id: Option<String>,
        personal_scope_id: Option<String>,
        group_scope_id: Option<String>,
        can_manage_group_memory: bool,
    ) -> Option<Self> {
        let user_id = clean_value(user_id?)?;
        let personal_scope_id = clean_value(personal_scope_id?)?;
        Some(Self {
            user_id,
            personal_scope_id,
            group_scope_id: group_scope_id.and_then(clean_value),
            can_manage_group_memory,
        })
    }
}

fn clean_value(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[derive(Debug, Clone)]
pub struct ReplaceScopedMemoryRequest {
    pub scope_type: super::storage::MemoryScopeType,
    pub scope_id: String,
    pub id_or_prefix: String,
    pub actor: MemoryActor,
    pub user_id: Option<String>,
    pub group_id: Option<String>,
    pub content: String,
    pub source_text: String,
    pub memory_type: String,
    pub scope: String,
}

/// v3 写入请求。target 决定权限范围，category/关系主体决定内容语义与冲突键。
#[derive(Debug, Clone)]
pub struct SaveMemoryRequest {
    pub actor: MemoryActor,
    pub target: MemoryTarget,
    pub content: String,
    pub source_text: String,
    pub category: MemoryCategory,
    pub legacy_scope: String,
    pub visibility: MemoryVisibility,
    pub source_type: MemorySourceType,
    pub source_ref: Option<String>,
    pub confirmed_at: Option<String>,
    pub pinned: bool,
    pub attribute_key: Option<String>,
    pub relation_subject_id: Option<String>,
    pub relation_object_id: Option<String>,
}

/// create/replace 的真实持久化结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryWriteResult {
    pub memory: MemoryRecord,
    pub archived_ids: Vec<String>,
}

/// archive/delete/clear 的真实影响范围。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryMutationResult {
    pub affected_ids: Vec<String>,
    pub count: usize,
}

impl MemoryMutationResult {
    pub(super) fn from_ids(affected_ids: Vec<String>) -> Self {
        Self {
            count: affected_ids.len(),
            affected_ids,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePreferenceResult {
    pub enabled: bool,
    pub archived_ids: Vec<String>,
}
