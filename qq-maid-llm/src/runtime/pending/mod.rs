//! 待用户确认的挂起操作。
//!
//! 当 LLM 返回的操作需要用户二次确认（如新增/修改/删除记忆或待办）时，
//! 将这些操作暂存为挂起状态，等待用户确认、取消或修改。

use serde::{Deserialize, Serialize};

use crate::storage::todo::{TodoItem, TodoItemDraft};

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

/// 挂起的操作枚举。
///
/// 根据 `kind` 字段进行序列化标记，涵盖记忆和待办两大类操作。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingOperation {
    /// 创建新记忆
    MemoryCreate { memory: PendingMemory },
    /// 更新已有记忆
    MemoryUpdate { update: PendingMemoryUpdate },
    /// 删除记忆
    MemoryDelete { delete: PendingMemoryDelete },
    /// 新增待办事项
    TodoAdd {
        /// 所有者标识键
        owner_key: String,
        /// 待办草稿
        draft: TodoItemDraft,
        /// 创建时间
        created_at: String,
    },
    /// 标记待办为完成
    TodoDone {
        owner_key: String,
        item: TodoItem,
        created_at: String,
    },
    /// 编辑待办事项
    TodoEdit {
        owner_key: String,
        /// 编辑前的待办项
        before: TodoItem,
        /// 编辑后的草稿
        draft: TodoItemDraft,
        created_at: String,
    },
    /// 删除单个待办
    TodoDelete {
        owner_key: String,
        item: TodoItem,
        created_at: String,
    },
    /// 按条件批量删除待办
    TodoBulkDelete {
        owner_key: String,
        /// 要删除的待办 ID 列表
        item_ids: Vec<String>,
        /// 操作摘要
        summary: String,
        /// 删除条件的原始描述
        source_condition: String,
        created_at: String,
    },
    /// 需要用户从多个候选中选择操作的待办
    TodoSelectCandidate {
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
            | Self::TodoSelectCandidate { owner_key, .. } => Some(owner_key.as_str()),
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
