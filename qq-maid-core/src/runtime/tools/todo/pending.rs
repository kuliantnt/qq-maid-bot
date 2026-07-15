//! Todo 专属 pending payload 与确认词表。
//!
//! `runtime::pending::PreparedAction` 只负责通用 envelope；本模块维护 Todo 的
//! 持久化 payload、旧 session 兼容变体、澄清候选边界和 Todo 确认词表。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::{
    pending::{PendingLexicon, PreparedAction, PreparedActionMetadata, expires_at_after},
    session::LAST_QUERY_TTL_SECONDS,
};

use super::{TodoItem, TodoItemDraft, TodoStatus};

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

/// 待确认的待办操作类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingTodoAction {
    /// 标记完成
    Done,
    /// 编辑内容
    Edit,
    /// 删除
    Delete,
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
    #[serde(default)]
    pub allow_many: bool,
    /// 触发澄清的结构化错误码。
    pub error_code: String,
    /// 给用户看的最小澄清问题。
    pub question: String,
    /// 本次澄清候选集及展示顺序；下一轮编号只能映射这份候选，不得使用无关的
    /// `last_todo_query` 快照。旧 pending 缺失该字段时兼容为空，恢复路径会安全提示
    /// 用户重新发起。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<ClarificationCandidate>,
    /// 创建时间，按最近查询 TTL 过期。
    pub created_at: String,
}

/// Todo 业务 pending payload。
///
/// 仍按历史 `kind=todo_*` 格式序列化，保证已持久化 session 和现有数据库字段兼容。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
// 这些变体名刻意保留 `Todo` 前缀：它们对应迁移期仍需兼容的历史
// `kind=todo_*` 持久化 pending 语义，避免和通用 Pending envelope 混淆。
#[allow(clippy::enum_variant_names)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoPendingPayload {
    /// 旧版新增待办草稿确认。
    ///
    /// 新版本 `create_todo` 已直接写库，不再产生该 pending；保留此变体只为兼容
    /// 已持久化的旧 Session，允许用户继续确认或取消旧草稿。
    TodoAdd {
        /// 发起 pending 的用户标识；与 owner_key 分开保存，避免 user_id 缺失时绕过校验。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        /// 所有者标识键
        owner_key: String,
        /// 待办草稿
        draft: TodoItemDraft,
        /// 旧版草稿是否允许自然语言修订。
        ///
        /// 新版本不会再生成 `TodoAdd` pending，也不会恢复 pending 阶段二次 LLM 修订；
        /// 字段仅为旧 Session 反序列化兼容保留。
        #[serde(default = "default_todo_add_allow_revision")]
        allow_revision: bool,
        /// 创建时间
        created_at: String,
    },
    /// 标记待办为完成
    TodoDone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        owner_key: String,
        item: TodoItem,
        created_at: String,
    },
    /// 编辑待办事项
    TodoEdit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        owner_key: String,
        /// 编辑前的待办项
        before: TodoItem,
        /// 编辑后的草稿
        draft: TodoItemDraft,
        created_at: String,
    },
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
        #[serde(default)]
        matched_count: usize,
        /// 批量删除限定的目标状态；旧 pending 缺失该字段时兼容为已完成清理。
        #[serde(default = "default_todo_bulk_delete_status")]
        status: TodoStatus,
        /// 操作摘要
        summary: String,
        /// 删除条件的原始描述
        source_condition: String,
        created_at: String,
    },
    /// 需要用户从多个候选中选择操作的待办
    TodoSelectCandidate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        owner_key: String,
        /// 待执行的操作类型
        action: PendingTodoAction,
        /// 候选待办项列表
        candidates: Vec<TodoItem>,
        /// 用户提供的编辑文本（仅在编辑操作时存在）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edit_text: Option<String>,
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
            Self::TodoAdd { draft, .. } => serde_json::json!({
                "title": draft.title,
            }),
            Self::TodoDone { item, .. } | Self::TodoDelete { item, .. } => serde_json::json!({
                "title": item.title,
                "status": item.status,
            }),
            Self::TodoEdit { before, draft, .. } => serde_json::json!({
                "before_title": before.title,
                "after_title": draft.title,
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
            Self::TodoSelectCandidate {
                action, candidates, ..
            } => serde_json::json!({
                "action": action,
                "candidates": candidates.iter().enumerate().map(|(index, item)| serde_json::json!({
                    "display_number": index + 1,
                    "title": item.title,
                    "status": item.status,
                })).collect::<Vec<_>>(),
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

fn default_todo_add_allow_revision() -> bool {
    true
}

fn default_todo_bulk_delete_status() -> TodoStatus {
    TodoStatus::Completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_pending_without_initiator_deserializes() {
        let pending: TodoPendingPayload = serde_json::from_value(json!({
            "kind": "todo_add",
            "owner_key": "u1",
            "draft": {
                "title": "旧待办"
            },
            "created_at": "2026-06-27T12:00:00+08:00"
        }))
        .unwrap();

        match pending {
            TodoPendingPayload::TodoAdd {
                initiator_user_id, ..
            } => assert_eq!(initiator_user_id, None),
            other => panic!("expected TodoAdd, got {other:?}"),
        }
    }

    #[test]
    fn all_legacy_todo_pending_json_variants_decode_through_prepared_action() {
        let item: TodoItem = serde_json::from_value(json!({
            "id": "todo-1",
            "scope_key": "u1",
            "title": "旧待办",
            "created_at": "2026-07-15T09:00:00+08:00",
            "updated_at": "2026-07-15T09:00:00+08:00"
        }))
        .unwrap();
        let draft: TodoItemDraft = serde_json::from_value(json!({"title": "旧待办"})).unwrap();
        let created_at = "2026-07-15T10:00:00+08:00".to_owned();
        let operations = vec![
            TodoPendingPayload::TodoAdd {
                initiator_user_id: None,
                owner_key: "u1".to_owned(),
                draft: draft.clone(),
                allow_revision: true,
                created_at: created_at.clone(),
            },
            TodoPendingPayload::TodoDone {
                initiator_user_id: None,
                owner_key: "u1".to_owned(),
                item: item.clone(),
                created_at: created_at.clone(),
            },
            TodoPendingPayload::TodoEdit {
                initiator_user_id: None,
                owner_key: "u1".to_owned(),
                before: item.clone(),
                draft: draft.clone(),
                created_at: created_at.clone(),
            },
            TodoPendingPayload::TodoDelete {
                initiator_user_id: None,
                owner_key: "u1".to_owned(),
                item: item.clone(),
                created_at: created_at.clone(),
            },
            TodoPendingPayload::TodoBulkDelete {
                initiator_user_id: None,
                owner_key: "u1".to_owned(),
                item_ids: vec![item.id.clone()],
                matched_count: 1,
                status: TodoStatus::Completed,
                summary: "旧待办".to_owned(),
                source_condition: "已完成".to_owned(),
                created_at: created_at.clone(),
            },
            TodoPendingPayload::TodoSelectCandidate {
                initiator_user_id: None,
                owner_key: "u1".to_owned(),
                action: PendingTodoAction::Delete,
                candidates: vec![item.clone()],
                edit_text: None,
                created_at: created_at.clone(),
            },
            TodoPendingPayload::TodoClarify {
                initiator_user_id: None,
                owner_key: "u1".to_owned(),
                request: PendingTodoClarification {
                    tool_name: "complete_todos".to_owned(),
                    arguments: json!({"numbers": null}),
                    allow_many: true,
                    error_code: "todo_reference_unavailable".to_owned(),
                    question: "哪一条？".to_owned(),
                    candidates: vec![],
                    created_at: created_at.clone(),
                },
                created_at,
            },
        ];

        for operation in operations {
            let legacy_json = serde_json::to_value(&operation).unwrap();
            let action: PreparedAction = serde_json::from_value(legacy_json).unwrap();
            assert!(action.is_legacy(), "kind={}", action.kind());
            assert_eq!(
                TodoPendingPayload::try_from_pending(&action).unwrap(),
                Some(operation)
            );
        }
    }

    #[test]
    fn todo_payload_builds_versioned_prepared_action() {
        let pending = TodoPendingPayload::TodoAdd {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: "owner:u1".to_owned(),
            draft: TodoItemDraft {
                title: "新待办".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: Default::default(),
                recurrence_kind: Default::default(),
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: Default::default(),
            },
            allow_revision: false,
            created_at: "2026-07-15T10:00:00+08:00".to_owned(),
        }
        .into_prepared_action("group:g1:actor:u1");

        assert!(!pending.is_legacy());
        assert_eq!(pending.kind(), "todo_add");
        assert_eq!(pending.scope_key(), Some("group:g1:actor:u1"));
        assert_eq!(pending.expires_at(), Some("2026-07-15T10:10:00+08:00"));
        assert_eq!(pending.display_snapshot()["title"], "新待办");
        assert!(matches!(
            TodoPendingPayload::try_from_pending(&pending).unwrap(),
            Some(TodoPendingPayload::TodoAdd { .. })
        ));
    }
}
