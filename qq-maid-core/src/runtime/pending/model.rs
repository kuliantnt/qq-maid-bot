//! 跨工具可复用的 pending 基础设施。
//!
//! 本模块只保存通用 PreparedAction envelope、生命周期校验和确认/取消意图分类。
//! 具体业务 payload 与用户文案由各工具域维护。

use chrono::{DateTime, Duration};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// 当前 PreparedAction 持久化结构版本。
pub const PREPARED_ACTION_SCHEMA_VERSION: u32 = 1;
/// 新动作的初始 revision。revision 只增不减，后续修订必须递增。
pub const INITIAL_PREPARED_ACTION_REVISION: u64 = 1;

const SUPPORTED_PENDING_DOMAINS: &[&str] = &["todo"];

/// 需要持久化的非终态生命周期。
///
/// Completed、Cancelled、Expired 是处理结果，完成后继续清除 Session pending，
/// 不在这里保存历史记录。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreparedActionState {
    /// 已准备完毕，尚未执行任何副作用。
    WaitingConfirmation,
    /// 已通过身份、作用域、过期时间和 revision 校验，正在执行真实工具。
    Executing,
    /// 真实执行返回失败；是否保留以便用户取消由业务域决定。
    Failed,
}

/// 执行前由可信运行时提供的身份与作用域上下文。
#[derive(Debug, Clone, Copy)]
pub struct PreparedActionExecutionContext<'a> {
    pub initiator_user_id: Option<&'a str>,
    pub owner_key: Option<&'a str>,
    pub scope_key: &'a str,
    pub expected_revision: u64,
    pub now: &'a str,
}

/// 创建 PreparedAction 时由业务域提供的通用元数据。
#[derive(Debug, Clone)]
pub struct PreparedActionMetadata {
    pub domain: String,
    pub action_kind: String,
    pub initiator_user_id: Option<String>,
    pub owner_key: Option<String>,
    pub scope_key: String,
    pub created_at: String,
    pub expires_at: String,
}

/// PreparedAction 在执行前校验失败的明确原因。
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PreparedActionValidationError {
    #[error("unsupported prepared action schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("prepared action is not waiting for confirmation")]
    InvalidState,
    #[error("prepared action initiator does not match current user")]
    InitiatorMismatch,
    #[error("prepared action owner does not match current owner")]
    OwnerMismatch,
    #[error("prepared action scope does not match current session")]
    ScopeMismatch,
    #[error("prepared action has expired or contains an invalid expiry")]
    Expired,
    #[error("prepared action revision does not match expected revision")]
    RevisionMismatch,
    #[error("prepared action is missing required execution metadata")]
    MissingMetadata,
}

/// 持久化 pending 的通用 PreparedAction envelope。
///
/// 通用层只理解动作身份、生命周期和不透明 JSON；Todo ID、提醒等业务字段仍由
/// `runtime::tools::todo` 解释。Session 只保存这一套 pending 状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedAction {
    schema_version: u32,
    domain: String,
    action_kind: String,
    state: PreparedActionState,
    initiator_user_id: Option<String>,
    owner_key: Option<String>,
    scope_key: String,
    created_at: String,
    expires_at: String,
    revision: u64,
    display_snapshot: Value,
    payload: Value,
}

impl PreparedAction {
    /// 构造新 PreparedAction。调用方必须传入可信的 session scope 与明确过期时间。
    pub fn new(metadata: PreparedActionMetadata, display_snapshot: Value, payload: Value) -> Self {
        Self {
            schema_version: PREPARED_ACTION_SCHEMA_VERSION,
            domain: metadata.domain,
            action_kind: metadata.action_kind,
            state: PreparedActionState::WaitingConfirmation,
            initiator_user_id: metadata.initiator_user_id,
            owner_key: metadata.owner_key,
            scope_key: metadata.scope_key,
            created_at: metadata.created_at,
            expires_at: metadata.expires_at,
            revision: INITIAL_PREPARED_ACTION_REVISION,
            display_snapshot,
            payload,
        }
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// pending 所属业务域，例如 `todo`。
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// 业务动作类型，例如 `todo_bulk_delete`。
    pub fn kind(&self) -> &str {
        &self.action_kind
    }

    pub fn state(&self) -> PreparedActionState {
        self.state
    }

    pub fn owner_key(&self) -> Option<&str> {
        self.owner_key.as_deref()
    }

    pub fn scope_key(&self) -> &str {
        &self.scope_key
    }

    pub fn initiator_user_id(&self) -> Option<&str> {
        self.initiator_user_id.as_deref()
    }

    pub fn created_at(&self) -> &str {
        &self.created_at
    }

    pub fn expires_at(&self) -> &str {
        &self.expires_at
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn display_snapshot(&self) -> &Value {
        &self.display_snapshot
    }

    /// 业务原始 payload。只允许对应工具域继续解释。
    pub fn payload(&self) -> &Value {
        &self.payload
    }

    /// 以 envelope 中固化的 expires_at 为唯一过期依据；非法时间按过期处理。
    pub fn is_expired_at(&self, now: &str) -> bool {
        let Ok(now) = DateTime::parse_from_rfc3339(now.trim()) else {
            return true;
        };
        let Ok(expires_at) = DateTime::parse_from_rfc3339(self.expires_at.trim()) else {
            return true;
        };
        now >= expires_at
    }

    /// 校验并切换到 Executing。调用方必须先持久化该状态，再执行副作用。
    pub fn begin_execution(
        &mut self,
        context: &PreparedActionExecutionContext<'_>,
    ) -> Result<(), PreparedActionValidationError> {
        self.validate_for_execution(context)?;
        self.state = PreparedActionState::Executing;
        Ok(())
    }

    /// 将真实执行失败记录为 Failed，不生成成功文案。
    pub fn mark_failed(&mut self, revision: u64) -> Result<(), PreparedActionValidationError> {
        if self.revision != revision {
            return Err(PreparedActionValidationError::RevisionMismatch);
        }
        if self.state != PreparedActionState::Executing {
            return Err(PreparedActionValidationError::InvalidState);
        }
        self.state = PreparedActionState::Failed;
        Ok(())
    }

    /// 为后续 #214 提供结构化修订能力；本任务不解析自然语言修订。
    ///
    /// revision 递增后，持有旧 revision 的确认会在 `begin_execution` 阶段失效。
    pub fn revise(
        &mut self,
        payload: Value,
        display_snapshot: Value,
        expires_at: impl Into<String>,
    ) -> Result<u64, PreparedActionValidationError> {
        if !matches!(
            self.state,
            PreparedActionState::WaitingConfirmation | PreparedActionState::Failed
        ) {
            return Err(PreparedActionValidationError::InvalidState);
        }
        self.payload = payload;
        self.display_snapshot = display_snapshot;
        self.expires_at = expires_at.into();
        self.revision = self.revision.saturating_add(1);
        self.state = PreparedActionState::WaitingConfirmation;
        Ok(self.revision)
    }

    /// 工具已经取得执行权但返回“仍需补充”时，保存新的展示问题并回到等待态。
    pub fn continue_waiting_after_execution(
        &mut self,
        payload: Value,
        display_snapshot: Value,
        expires_at: impl Into<String>,
    ) -> Result<u64, PreparedActionValidationError> {
        if self.state != PreparedActionState::Executing {
            return Err(PreparedActionValidationError::InvalidState);
        }
        self.payload = payload;
        self.display_snapshot = display_snapshot;
        self.expires_at = expires_at.into();
        self.revision = self.revision.saturating_add(1);
        self.state = PreparedActionState::WaitingConfirmation;
        Ok(self.revision)
    }

    pub fn validate_for_execution(
        &self,
        context: &PreparedActionExecutionContext<'_>,
    ) -> Result<(), PreparedActionValidationError> {
        self.validate_envelope()?;
        if self.state != PreparedActionState::WaitingConfirmation {
            return Err(PreparedActionValidationError::InvalidState);
        }
        if self.revision != context.expected_revision {
            return Err(PreparedActionValidationError::RevisionMismatch);
        }
        let Some(initiator) = self.initiator_user_id.as_deref() else {
            return Err(PreparedActionValidationError::MissingMetadata);
        };
        if context.initiator_user_id != Some(initiator) {
            return Err(PreparedActionValidationError::InitiatorMismatch);
        }
        if self.owner_key.as_deref() != context.owner_key {
            return Err(PreparedActionValidationError::OwnerMismatch);
        }
        if self.scope_key != context.scope_key {
            return Err(PreparedActionValidationError::ScopeMismatch);
        }
        let Ok(now) = DateTime::parse_from_rfc3339(context.now.trim()) else {
            return Err(PreparedActionValidationError::Expired);
        };
        let Ok(expires_at) = DateTime::parse_from_rfc3339(self.expires_at.trim()) else {
            return Err(PreparedActionValidationError::Expired);
        };
        if now >= expires_at {
            return Err(PreparedActionValidationError::Expired);
        }
        Ok(())
    }

    /// 校验从 Session 读取的 envelope 是否为当前支持的完整结构。
    ///
    /// 升级前的扁平 Pending 会在 serde 阶段失败；版本不支持或字段不完整的 envelope
    /// 同样由 Session 层直接清理，不进入具体业务域。
    pub fn validate_envelope(&self) -> Result<(), PreparedActionValidationError> {
        if self.schema_version != PREPARED_ACTION_SCHEMA_VERSION {
            return Err(PreparedActionValidationError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if !SUPPORTED_PENDING_DOMAINS.contains(&self.domain.as_str())
            || self.revision == 0
            || self.scope_key.trim().is_empty()
            || self.created_at.trim().is_empty()
            || self.expires_at.trim().is_empty()
            || self.action_kind.trim().is_empty()
        {
            return Err(PreparedActionValidationError::MissingMetadata);
        }
        if DateTime::parse_from_rfc3339(self.created_at.trim()).is_err()
            || DateTime::parse_from_rfc3339(self.expires_at.trim()).is_err()
        {
            return Err(PreparedActionValidationError::Expired);
        }
        Ok(())
    }
}

/// 按 created_at 计算明确的 RFC3339 过期时间；非法时间返回 None，由调用方拒绝创建。
pub fn expires_at_after(created_at: &str, ttl_seconds: i64) -> Option<String> {
    let created_at = DateTime::parse_from_rfc3339(created_at.trim()).ok()?;
    Some((created_at + Duration::seconds(ttl_seconds)).to_rfc3339())
}
