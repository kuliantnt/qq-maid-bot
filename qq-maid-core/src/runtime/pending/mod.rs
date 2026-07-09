//! 跨工具可复用的 pending 基础设施。
//!
//! 本模块只保存通用 envelope、生命周期判断所需元数据和确认/取消意图分类。
//! 具体业务 pending payload 由各工具域维护。

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};
use serde_json::Value;

/// 持久化 pending 的通用 envelope。
///
/// 数据库中仍保存各业务原有的 JSON payload；本结构只从 payload 中抽取通用字段，
/// 便于 respond 层做过期、发起人和 owner 隔离判断。业务执行前必须回到对应工具域
/// 反序列化 payload，不能在这里解释业务字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOperation {
    domain: String,
    kind: String,
    created_at: String,
    initiator_user_id: Option<String>,
    owner_key: Option<String>,
    payload: Value,
}

const SUPPORTED_PENDING_DOMAINS: &[&str] = &["todo"];

impl PendingOperation {
    /// 从业务 payload 构造通用 envelope。
    pub fn from_payload(domain: impl Into<String>, payload: Value) -> Self {
        let domain = domain.into();
        Self {
            kind: string_field(&payload, "kind").unwrap_or_else(|| domain.clone()),
            created_at: string_field(&payload, "created_at").unwrap_or_default(),
            initiator_user_id: string_field(&payload, "initiator_user_id"),
            owner_key: string_field(&payload, "owner_key"),
            domain,
            payload,
        }
    }

    /// pending 所属业务域，例如 `todo`。
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// 业务 payload 的原始 kind，例如 `todo_bulk_delete`。
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// 获取操作的所有者键。没有 owner 概念的业务返回 `None`。
    pub fn owner_key(&self) -> Option<&str> {
        self.owner_key.as_deref()
    }

    /// 获取 pending 发起人。旧持久化数据没有该字段时返回 `None`，继续按历史行为处理。
    pub fn initiator_user_id(&self) -> Option<&str> {
        self.initiator_user_id.as_deref()
    }

    /// 获取 pending 创建时间。缺失时返回空串，调用方会按过期状态处理。
    pub fn created_at(&self) -> &str {
        &self.created_at
    }

    /// 业务原始 payload。只允许对应工具域继续解释。
    pub fn payload(&self) -> &Value {
        &self.payload
    }
}

impl Serialize for PendingOperation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.payload.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PendingOperation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let payload = Value::deserialize(deserializer)?;
        let kind = string_field(&payload, "kind").unwrap_or_else(|| "unknown".to_owned());
        let domain = domain_from_kind(&kind);
        if !SUPPORTED_PENDING_DOMAINS.contains(&domain.as_str()) {
            return Err(D::Error::custom(format!(
                "unsupported pending operation kind `{kind}`"
            )));
        }
        Ok(Self::from_payload(domain, payload))
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

impl PendingLexicon {
    /// 构造某个业务域自己的确认/取消词表。
    pub const fn new(
        confirm_words: &'static [&'static str],
        cancel_words: &'static [&'static str],
    ) -> Self {
        Self {
            confirm_words,
            cancel_words,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_pending_without_initiator_deserializes_as_generic_envelope() {
        let pending: PendingOperation = serde_json::from_value(json!({
            "kind": "todo_add",
            "owner_key": "u1",
            "draft": {
                "title": "旧事项"
            },
            "created_at": "2026-06-27T12:00:00+08:00"
        }))
        .unwrap();

        assert_eq!(pending.domain(), "todo");
        assert_eq!(pending.kind(), "todo_add");
        assert_eq!(pending.owner_key(), Some("u1"));
        assert_eq!(pending.initiator_user_id(), None);
    }
}
