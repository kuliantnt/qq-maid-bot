//! Todo 专属 pending payload 与确认词表。
//!
//! `runtime::pending::PendingOperation` 只负责通用 envelope；本模块维护 Todo 的
//! 持久化 payload、旧 session 兼容变体、澄清候选边界和 Todo 确认词表。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::pending::{PendingLexicon, PendingOperation};

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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoPendingOperation {
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

impl TodoPendingOperation {
    pub(crate) fn try_from_pending(
        pending: &PendingOperation,
    ) -> Result<Option<Self>, serde_json::Error> {
        if pending.domain() != TODO_PENDING_DOMAIN {
            return Ok(None);
        }
        serde_json::from_value(pending.payload().clone()).map(Some)
    }

    pub(crate) fn expired_command(pending: &PendingOperation) -> &'static str {
        if pending.kind() == "todo_clarify" {
            "todo_clarify_expired"
        } else {
            "todo_pending_expired"
        }
    }
}

impl From<TodoPendingOperation> for PendingOperation {
    fn from(operation: TodoPendingOperation) -> Self {
        let payload = serde_json::to_value(operation)
            .expect("TodoPendingOperation serialization should not fail");
        PendingOperation::from_payload(TODO_PENDING_DOMAIN, payload)
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
        let pending: TodoPendingOperation = serde_json::from_value(json!({
            "kind": "todo_add",
            "owner_key": "u1",
            "draft": {
                "title": "旧待办"
            },
            "created_at": "2026-06-27T12:00:00+08:00"
        }))
        .unwrap();

        match pending {
            TodoPendingOperation::TodoAdd {
                initiator_user_id, ..
            } => assert_eq!(initiator_user_id, None),
            other => panic!("expected TodoAdd, got {other:?}"),
        }
    }
}
