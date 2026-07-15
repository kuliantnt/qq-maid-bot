//! Memory storage 持久化类型与强类型范围模型。

use serde::{Deserialize, Serialize};

use super::{
    MemoryError,
    clean::{
        clean_optional_option, clean_scope_id, default_memory_type, default_scope,
        legacy_unassigned_scope_type,
    },
};

/// 记忆范围；它与访问边界 `MemoryScopeType` 分离。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Personal,
    GroupProfile,
    Group,
    LegacyUnassigned,
}

/// 记忆内容类别。v3 调用层使用强类型，旧字符串字段仅保留数据兼容。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    Note,
    Preference,
    Identity,
    Relation,
    Instruction,
}

/// 记忆可见性。召回时由 Memory 领域层按当前场景转换为允许集合。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryVisibility {
    Private,
    ContextOnly,
    GroupMembers,
    Public,
}

/// 记忆生命周期状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Active,
    Archived,
}

/// 记忆来源类型；`source_ref` 只保存调用层提供的安全引用，不保存消息正文。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySourceType {
    UserConfirmed,
    ManualImport,
    SystemDerived,
    Legacy,
}

/// 记忆记录，兼容旧字段并提供 v3 范围、可见性、来源与生命周期字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub ts: String,
    #[serde(rename = "createdAt", default)]
    pub created_at: String,
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(
        rename = "type",
        alias = "memory_type",
        default = "default_memory_type"
    )]
    pub memory_type: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    /// 真正的访问边界类型：personal / group / legacy_unassigned。
    #[serde(default = "legacy_unassigned_scope_type")]
    pub scope_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_user_id: Option<String>,
    #[serde(default = "legacy_unassigned_memory_kind")]
    pub memory_kind: MemoryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    /// 关系记忆的稳定主语和宾语；关系沿用当前 target/visibility，不另建权限域。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation_subject_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation_object_id: Option<String>,
    #[serde(default = "private_memory_visibility")]
    pub visibility: MemoryVisibility,
    #[serde(default = "legacy_memory_source_type")]
    pub source_type: MemorySourceType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_confirmed_at: Option<String>,
    #[serde(default = "archived_memory_status")]
    pub status: MemoryStatus,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub source_text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateMemoryRequest {
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub source_text: String,
    #[serde(
        rename = "type",
        alias = "memory_type",
        default = "default_memory_type"
    )]
    pub memory_type: String,
    #[serde(default = "default_scope")]
    pub scope: String,
}

/// 旧 scoped 创建入口，只用于兼容现有 Memory respond flow。
#[derive(Debug, Clone)]
pub struct CreateScopedMemoryRequest {
    pub scope_type: MemoryScopeType,
    pub scope_id: String,
    pub created_by_user_id: String,
    pub user_id: Option<String>,
    pub group_id: Option<String>,
    pub content: String,
    pub source_text: String,
    pub memory_type: String,
    pub scope: String,
}

/// 旧 scoped 入口的底层原子替换请求；领域层先完成权限校验。
#[derive(Debug, Clone)]
pub(crate) struct ReplaceScopedStorageRequest {
    pub scope_type: MemoryScopeType,
    pub scope_id: String,
    pub id_or_prefix: String,
    pub created_by_user_id: String,
    pub user_id: Option<String>,
    pub group_id: Option<String>,
    pub content: String,
    pub source_text: String,
    pub memory_type: String,
    pub scope: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateMemoryRequest {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub source_text: Option<String>,
    #[serde(rename = "type", alias = "memory_type", default)]
    pub memory_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl UpdateMemoryRequest {
    pub(super) fn has_update(&self) -> bool {
        self.content.is_some()
            || self.source_text.is_some()
            || self.memory_type.is_some()
            || self.scope.is_some()
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ListMemoryQuery {
    pub limit: Option<usize>,
    pub q: Option<String>,
    pub scope: Option<String>,
    #[serde(rename = "type", alias = "memory_type")]
    pub memory_type: Option<String>,
    pub user_id: Option<String>,
    pub group_id: Option<String>,
}

impl ListMemoryQuery {
    #[cfg(test)]
    pub(super) fn limit(&self) -> usize {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
}

/// 长期记忆访问边界。不要和业务分类字段 `MemoryRecord::scope` 混用。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScopeType {
    Personal,
    Group,
    LegacyUnassigned,
}

/// v3 精确目标：访问边界、记忆范围与可选画像主体共同组成隔离键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryTarget {
    pub(super) scope_type: MemoryScopeType,
    pub(super) scope_id: String,
    pub(super) memory_kind: MemoryKind,
    pub(super) subject_id: Option<String>,
}

impl MemoryTarget {
    pub fn personal(scope_id: impl Into<String>) -> Self {
        Self {
            scope_type: MemoryScopeType::Personal,
            scope_id: scope_id.into(),
            memory_kind: MemoryKind::Personal,
            subject_id: None,
        }
    }

    pub fn group_profile(group_scope_id: impl Into<String>, subject_id: impl Into<String>) -> Self {
        Self {
            scope_type: MemoryScopeType::Group,
            scope_id: group_scope_id.into(),
            memory_kind: MemoryKind::GroupProfile,
            subject_id: Some(subject_id.into()),
        }
    }

    pub fn group(scope_id: impl Into<String>) -> Self {
        Self {
            scope_type: MemoryScopeType::Group,
            scope_id: scope_id.into(),
            memory_kind: MemoryKind::Group,
            subject_id: None,
        }
    }

    pub fn scope_type(&self) -> MemoryScopeType {
        self.scope_type
    }

    pub fn scope_id(&self) -> &str {
        &self.scope_id
    }

    pub fn memory_kind(&self) -> MemoryKind {
        self.memory_kind
    }

    pub fn subject_id(&self) -> Option<&str> {
        self.subject_id.as_deref()
    }

    pub(super) fn clean(&self) -> Result<Self, MemoryError> {
        let scope_id = clean_scope_id(&self.scope_id)?;
        let subject_id = clean_optional_option(self.subject_id.clone());
        let valid = matches!(
            (self.scope_type, self.memory_kind, subject_id.as_deref()),
            (MemoryScopeType::Personal, MemoryKind::Personal, None)
                | (MemoryScopeType::Group, MemoryKind::GroupProfile, Some(_))
                | (MemoryScopeType::Group, MemoryKind::Group, None)
                | (
                    MemoryScopeType::LegacyUnassigned,
                    MemoryKind::LegacyUnassigned,
                    None
                )
        );
        if !valid {
            return Err(MemoryError::bad_request("invalid memory target"));
        }
        Ok(Self {
            scope_type: self.scope_type,
            scope_id,
            memory_kind: self.memory_kind,
            subject_id,
        })
    }
}

/// v3 存储查询条件。目标必填，其他筛选使用枚举或结构化字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryQuery {
    pub target: MemoryTarget,
    pub status: Option<MemoryStatus>,
    pub category: Option<MemoryCategory>,
    pub visibility: Option<MemoryVisibility>,
    pub pinned: Option<bool>,
    pub attribute_key: Option<String>,
    pub relation_subject_id: Option<String>,
    pub relation_object_id: Option<String>,
    pub limit: Option<usize>,
}

impl MemoryQuery {
    pub fn active(target: MemoryTarget) -> Self {
        Self {
            target,
            status: Some(MemoryStatus::Active),
            category: None,
            visibility: None,
            pinned: None,
            attribute_key: None,
            relation_subject_id: None,
            relation_object_id: None,
            limit: None,
        }
    }

    pub(super) fn limit(&self) -> usize {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
}

/// storage 接收的 v3 请求；权限与可见性组合由领域层校验。
#[derive(Debug, Clone)]
pub(crate) struct PersistMemoryRequest {
    pub target: MemoryTarget,
    pub created_by_user_id: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistMemoryResult {
    pub record: MemoryRecord,
    pub archived_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedMemoryQuery {
    pub scope_type: MemoryScopeType,
    pub scope_id: String,
    pub limit: Option<usize>,
    pub q: Option<String>,
    pub scope: Option<String>,
    pub memory_type: Option<String>,
}

impl ScopedMemoryQuery {
    pub(super) fn limit(&self) -> usize {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
}

impl MemoryScopeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Group => "group",
            Self::LegacyUnassigned => "legacy_unassigned",
        }
    }
}

impl std::str::FromStr for MemoryScopeType {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "personal" => Ok(Self::Personal),
            "group" => Ok(Self::Group),
            "legacy_unassigned" => Ok(Self::LegacyUnassigned),
            _ => Err(()),
        }
    }
}

macro_rules! impl_memory_enum_text {
    ($ty:ty, {$($variant:path => $value:literal),+ $(,)?}) => {
        impl $ty {
            pub fn as_str(self) -> &'static str {
                match self {
                    $($variant => $value,)+
                }
            }
        }

        impl std::str::FromStr for $ty {
            type Err = ();

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok($variant),)+
                    _ => Err(()),
                }
            }
        }

        impl rusqlite::types::FromSql for $ty {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let value = value.as_str()?;
                value.parse().map_err(|_| {
                    rusqlite::types::FromSqlError::Other(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid {} value `{value}`", stringify!($ty)),
                    )))
                })
            }
        }
    };
}

impl_memory_enum_text!(MemoryKind, {
    MemoryKind::Personal => "personal",
    MemoryKind::GroupProfile => "group_profile",
    MemoryKind::Group => "group",
    MemoryKind::LegacyUnassigned => "legacy_unassigned",
});
impl_memory_enum_text!(MemoryCategory, {
    MemoryCategory::Note => "note",
    MemoryCategory::Preference => "preference",
    MemoryCategory::Identity => "identity",
    MemoryCategory::Relation => "relation",
    MemoryCategory::Instruction => "instruction",
});
impl_memory_enum_text!(MemoryVisibility, {
    MemoryVisibility::Private => "private",
    MemoryVisibility::ContextOnly => "context_only",
    MemoryVisibility::GroupMembers => "group_members",
    MemoryVisibility::Public => "public",
});
impl_memory_enum_text!(MemoryStatus, {
    MemoryStatus::Active => "active",
    MemoryStatus::Archived => "archived",
});
impl_memory_enum_text!(MemorySourceType, {
    MemorySourceType::UserConfirmed => "user_confirmed",
    MemorySourceType::ManualImport => "manual_import",
    MemorySourceType::SystemDerived => "system_derived",
    MemorySourceType::Legacy => "legacy",
});

fn legacy_unassigned_memory_kind() -> MemoryKind {
    MemoryKind::LegacyUnassigned
}

fn private_memory_visibility() -> MemoryVisibility {
    MemoryVisibility::Private
}

fn legacy_memory_source_type() -> MemorySourceType {
    MemorySourceType::Legacy
}

fn archived_memory_status() -> MemoryStatus {
    MemoryStatus::Archived
}
