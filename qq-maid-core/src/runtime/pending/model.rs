//! 跨工具可复用的 pending 基础设施。
//!
//! 本模块只保存通用 PreparedAction envelope、生命周期校验和确认/取消意图分类。
//! 具体业务 payload 与用户文案由各工具域维护。

use chrono::{DateTime, Duration};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedAction {
    schema_version: u32,
    domain: String,
    action_kind: String,
    state: PreparedActionState,
    initiator_user_id: Option<String>,
    owner_key: Option<String>,
    scope_key: Option<String>,
    created_at: String,
    expires_at: Option<String>,
    revision: u64,
    display_snapshot: Value,
    payload: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredPreparedAction {
    schema_version: u32,
    domain: String,
    action_kind: String,
    state: PreparedActionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initiator_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_key: Option<String>,
    scope_key: String,
    created_at: String,
    expires_at: String,
    revision: u64,
    #[serde(default)]
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
            scope_key: Some(metadata.scope_key),
            created_at: metadata.created_at,
            expires_at: Some(metadata.expires_at),
            revision: INITIAL_PREPARED_ACTION_REVISION,
            display_snapshot,
            payload,
        }
    }

    /// 从旧扁平业务 payload 构造兼容对象。
    ///
    /// 旧数据没有 scope/expires_at/revision，不能伪造这些字段。运行时只对明确支持的
    /// 旧 Todo JSON 沿用原 TTL 与 owner 规则；无法安全恢复的 payload 由业务域清理。
    fn from_legacy_payload(domain: impl Into<String>, payload: Value) -> Self {
        let domain = domain.into();
        Self {
            schema_version: 0,
            action_kind: string_field(&payload, "kind").unwrap_or_else(|| domain.clone()),
            state: PreparedActionState::WaitingConfirmation,
            initiator_user_id: string_field(&payload, "initiator_user_id"),
            owner_key: string_field(&payload, "owner_key"),
            scope_key: None,
            created_at: string_field(&payload, "created_at").unwrap_or_default(),
            expires_at: None,
            revision: INITIAL_PREPARED_ACTION_REVISION,
            display_snapshot: Value::Null,
            domain,
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

    pub fn scope_key(&self) -> Option<&str> {
        self.scope_key.as_deref()
    }

    pub fn initiator_user_id(&self) -> Option<&str> {
        self.initiator_user_id.as_deref()
    }

    pub fn created_at(&self) -> &str {
        &self.created_at
    }

    pub fn expires_at(&self) -> Option<&str> {
        self.expires_at.as_deref()
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

    pub fn is_legacy(&self) -> bool {
        self.schema_version == 0
    }

    /// 新结构以 expires_at 为唯一过期依据；旧结构由业务域继续使用原 created_at TTL。
    pub fn is_expired_at(&self, now: &str) -> bool {
        let (Some(expires_at), Ok(now)) = (
            self.expires_at.as_deref(),
            DateTime::parse_from_rfc3339(now.trim()),
        ) else {
            return !self.is_legacy();
        };
        let Ok(expires_at) = DateTime::parse_from_rfc3339(expires_at.trim()) else {
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
        self.expires_at = Some(expires_at.into());
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
        self.expires_at = Some(expires_at.into());
        self.revision = self.revision.saturating_add(1);
        self.state = PreparedActionState::WaitingConfirmation;
        Ok(self.revision)
    }

    pub fn validate_for_execution(
        &self,
        context: &PreparedActionExecutionContext<'_>,
    ) -> Result<(), PreparedActionValidationError> {
        if self.schema_version != PREPARED_ACTION_SCHEMA_VERSION {
            return Err(PreparedActionValidationError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.state != PreparedActionState::WaitingConfirmation {
            return Err(PreparedActionValidationError::InvalidState);
        }
        if self.revision != context.expected_revision {
            return Err(PreparedActionValidationError::RevisionMismatch);
        }
        let (Some(initiator), Some(scope_key), Some(expires_at)) = (
            self.initiator_user_id.as_deref(),
            self.scope_key.as_deref(),
            self.expires_at.as_deref(),
        ) else {
            return Err(PreparedActionValidationError::MissingMetadata);
        };
        if context.initiator_user_id != Some(initiator) {
            return Err(PreparedActionValidationError::InitiatorMismatch);
        }
        if self.owner_key.as_deref() != context.owner_key {
            return Err(PreparedActionValidationError::OwnerMismatch);
        }
        if scope_key != context.scope_key {
            return Err(PreparedActionValidationError::ScopeMismatch);
        }
        let Ok(now) = DateTime::parse_from_rfc3339(context.now.trim()) else {
            return Err(PreparedActionValidationError::Expired);
        };
        let Ok(expires_at) = DateTime::parse_from_rfc3339(expires_at.trim()) else {
            return Err(PreparedActionValidationError::Expired);
        };
        if now >= expires_at {
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

impl Serialize for PreparedAction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 旧对象只来自既有 Session 或兼容测试；未发生修订时保持原扁平 JSON，避免
        // 无意把缺少可信 scope/expires_at 的数据包装成看似完整的新动作。
        if self.is_legacy() {
            return self.payload.serialize(serializer);
        }
        StoredPreparedAction {
            schema_version: self.schema_version,
            domain: self.domain.clone(),
            action_kind: self.action_kind.clone(),
            state: self.state,
            initiator_user_id: self.initiator_user_id.clone(),
            owner_key: self.owner_key.clone(),
            scope_key: self.scope_key.clone().unwrap_or_default(),
            created_at: self.created_at.clone(),
            expires_at: self.expires_at.clone().unwrap_or_default(),
            revision: self.revision,
            display_snapshot: self.display_snapshot.clone(),
            payload: self.payload.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PreparedAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        if value.get("schema_version").is_none() {
            let kind = string_field(&value, "kind").unwrap_or_else(|| "unknown".to_owned());
            let domain = domain_from_kind(&kind);
            if !SUPPORTED_PENDING_DOMAINS.contains(&domain.as_str()) {
                return Err(D::Error::custom(format!(
                    "unsupported pending operation kind `{kind}`"
                )));
            }
            return Ok(Self::from_legacy_payload(domain, value));
        }

        let stored: StoredPreparedAction =
            serde_json::from_value(value).map_err(D::Error::custom)?;
        if !SUPPORTED_PENDING_DOMAINS.contains(&stored.domain.as_str()) {
            return Err(D::Error::custom(format!(
                "unsupported prepared action domain `{}`",
                stored.domain
            )));
        }
        if stored.schema_version != PREPARED_ACTION_SCHEMA_VERSION {
            return Err(D::Error::custom(format!(
                "unsupported prepared action schema version `{}`",
                stored.schema_version
            )));
        }
        if stored.revision == 0
            || stored.scope_key.trim().is_empty()
            || stored.created_at.trim().is_empty()
            || stored.expires_at.trim().is_empty()
            || stored.action_kind.trim().is_empty()
        {
            return Err(D::Error::custom(
                "prepared action is missing required lifecycle metadata",
            ));
        }
        Ok(Self {
            schema_version: stored.schema_version,
            domain: stored.domain,
            action_kind: stored.action_kind,
            state: stored.state,
            initiator_user_id: stored.initiator_user_id,
            owner_key: stored.owner_key,
            scope_key: Some(stored.scope_key),
            created_at: stored.created_at,
            expires_at: Some(stored.expires_at),
            revision: stored.revision,
            display_snapshot: stored.display_snapshot,
            payload: stored.payload,
        })
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn domain_from_kind(kind: &str) -> String {
    kind.split_once('_')
        .map(|(domain, _)| domain)
        .filter(|domain| !domain.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}
