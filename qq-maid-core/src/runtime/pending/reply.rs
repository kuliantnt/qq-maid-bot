//! Pending 确认、取消和修订意图的轻量文本分类。

/// 用户对挂起操作的回复类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingReplyKind {
    Confirm,
    Cancel,
    Revise,
    Wait,
}

/// 用于识别用户确认/取消意图的词汇配置。
#[derive(Debug, Clone, Copy)]
pub struct PendingLexicon {
    confirm_words: &'static [&'static str],
    cancel_words: &'static [&'static str],
}

impl PendingLexicon {
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

pub fn should_parse_pending_revision(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && !text.starts_with('/')
}

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
