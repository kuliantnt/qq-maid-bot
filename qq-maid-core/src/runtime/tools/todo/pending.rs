//! Todo 专属 pending payload 与确认词表。
//!
//! `runtime::pending::PreparedAction` 只负责通用 envelope；本模块维护 Todo 的
//! 持久化 payload、澄清候选边界和 Todo 确认词表。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::{
    pending::{PendingLexicon, PreparedAction, PreparedActionMetadata, expires_at_after},
    session::LAST_QUERY_TTL_SECONDS,
};

use super::{TodoItem, TodoStatus};

pub(crate) const TODO_PENDING_DOMAIN: &str = "todo";

/// 澄清候选的精简展示结构。
///
/// 只保存恢复任务与生成提示所需的最小字段，不持久化完整 [`TodoItem`]。内部 ID
/// 仅供受限 Tool Loop 重新查询 [`crate::runtime::tools::todo::TodoStore`] 校验和确定性编号映射，
/// **不进入用户提示，也不能由 LLM 自由提交**；恢复执行前必须按 ID 重新读取真实状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClarificationCandidate {
    /// 内部 Todo ID；仅供受限 Tool Loop 重新查询与编号映射。
    pub id: String,
    /// 展示顺序（从 1 开始），与给用户看的候选编号一致。
    pub display_number: usize,
    /// 标题，用于生成澄清提示。
    pub title: String,
    /// 捕获时的状态；仅用于检测变化和生成提示，不是执行依据。
    pub status: TodoStatus,
}

/// Agent Loop 中等待用户补充目标的 Todo 工具调用。
///
/// 这里只保存恢复原任务必需的结构化信息：原工具名、原始参数、选择基数、触发澄清的
/// 错误码和本次澄清候选集。后续用户补充目标后，运行时会以候选集作为请求级选择作用域
/// 重入受限 Tool Loop，由 LLM 产出结构化工具调用，再由原 Todo Tool 重新读取
/// `TodoStore` 校验当前目标并执行。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingTodoClarification {
    /// 原始工具名，例如 `complete_todos` / `edit_todo`。
    pub tool_name: String,
    /// 原始工具参数；不包含数据库内部 ID。
    pub arguments: Value,
    /// 原工具是否允许一次操作多条。
    pub allow_many: bool,
    /// 触发澄清的结构化错误码。
    pub error_code: String,
    /// 给用户看的最小澄清问题。
    pub question: String,
    /// 本次澄清候选集及展示顺序；下一轮编号只能映射这份候选，不得使用无关的
    /// `last_todo_query` 快照。
    pub candidates: Vec<ClarificationCandidate>,
    /// 创建时间，按最近查询 TTL 过期。
    pub created_at: String,
}

/// Todo 业务 pending payload。
///
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoPendingPayload {
    /// 删除单个待办
    TodoDelete {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        owner_key: String,
        item: TodoItem,
        created_at: String,
    },
    /// 按条件批量删除待办
    TodoBulkDelete {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        owner_key: String,
        /// 要删除的待办 ID 列表
        item_ids: Vec<String>,
        /// 发起时匹配到的条目数量，用于确认后按原始范围反馈。
        matched_count: usize,
        /// 批量删除限定的目标状态。
        status: TodoStatus,
        /// 操作摘要
        summary: String,
        /// 删除条件的原始描述
        source_condition: String,
        created_at: String,
    },
    /// Agent Loop 内等待用户补充待办目标后恢复原工具动作。
    TodoClarify {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        owner_key: String,
        request: PendingTodoClarification,
        created_at: String,
    },
}

impl TodoPendingPayload {
    pub(crate) fn try_from_pending(
        pending: &PreparedAction,
    ) -> Result<Option<Self>, serde_json::Error> {
        if pending.domain() != TODO_PENDING_DOMAIN {
            return Ok(None);
        }
        serde_json::from_value(pending.payload().clone()).map(Some)
    }

    pub(crate) fn expired_command(pending: &PreparedAction) -> &'static str {
        if pending.kind() == "todo_clarify" {
            "todo_clarify_expired"
        } else {
            "todo_pending_expired"
        }
    }

    /// 将 Todo payload 包装为统一 PreparedAction。
    ///
    /// `scope_key` 必须来自当前 interaction session，不能从 payload 或模型参数推导。
    pub(crate) fn into_prepared_action(self, scope_key: &str) -> PreparedAction {
        let display_snapshot = self.display_snapshot();
        let payload =
            serde_json::to_value(&self).expect("TodoPendingPayload serialization should not fail");
        let action_kind = payload
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("todo_unknown")
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
                domain: TODO_PENDING_DOMAIN.to_owned(),
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

    /// 生成只用于确认提示/后续修订展示的快照，不作为执行依据。
    fn display_snapshot(&self) -> Value {
        match self {
            Self::TodoDelete { item, .. } => serde_json::json!({
                "title": item.title,
                "status": item.status,
            }),
            Self::TodoBulkDelete {
                matched_count,
                status,
                summary,
                source_condition,
                ..
            } => serde_json::json!({
                "matched_count": matched_count,
                "status": status,
                "summary": summary,
                "source_condition": source_condition,
            }),
            Self::TodoClarify { request, .. } => serde_json::json!({
                "question": request.question,
                "candidates": request.candidates.iter().map(|candidate| serde_json::json!({
                    "display_number": candidate.display_number,
                    "title": candidate.title,
                    "status": candidate.status,
                })).collect::<Vec<_>>(),
            }),
        }
    }
}

/// 获取 Todo 场景下的意图识别词汇表。
pub(crate) fn todo_lexicon() -> PendingLexicon {
    PendingLexicon::new(
        &[
            "确认",
            "可以",
            "好",
            "好的",
            "执行",
            "保存",
            "嗯",
            "就这个",
            "就这样",
        ],
        &["取消", "不要", "算了", "不用", "撤销", "放弃"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_payload_builds_versioned_prepared_action() {
        let pending = TodoPendingPayload::TodoBulkDelete {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: "owner:u1".to_owned(),
            item_ids: vec!["todo-1".to_owned()],
            matched_count: 1,
            status: TodoStatus::Completed,
            summary: "旧待办".to_owned(),
            source_condition: "已完成".to_owned(),
            created_at: "2026-07-15T10:00:00+08:00".to_owned(),
        }
        .into_prepared_action("group:g1:actor:u1");

        assert_eq!(pending.kind(), "todo_bulk_delete");
        assert_eq!(pending.scope_key(), "group:g1:actor:u1");
        assert_eq!(pending.expires_at(), "2026-07-15T10:10:00+08:00");
        assert_eq!(pending.display_snapshot()["matched_count"], 1);
        assert!(matches!(
            TodoPendingPayload::try_from_pending(&pending).unwrap(),
            Some(TodoPendingPayload::TodoBulkDelete { .. })
        ));
    }
}
