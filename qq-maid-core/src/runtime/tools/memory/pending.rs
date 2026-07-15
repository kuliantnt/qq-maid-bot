//! Memory 专属 PreparedAction payload、草稿快照与确认词表。
//!
//! 通用 pending 只维护 actor、作用域和生命周期；目标范围、固定对象、可见性和
//! 用户可见文案所需字段都保留在 Memory 领域，模型输出不能直接决定权限边界。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::{
    pending::{PendingLexicon, PreparedAction, PreparedActionMetadata, expires_at_after},
    session::{LAST_QUERY_TTL_SECONDS, now_iso_cn},
};

use super::{
    SaveMemoryRequest,
    storage::{MemoryCategory, MemoryKind, MemorySourceType, MemoryTarget, MemoryVisibility},
};

pub(crate) const MEMORY_PENDING_DOMAIN: &str = "memory";

/// 准备阶段固化的分域记忆草稿；当前用于修改确认和旧 Save Pending 兼容。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedMemoryDraft {
    pub target: MemoryTarget,
    pub visibility: MemoryVisibility,
    pub category: MemoryCategory,
    pub content: String,
    pub source_text: String,
    pub source_summary: String,
    pub change_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
}

impl PreparedMemoryDraft {
    pub(crate) fn kind(&self) -> MemoryKind {
        self.target.memory_kind()
    }

    pub(crate) fn source_type(&self) -> MemorySourceType {
        MemorySourceType::UserConfirmed
    }

    pub(crate) fn into_save_request(self, actor: super::MemoryActor) -> SaveMemoryRequest {
        let source_type = self.source_type();
        SaveMemoryRequest {
            actor,
            target: self.target,
            content: self.content,
            source_text: self.source_text,
            category: self.category,
            legacy_scope: "general".to_owned(),
            visibility: self.visibility,
            source_type,
            source_ref: self.source_ref,
            confirmed_at: Some(now_iso_cn()),
            pinned: false,
            attribute_key: self.attribute_key,
            relation_subject_id: None,
            relation_object_id: None,
        }
    }
}

/// Memory 业务 pending payload。内部 ID 只用于确认后的服务端复核，绝不展示给用户。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryPendingPayload {
    Save {
        initiator_user_id: String,
        owner_key: String,
        draft: PreparedMemoryDraft,
        created_at: String,
    },
    ClarifyScope {
        initiator_user_id: String,
        owner_key: String,
        normalized_content: String,
        source_text: String,
        source_ref: Option<String>,
        created_at: String,
    },
    Replace {
        initiator_user_id: String,
        owner_key: String,
        record_id: String,
        /// 旧字段仅用于兼容已持久化的 PreparedAction；新确认必须使用完整记录快照 CAS。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_updated_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_record: Option<Box<super::MemoryRecord>>,
        draft: PreparedMemoryDraft,
        created_at: String,
    },
    Delete {
        initiator_user_id: String,
        owner_key: String,
        target: MemoryTarget,
        record_id: String,
        /// 旧字段仅用于兼容已持久化的 PreparedAction；新确认必须使用完整记录快照 CAS。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_updated_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_record: Option<Box<super::MemoryRecord>>,
        content_snapshot: String,
        created_at: String,
    },
    Clear {
        initiator_user_id: String,
        owner_key: String,
        target: MemoryTarget,
        record_ids: Vec<String>,
        scope_label: String,
        created_at: String,
    },
    SetProfileEnabled {
        initiator_user_id: String,
        owner_key: String,
        target: MemoryTarget,
        enabled: bool,
        expected_enabled: bool,
        record_ids: Vec<String>,
        created_at: String,
    },
}

impl MemoryPendingPayload {
    pub(crate) fn try_from_pending(
        pending: &PreparedAction,
    ) -> Result<Option<Self>, serde_json::Error> {
        if pending.domain() != MEMORY_PENDING_DOMAIN {
            return Ok(None);
        }
        serde_json::from_value(pending.payload().clone()).map(Some)
    }

    pub(crate) fn into_prepared_action(self, scope_key: &str) -> PreparedAction {
        let display_snapshot = self.display_snapshot();
        let payload = serde_json::to_value(&self)
            .expect("MemoryPendingPayload serialization should not fail");
        let action_kind = payload
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("memory_unknown")
            .to_owned();
        let created_at = payload
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let expires_at = expires_at_after(&created_at, LAST_QUERY_TTL_SECONDS)
            .unwrap_or_else(|| created_at.clone());
        let initiator_user_id = payload
            .get("initiator_user_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let owner_key = payload
            .get("owner_key")
            .and_then(Value::as_str)
            .map(str::to_owned);
        PreparedAction::new(
            PreparedActionMetadata {
                domain: MEMORY_PENDING_DOMAIN.to_owned(),
                action_kind,
                initiator_user_id,
                owner_key,
                scope_key: scope_key.to_owned(),
                created_at,
                expires_at,
            },
            display_snapshot,
            payload,
        )
    }

    fn display_snapshot(&self) -> Value {
        match self {
            Self::Save { draft, .. } | Self::Replace { draft, .. } => serde_json::json!({
                "memory_kind": draft.kind().as_str(),
                "visibility": draft.visibility.as_str(),
                "category": draft.category.as_str(),
                "content": draft.content,
                "source_summary": draft.source_summary,
                "change_type": draft.change_type,
            }),
            Self::ClarifyScope {
                normalized_content, ..
            } => serde_json::json!({
                "content": normalized_content,
                "choices": ["personal", "group_profile"],
            }),
            Self::Delete {
                content_snapshot, ..
            } => serde_json::json!({"content": content_snapshot}),
            Self::Clear {
                record_ids,
                scope_label,
                ..
            } => serde_json::json!({
                "count": record_ids.len(),
                "scope": scope_label,
            }),
            Self::SetProfileEnabled {
                enabled,
                record_ids,
                ..
            } => serde_json::json!({
                "enabled": enabled,
                "affected_count": record_ids.len(),
            }),
        }
    }
}

pub(crate) fn memory_lexicon() -> PendingLexicon {
    PendingLexicon::new(
        &["确认", "可以", "好", "好的", "保存", "写入", "执行", "记吧"],
        &["取消", "不记", "不要", "算了", "不用", "放弃"],
    )
}

pub(crate) fn draft_confirmation_text(draft: &PreparedMemoryDraft) -> String {
    let scope = match draft.target.memory_kind() {
        MemoryKind::Personal => "个人记忆",
        MemoryKind::GroupProfile => "当前群画像",
        MemoryKind::Group => "当前群组记忆",
        MemoryKind::LegacyUnassigned => "未归属旧记忆",
    };
    let subject = match draft.target.memory_kind() {
        MemoryKind::Personal => "当前用户",
        MemoryKind::GroupProfile => "当前用户在本群的画像",
        MemoryKind::Group => "当前群组",
        MemoryKind::LegacyUnassigned => "未归属主体",
    };
    format!(
        "已整理记忆草稿：\n- 目标范围：{scope}\n- 主体：{subject}\n- 可见性：{}\n- 类型：{}\n- 规范化内容：{}\n- 来源摘要：{}\n- 预期变更：{}\n回复“确认 / 可以 / 记吧”执行真实写入；回复“取消 / 不记 / 算了”放弃。",
        draft.visibility.as_str(),
        draft.category.as_str(),
        draft.content,
        draft.source_summary,
        draft.change_type,
    )
}
