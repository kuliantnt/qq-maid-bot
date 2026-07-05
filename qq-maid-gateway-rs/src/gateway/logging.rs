use super::{
    event::{C2cMessage, GroupMessage},
    ping::is_ping_command,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct C2cMessageLogSummary {
    pub message_id: String,
    pub masked_user: String,
    pub content_len: usize,
    pub attachment_count: usize,
    pub is_ping: bool,
    pub extracted_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMessageLogSummary {
    pub message_id: String,
    pub masked_group: String,
    pub masked_member: Option<String>,
    pub event_type: &'static str,
    pub content_len: usize,
    pub mention_count: usize,
    pub attachment_count: usize,
    pub is_ping: bool,
    pub extracted_content: Option<String>,
}

pub fn mask_openid(value: &str) -> String {
    mask_identifier(value)
}

pub fn mask_identifier(value: &str) -> String {
    let text = value.trim();
    if text.is_empty() {
        return "<empty>".to_owned();
    }
    if text.chars().count() <= 6 {
        return "******".to_owned();
    }

    let suffix = text
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("******{suffix}")
}

pub fn mask_scope_key(scope_key: &str) -> String {
    let structural_parts = [
        "private",
        "group",
        "guild",
        "channel",
        "guild_channel",
        "qq",
        "c2c",
    ];
    scope_key
        .split(':')
        .map(|part| {
            if structural_parts.contains(&part) || part.is_empty() {
                part.to_owned()
            } else {
                mask_identifier(part)
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// 对 URL 进行脱敏处理：遮盖用户名/密码、敏感查询参数值以及 fragment。
/// 如果 URL 无法被 `reqwest::Url` 解析，则回退到基于字符串的脱敏逻辑。
pub fn mask_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // 尝试用 reqwest::Url 解析，走结构化脱敏路径
    if let Ok(parsed) = reqwest::Url::parse(trimmed) {
        return mask_parsed_url(parsed);
    }

    // 解析失败时，回退到原始字符串处理
    mask_raw_url_query(trimmed)
}

pub fn mask_url_query(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return "empty url".to_owned();
    }

    if let Ok(parsed) = reqwest::Url::parse(trimmed) {
        return parsed
            .query()
            .filter(|query| !query.is_empty())
            .map(mask_query_string)
            .unwrap_or_else(|| "none".to_owned());
    }

    trimmed
        .split_once('?')
        .map(|(_, query)| {
            query
                .split_once('#')
                .map(|(query, _)| query)
                .unwrap_or(query)
        })
        .filter(|query| !query.is_empty())
        .map(mask_query_string)
        .unwrap_or_else(|| "none or invalid url".to_owned())
}

fn mask_parsed_url(mut url: reqwest::Url) -> String {
    if !url.username().is_empty() {
        let _ = url.set_username("***");
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("***"));
    }
    let pairs = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if !pairs.is_empty() {
        url.set_query(None);
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in pairs {
                let masked_value = if is_sensitive_query_key(&key) {
                    "***"
                } else {
                    value.as_str()
                };
                query.append_pair(&key, masked_value);
            }
        }
    }
    if url.fragment().is_some() {
        url.set_fragment(Some("***"));
    }
    url.to_string()
}

fn mask_raw_url_query(raw: &str) -> String {
    let (without_fragment, fragment) = raw
        .split_once('#')
        .map(|(base, _)| (base, Some("***")))
        .unwrap_or((raw, None));
    let base = without_fragment
        .split_once('?')
        .map(|(base, query)| format!("{base}?{}", mask_query_string(query)))
        .unwrap_or_else(|| without_fragment.to_owned());
    match fragment {
        Some(fragment) => format!("{base}#{fragment}"),
        None => base,
    }
}

fn mask_query_string(query: &str) -> String {
    query
        .split('&')
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            if is_sensitive_query_key(key) {
                format!("{key}=***")
            } else if part.contains('=') {
                format!("{key}={value}")
            } else {
                key.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn is_sensitive_query_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "token"
            | "access_token"
            | "secret"
            | "client_secret"
            | "app_secret"
            | "key"
            | "api_key"
            | "secret_key"
            | "sign"
            | "signature"
            | "authorization"
            | "auth"
            | "password"
            | "passwd"
    ) || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_key")
}

pub fn reqwest_error_summary(error: &reqwest::Error) -> String {
    let mut flags = Vec::new();
    if error.is_timeout() {
        flags.push("timeout");
    }
    if error.is_connect() {
        flags.push("connect");
    }
    if error.is_decode() {
        flags.push("decode");
    }
    if error.is_request() {
        flags.push("request");
    }
    if let Some(status) = error.status() {
        return format!("http status {status}");
    }
    if flags.is_empty() {
        "request failed".to_owned()
    } else {
        format!("request failed ({})", flags.join(","))
    }
}

pub fn c2c_message_log_summary(message: &C2cMessage, verbose_log: bool) -> C2cMessageLogSummary {
    C2cMessageLogSummary {
        message_id: message.message_id.clone(),
        masked_user: mask_openid(&message.user_openid),
        content_len: message.content.chars().count(),
        attachment_count: message.attachments.len(),
        is_ping: is_ping_command(&message.content),
        extracted_content: verbose_log.then(|| message.content.clone()),
    }
}

pub fn group_message_log_summary(
    message: &GroupMessage,
    verbose_log: bool,
) -> GroupMessageLogSummary {
    GroupMessageLogSummary {
        message_id: message.message_id.clone(),
        masked_group: mask_identifier(&message.group_openid),
        masked_member: message.member_openid.as_deref().map(mask_identifier),
        event_type: message.event_type.as_respond_event_type(),
        content_len: message.content.chars().count(),
        mention_count: message.mentions.len(),
        attachment_count: message.attachments.len(),
        is_ping: is_ping_command(&message.content),
        extracted_content: verbose_log.then(|| message.content.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(content: &str) -> C2cMessage {
        C2cMessage {
            message_id: "msg-1".to_owned(),
            current_msg_idx: None,
            event_id: Some("event-1".to_owned()),
            source_message_ids: vec!["msg-1".to_owned()],
            source_event_ids: vec!["event-1".to_owned()],
            user_openid: "用户-openid-123456".to_owned(),
            content: content.to_owned(),
            reply: None,
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            input_parts: if content.trim().is_empty() {
                Vec::new()
            } else {
                vec![qq_maid_common::input_part::MessageInputPart::text(content)]
            },
            attachments: Vec::new(),
        }
    }

    #[test]
    fn masks_openid_without_byte_slicing() {
        assert_eq!(mask_openid("abc"), "******");
        assert_eq!(mask_openid("用户-openid-123456"), "******123456");
        assert_eq!(mask_openid("  "), "<empty>");
        assert_eq!(
            mask_scope_key("private:用户-openid-123456"),
            "private:******123456"
        );
    }

    #[test]
    fn masks_sensitive_url_query_values_only() {
        assert_eq!(
            mask_url("http://127.0.0.1:8787/api/debug?token=secret&debug=1&timeout=800"),
            "http://127.0.0.1:8787/api/debug?token=***&debug=1&timeout=800"
        );
        assert_eq!(
            mask_url("http://127.0.0.1:8787/api/debug"),
            "http://127.0.0.1:8787/api/debug"
        );
        assert_eq!(
            mask_url("not a url?api_key=secret&debug=1#token-fragment"),
            "not a url?api_key=***&debug=1#***"
        );
        assert_eq!(
            mask_url("http://user:pass@127.0.0.1:8787/api/debug?debug=1"),
            "http://***:***@127.0.0.1:8787/api/debug?debug=1"
        );
        assert_eq!(
            mask_url_query("http://127.0.0.1:8787/api/debug?access_token=secret&debug=1"),
            "access_token=***&debug=1"
        );
    }

    /// 合并 3 个 c2c_message_log_summary 测试为表驱动测试。
    #[test]
    fn c2c_message_log_summary_reports_correctly() {
        struct Case {
            name: &'static str,
            content: &'static str,
            attachments: Vec<crate::event::Attachment>,
            verbose: bool,
            /// None 表示不检查 extracted_content
            expected_extracted: Option<Option<&'static str>>,
            /// None 表示不检查 is_ping
            expected_is_ping: Option<bool>,
            /// None 表示不检查 attachment_count
            expected_attachment_count: Option<usize>,
            forbidden_debug_substrings: &'static [&'static str],
        }

        let cases = [
            Case {
                name: "summary_omits_content_when_verbose_is_disabled",
                content: "secret token Authorization header",
                attachments: Vec::new(),
                verbose: false,
                expected_extracted: Some(None),
                expected_is_ping: None,
                expected_attachment_count: None,
                forbidden_debug_substrings: &[
                    "secret",
                    "token",
                    "Authorization",
                    "用户-openid-123456",
                ],
            },
            Case {
                name: "summary_includes_extracted_content_when_verbose_is_enabled",
                content: "extracted content",
                attachments: Vec::new(),
                verbose: true,
                expected_extracted: Some(Some("extracted content")),
                expected_is_ping: None,
                expected_attachment_count: None,
                forbidden_debug_substrings: &[],
            },
            Case {
                name: "summary_marks_ping_and_attachment_count",
                content: "/ping",
                attachments: vec![crate::event::Attachment {
                    content_type: Some("image/jpeg".to_owned()),
                    filename: Some("a.jpg".to_owned()),
                    url: Some("https://example.test/a.jpg".to_owned()),
                    size_bytes: None,
                    media_id: None,
                    file_id: None,
                    attachment_id: None,
                }],
                verbose: false,
                expected_extracted: None,
                expected_is_ping: Some(true),
                expected_attachment_count: Some(1),
                forbidden_debug_substrings: &[],
            },
        ];

        for case in &cases {
            let mut msg = message(case.content);
            msg.attachments = case.attachments.clone();
            let summary = c2c_message_log_summary(&msg, case.verbose);

            if let Some(expected) = case.expected_extracted {
                assert_eq!(
                    summary.extracted_content.as_deref(),
                    expected,
                    "case '{}' failed: extracted_content mismatch",
                    case.name
                );
            }
            if let Some(expected_ping) = case.expected_is_ping {
                assert_eq!(
                    summary.is_ping, expected_ping,
                    "case '{}' failed: is_ping mismatch",
                    case.name
                );
            }
            if let Some(expected_count) = case.expected_attachment_count {
                assert_eq!(
                    summary.attachment_count, expected_count,
                    "case '{}' failed: attachment_count mismatch",
                    case.name
                );
            }

            // 安全断言：verbose=false 时 Debug 输出不应包含敏感内容。
            if !case.verbose && !case.forbidden_debug_substrings.is_empty() {
                let rendered = format!("{summary:?}");
                for forbidden in case.forbidden_debug_substrings {
                    assert!(
                        !rendered.contains(forbidden),
                        "case '{}': Debug output leaked '{}'",
                        case.name,
                        forbidden
                    );
                }
            }
        }
    }
}
