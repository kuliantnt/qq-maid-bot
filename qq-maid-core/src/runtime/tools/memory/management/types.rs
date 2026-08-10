//! Memory 管理领域的安全输入、输出和内部确认状态。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serde::Serialize;

use super::super::storage::ManagementTargetSnapshot;
use super::super::{
    MemoryCategory, MemoryError, MemoryKind, MemoryRecord, MemoryStatus, MemoryStore, MemoryTarget,
    MemoryVisibility,
};

pub(super) const TARGET_REF_PREFIX: &str = "memory_target:v1:";
pub(super) const ACCOUNT_REF_PREFIX: &str = "memory_account:v1:";
pub(super) const GROUP_REF_PREFIX: &str = "memory_group:v1:";
pub(super) const SUBJECT_REF_PREFIX: &str = "memory_subject:v1:";
pub(super) const MEMORY_REF_PREFIX: &str = "memory:v1:";
pub(super) const CONFIRMATION_PREFIX: &str = "memory_confirmation:v1:";
pub(super) const CONFIRMATION_TTL_SECONDS: i64 = 5 * 60;
pub(super) const MAX_CONFIRMATIONS: usize = 1_024;
pub(super) const MAX_CONTENT_CHARS: usize = 16 * 1024;
pub(super) const MAX_KEYWORD_CHARS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManagementActor {
    pub(crate) admin_id: i64,
    pub(crate) session_digest: [u8; 32],
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct MemoryTargetSummary {
    pub(crate) target_ref: String,
    pub(crate) scope: String,
    pub(crate) platform: String,
    pub(crate) account_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) group_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subject_ref: Option<String>,
    /// 目标级操作能力；群画像的停用能力来自持久化 profile preference。
    pub(crate) capabilities: MemoryOperationCapabilities,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct MemoryCapabilities {
    pub(crate) can_update: bool,
    pub(crate) can_archive: bool,
    pub(crate) can_restore: bool,
    pub(crate) can_delete: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct MemoryManagementItem {
    pub(crate) memory_ref: String,
    pub(crate) version: u64,
    pub(crate) target: MemoryTargetSummary,
    pub(crate) content: String,
    pub(crate) kind: String,
    pub(crate) category: String,
    pub(crate) visibility: String,
    pub(crate) status: String,
    pub(crate) pinned: bool,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) last_confirmed_at: Option<String>,
    pub(crate) source_type: String,
    pub(crate) capabilities: MemoryCapabilities,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct MemoryManagementMutationResult {
    pub(crate) memory: MemoryManagementItem,
    pub(crate) archived_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct MemoryDeleteResult {
    pub(crate) memory_ref: String,
    pub(crate) deleted: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct MemoryTargetPage {
    pub(crate) items: Vec<MemoryTargetSummary>,
    pub(crate) total_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct MemoryManagementPage {
    pub(crate) items: Vec<MemoryManagementItem>,
    pub(crate) total_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PreparedMemoryOperation {
    pub(crate) confirmation_token: String,
    pub(crate) operation: String,
    pub(crate) target: MemoryTargetSummary,
    pub(crate) affected_count: usize,
    pub(crate) expires_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct MemoryOperationResult {
    pub(crate) operation: String,
    pub(crate) target: MemoryTargetSummary,
    pub(crate) affected_count: usize,
    pub(crate) capabilities: MemoryOperationCapabilities,
    /// 仅永久删除返回被删除的 opaque reference；不返回内部记录 ID 或正文。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) memory_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deleted: Option<bool>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct MemoryOperationCapabilities {
    pub(crate) can_clear_target: bool,
    pub(crate) can_disable_group_profile: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MemoryTargetFilter {
    pub(crate) scope: Option<MemoryKind>,
    pub(crate) platform: Option<String>,
    pub(crate) account_ref: Option<String>,
    pub(crate) group_ref: Option<String>,
    pub(crate) subject_ref: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MemoryListFilter {
    pub(crate) target_ref: Option<String>,
    pub(crate) scope: Option<MemoryKind>,
    pub(crate) platform: Option<String>,
    pub(crate) account_ref: Option<String>,
    pub(crate) group_ref: Option<String>,
    pub(crate) subject_ref: Option<String>,
    pub(crate) category: Option<MemoryCategory>,
    pub(crate) status: Option<MemoryStatus>,
    pub(crate) visibility: Option<MemoryVisibility>,
    pub(crate) pinned: Option<bool>,
    pub(crate) keyword: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryCreateInput {
    pub(crate) target_ref: String,
    pub(crate) content: String,
    pub(crate) category: MemoryCategory,
    pub(crate) visibility: MemoryVisibility,
    pub(crate) pinned: bool,
    pub(crate) attribute_key: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MemoryUpdatePatch {
    pub(crate) content: Option<String>,
    pub(crate) category: Option<MemoryCategory>,
    pub(crate) visibility: Option<MemoryVisibility>,
    pub(crate) pinned: Option<bool>,
    pub(crate) attribute_key: Option<Option<String>>,
}

impl MemoryUpdatePatch {
    pub(super) fn is_empty(&self) -> bool {
        self.content.is_none()
            && self.category.is_none()
            && self.visibility.is_none()
            && self.pinned.is_none()
            && self.attribute_key.is_none()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum MemoryManagementError {
    Validation(String),
    NotFound,
    Conflict(String),
    PermissionDenied,
    ProfileDisabled,
    AuditUnavailable,
    Internal,
}

impl MemoryManagementError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation_error",
            Self::NotFound => "not_found",
            Self::Conflict(_) => "conflict",
            Self::PermissionDenied => "permission_denied",
            Self::ProfileDisabled => "profile_disabled",
            Self::AuditUnavailable => "audit_unavailable",
            Self::Internal => "internal_error",
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Validation(message) | Self::Conflict(message) => message,
            Self::NotFound => "memory not found",
            Self::PermissionDenied => "memory management is not permitted",
            Self::ProfileDisabled => "group profile is disabled",
            Self::AuditUnavailable => "management audit is unavailable",
            Self::Internal => "memory management failed",
        }
    }
}

impl From<MemoryError> for MemoryManagementError {
    fn from(error: MemoryError) -> Self {
        match error.code() {
            "bad_request" => Self::Validation("invalid memory request".to_owned()),
            "not_found" => Self::NotFound,
            "memory_changed" => Self::Conflict("memory changed; refresh and retry".to_owned()),
            "forbidden" => Self::PermissionDenied,
            "profile_opted_out" => Self::ProfileDisabled,
            "management_audit_error" => Self::AuditUnavailable,
            _ => Self::Internal,
        }
    }
}

#[derive(Clone)]
pub(crate) struct MemoryManagementService {
    pub(super) store: MemoryStore,
    pub(super) confirmations: Arc<Mutex<HashMap<[u8; 32], ConfirmationEntry>>>,
}

#[derive(Debug, Clone)]
pub(super) struct ConfirmationEntry {
    pub(super) actor_id: i64,
    pub(super) session_digest: [u8; 32],
    pub(super) operation: ManagementOperation,
    pub(super) target_ref: String,
    pub(super) target: MemoryTarget,
    pub(super) snapshot: ManagementTargetSnapshot,
    /// 删除确认绑定完整记录快照，确保 commit 不能改用同 target 下的另一条记录。
    pub(super) memory_ref: Option<String>,
    pub(super) memory: Option<MemoryRecord>,
    pub(super) expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ManagementOperation {
    ClearTarget,
    DisableGroupProfile,
    DeleteMemory,
}

impl ManagementOperation {
    pub(super) fn parse(value: &str) -> Result<Self, MemoryManagementError> {
        match value.trim() {
            "clear_target" => Ok(Self::ClearTarget),
            "disable_group_profile" => Ok(Self::DisableGroupProfile),
            "delete_memory" => Ok(Self::DeleteMemory),
            _ => Err(MemoryManagementError::Validation(
                "operation is not supported".to_owned(),
            )),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::ClearTarget => "clear_target",
            Self::DisableGroupProfile => "disable_group_profile",
            Self::DeleteMemory => "delete_memory",
        }
    }
}
