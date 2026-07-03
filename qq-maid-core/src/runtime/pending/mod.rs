//! 待用户确认的挂起操作。
//!
//! 当 LLM 返回的操作需要用户二次确认（如记忆写入、待办取消/永久删除，
//! 以及旧版新增待办草稿兼容）时，将这些操作暂存为挂起状态，等待用户确认或取消。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::todo::{TodoItem, TodoItemDraft, TodoStatus};

/// 澄清候选的精简展示结构。
///
/// 只保存恢复任务与生成提示所需的最小字段，不持久化完整 [`TodoItem`]。内部 ID
/// 仅供受限 Tool Loop 重新查询 [`crate::runtime::todo::TodoStore`] 校验和确定性编号映射，
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

/// 待确认的记忆创建操作。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingMemory {
    /// 记忆内容
    pub content: String,
    /// 提取记忆的原始文本来源
    pub source_text: String,
    #[serde(rename = "type")]
    /// 记忆类型（如 "profile"、"preference" 等）
    pub memory_type: String,
    /// 作用范围（如 "user"、"group"）
    pub scope: String,
    /// 创建时间
    pub created_at: String,
    /// 目标访问边界类型；旧 `scope` 只是业务分类，不能用于权限判断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope_type: Option<String>,
    /// 目标访问边界 ID：个人为用户 ID，群记忆为群 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope_id: Option<String>,
}

/// 待确认的记忆更新操作。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingMemoryUpdate {
    /// 要更新的记忆 ID
    pub id: String,
    /// 更新前的内容（供对比）
    pub before_content: String,
    /// 更新后的新内容
    pub content: String,
    #[serde(rename = "type")]
    /// 记忆类型
    pub memory_type: String,
    /// 作用范围
    pub scope: String,
    /// 创建时间
    pub created_at: String,
    /// 发起编辑时的目标访问边界类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope_type: Option<String>,
    /// 发起编辑时的目标访问边界 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope_id: Option<String>,
}

/// 待确认的记忆删除操作。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingMemoryDelete {
    /// 要删除的记忆 ID
    pub id: String,
    /// 被删除的记忆内容
    pub content: String,
    #[serde(rename = "type")]
    /// 记忆类型
    pub memory_type: String,
    /// 作用范围
    pub scope: String,
    /// 创建时间
    pub created_at: String,
    /// 发起删除时的目标访问边界类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope_type: Option<String>,
    /// 发起删除时的目标访问边界 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope_id: Option<String>,
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

/// 挂起的操作枚举。
///
/// 根据 `kind` 字段进行序列化标记，涵盖记忆和待办两大类操作。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingOperation {
    /// 创建新记忆
    MemoryCreate {
        /// 发起 pending 的用户标识；旧会话缺失该字段时按历史行为兼容。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        memory: PendingMemory,
    },
    /// 更新已有记忆
    MemoryUpdate {
        /// 发起 pending 的用户标识；旧会话缺失该字段时按历史行为兼容。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        update: PendingMemoryUpdate,
    },
    /// 删除记忆
    MemoryDelete {
        /// 发起 pending 的用户标识；旧会话缺失该字段时按历史行为兼容。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initiator_user_id: Option<String>,
        delete: PendingMemoryDelete,
    },
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

fn default_todo_add_allow_revision() -> bool {
    true
}

fn default_todo_bulk_delete_status() -> TodoStatus {
    TodoStatus::Completed
}

impl PendingOperation {
    /// 获取操作的所有者键。
    ///
    /// 记忆操作没有所有者概念，返回 `None`；待办操作返回 `owner_key`。
    pub fn owner_key(&self) -> Option<&str> {
        match self {
            Self::MemoryCreate { .. } | Self::MemoryUpdate { .. } | Self::MemoryDelete { .. } => {
                None
            }
            Self::TodoAdd { owner_key, .. }
            | Self::TodoDone { owner_key, .. }
            | Self::TodoEdit { owner_key, .. }
            | Self::TodoDelete { owner_key, .. }
            | Self::TodoBulkDelete { owner_key, .. }
            | Self::TodoSelectCandidate { owner_key, .. }
            | Self::TodoClarify { owner_key, .. } => Some(owner_key.as_str()),
        }
    }

    /// 获取 pending 发起人。旧持久化数据没有该字段时返回 `None`，继续按历史行为处理。
    pub fn initiator_user_id(&self) -> Option<&str> {
        match self {
            Self::MemoryCreate {
                initiator_user_id, ..
            }
            | Self::MemoryUpdate {
                initiator_user_id, ..
            }
            | Self::MemoryDelete {
                initiator_user_id, ..
            }
            | Self::TodoAdd {
                initiator_user_id, ..
            }
            | Self::TodoDone {
                initiator_user_id, ..
            }
            | Self::TodoEdit {
                initiator_user_id, ..
            }
            | Self::TodoDelete {
                initiator_user_id, ..
            }
            | Self::TodoBulkDelete {
                initiator_user_id, ..
            }
            | Self::TodoSelectCandidate {
                initiator_user_id, ..
            }
            | Self::TodoClarify {
                initiator_user_id, ..
            } => initiator_user_id.as_deref(),
        }
    }

    /// 获取 pending 创建时间。旧持久化结构均有该字段；用于恢复前统一过期治理。
    pub fn created_at(&self) -> &str {
        match self {
            Self::MemoryCreate { memory, .. } => &memory.created_at,
            Self::MemoryUpdate { update, .. } => &update.created_at,
            Self::MemoryDelete { delete, .. } => &delete.created_at,
            Self::TodoAdd { created_at, .. }
            | Self::TodoDone { created_at, .. }
            | Self::TodoEdit { created_at, .. }
            | Self::TodoDelete { created_at, .. }
            | Self::TodoBulkDelete { created_at, .. }
            | Self::TodoSelectCandidate { created_at, .. }
            | Self::TodoClarify { created_at, .. } => created_at,
        }
    }
}

/// 用户对挂起操作的回复类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingReplyKind {
    /// 确认执行
    Confirm,
    /// 取消操作
    Cancel,
    /// 修改草稿内容
    Revise,
    /// 暂不确定，保持等待
    Wait,
}

/// 用于识别用户确认/取消意图的词汇配置。
#[derive(Debug, Clone, Copy)]
pub struct PendingLexicon {
    /// 表示确认的关键词列表
    confirm_words: &'static [&'static str],
    /// 表示取消的关键词列表
    cancel_words: &'static [&'static str],
}

/// 获取待办场景下的意图识别词汇表。
pub fn todo_lexicon() -> PendingLexicon {
    PendingLexicon {
        confirm_words: &[
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
        cancel_words: &["取消", "不要", "算了", "不用", "撤销", "放弃"],
    }
}

pub fn memory_lexicon() -> PendingLexicon {
    PendingLexicon {
        confirm_words: &["确认", "可以", "记吧", "写入", "保存", "嗯", "好"],
        cancel_words: &["取消", "不记", "算了", "不要", "不用", "撤销"],
    }
}

/// 根据用户回复文本和词汇表，分类用户的确认/取消/修改意图。
///
/// 优先匹配取消词，其次匹配确认词，如果有非空非命令文本则视为修订，
/// 否则保持等待状态。
pub fn classify_reply(text: &str, lexicon: PendingLexicon) -> PendingReplyKind {
    let text = text.trim();
    let compact = compact_pending_reply(text);
    if lexicon.cancel_words.contains(&text)
        || lexicon
            .cancel_words
            .iter()
            .any(|word| compact == *word || compact.starts_with(word))
    {
        return PendingReplyKind::Cancel;
    }

    if lexicon
        .confirm_words
        .iter()
        .any(|word| is_confirm_match(&compact, word))
    {
        return PendingReplyKind::Confirm;
    }
    if should_parse_pending_revision(text) {
        return PendingReplyKind::Revise;
    }
    PendingReplyKind::Wait
}

/// 判断用户回复是否应被解析为对挂起草稿的修订。
///
/// 非空且不以 `/` 开头的文本视为修订意图。
pub fn should_parse_pending_revision(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && !text.starts_with('/')
}

/// 返回修订失败时的提示文本。
pub fn pending_revision_failed_reply() -> &'static str {
    "这次没整理成功，当前草稿已保留。可以换个说法，或回复“确认 / 取消”。"
}

/// 压缩回复文本：移除空白字符和常见标点，去掉尾部的语气助词，
/// 得到紧凑的文本用于关键词匹配。
fn compact_pending_reply(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !ch.is_whitespace()
                && !matches!(
                    ch,
                    '，' | ','
                        | '。'
                        | '.'
                        | '！'
                        | '!'
                        | '？'
                        | '?'
                        | '、'
                        | ';'
                        | '；'
                        | ':'
                        | '：'
                )
        })
        .collect::<String>()
        .trim_end_matches(['了', '吧', '啊', '呀', '呢'])
        .to_owned()
}

/// 判断紧凑文本是否匹配确认词，支持关键词后接补充指示（如"确认执行"）。
fn is_confirm_match(compact: &str, word: &str) -> bool {
    if compact == word {
        return true;
    }
    let Some(rest) = compact.strip_prefix(word) else {
        return false;
    };
    matches!(
        rest,
        "就这个" | "就这样" | "执行" | "保存" | "写入" | "删除" | "新增" | "修改"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_pending_without_initiator_deserializes() {
        let pending: PendingOperation = serde_json::from_value(json!({
            "kind": "memory_create",
            "memory": {
                "content": "旧 pending",
                "source_text": "/memory 旧 pending",
                "type": "note",
                "scope": "general",
                "created_at": "2026-06-27T12:00:00+08:00"
            }
        }))
        .unwrap();

        assert_eq!(pending.initiator_user_id(), None);
    }
}
