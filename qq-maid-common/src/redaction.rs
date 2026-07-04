//! 敏感信息脱敏工具。
//!
//! 这里只维护跨模块共享的文本级脱敏规则；正则集合保持私有，避免调用方依赖规则细节。

use std::sync::LazyLock;

use regex::Regex;

/// 敏感信息匹配模式列表，用于在写入持久化数据或日志前自动脱敏 API Key、Token 等凭证。
static SENSITIVE_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"(?i)(OPENAI_API_KEY\s*=\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"(?i)(DEEPSEEK_API_KEY\s*=\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"(?i)(QQ_SECRET\s*=\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"(?i)(API[_ -]?KEY\s*[:=]\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r#"(?i)((?:ACCESS[_ -]?)?TOKEN["']?\s*[:=]\s*)(["']?)[^"',\s}]+(["']?)"#)
                .unwrap(),
            "$1$2<redacted>$3",
        ),
        (
            Regex::new(r#"(?i)((?:APP[_ -]?)?SECRET["']?\s*[:=]\s*)(["']?)[^"',\s}]+(["']?)"#)
                .unwrap(),
            "$1$2<redacted>$3",
        ),
        (
            Regex::new(r"(?i)(SECRET\s*[:=]\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"(?i)(TOKEN\s*[:=]\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            // 日志中的上游错误有时只带截短后的 key 片段，仍需要按凭证处理。
            Regex::new(r"sk-[A-Za-z0-9_-]{8,}").unwrap(),
            "<redacted:openai_api_key>",
        ),
        (
            // Bearer 既可能是长 token，也可能是点分段的 JWT 片段；短普通词保持不脱敏。
            Regex::new(r"(?i)Bearer\s+(?:[A-Za-z0-9._-]{20,}|[A-Za-z0-9_-]+\.[A-Za-z0-9._-]+)")
                .unwrap(),
            "Bearer <redacted>",
        ),
    ]
});

/// 脱敏文本中的敏感信息。
pub fn redact_sensitive_text(text: impl AsRef<str>) -> String {
    let mut redacted = text.as_ref().to_owned();
    for (pattern, replacement) in SENSITIVE_PATTERNS.iter() {
        redacted = pattern.replace_all(&redacted, *replacement).to_string();
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_named_provider_keys_and_tokens() {
        assert_eq!(
            redact_sensitive_text("OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz123456"),
            "OPENAI_API_KEY=<redacted>"
        );
        assert_eq!(
            redact_sensitive_text("DEEPSEEK_API_KEY=deepseek-fake-token-value"),
            "DEEPSEEK_API_KEY=<redacted>"
        );
        assert_eq!(
            redact_sensitive_text("QQ_SECRET=qq-fake-secret-value"),
            "QQ_SECRET=<redacted>"
        );
        assert_eq!(
            redact_sensitive_text("TOKEN: abcdefghijklmnopqrstuvwxyz123456"),
            "TOKEN: <redacted>"
        );
        assert_eq!(
            redact_sensitive_text(r#"{"access_token":"token-value","app_secret":"secret-value"}"#),
            r#"{"access_token":"<redacted>","app_secret":"<redacted>"}"#
        );
    }

    #[test]
    fn redacts_generic_sk_and_bearer_values() {
        assert_eq!(
            redact_sensitive_text("key sk-abcdefghijklmnopqrstuvwxyz123456"),
            "key <redacted:openai_api_key>"
        );
        assert_eq!(
            redact_sensitive_text("key sk-test-secret"),
            "key <redacted:openai_api_key>"
        );
        assert_eq!(
            redact_sensitive_text("Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456"),
            "Authorization: Bearer <redacted>"
        );
        assert_eq!(
            redact_sensitive_text("Authorization: Bearer abc.def.ghi"),
            "Authorization: Bearer <redacted>"
        );
    }

    #[test]
    fn redacts_multiple_sensitive_values_in_same_text() {
        let text = "OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz123456\nBearer abcdefghijklmnopqrstuvwxyz123456";

        assert_eq!(
            redact_sensitive_text(text),
            "OPENAI_API_KEY=<redacted>\nBearer <redacted>"
        );
    }

    #[test]
    fn keeps_normal_and_partial_similar_text_unchanged() {
        assert_eq!(
            redact_sensitive_text("普通文本没有凭证"),
            "普通文本没有凭证"
        );
        assert_eq!(redact_sensitive_text("sk-short"), "sk-short");
        assert_eq!(redact_sensitive_text("Bearer short"), "Bearer short");
        assert_eq!(
            redact_sensitive_text("API key maybe later"),
            "API key maybe later"
        );
    }
}
