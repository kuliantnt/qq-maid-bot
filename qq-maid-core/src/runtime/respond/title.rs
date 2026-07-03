//! 会话标题生成与处理。
//! 调用 LLM 根据最近对话历史自动生成简短的中文会话标题，
//! 并提供标题清洗、截断、脏数据过滤等工具函数。

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::{
    provider::{
        LlmProvider,
        types::{ChatMessage, ChatRequest},
    },
    runtime::session::{DEFAULT_SESSION_TITLE, SessionMessage, redact_sensitive_text},
};

// 生成标题时使用的最近对话历史最大字符数
const TITLE_INPUT_CHAR_LIMIT: usize = 800;
// LLM 生成的标题最大字符数
const GENERATED_TITLE_CHAR_LIMIT: usize = 16;
// 展示标题时截断的最大字符数
const DISPLAY_TITLE_CHAR_LIMIT: usize = 24;

/// 调用 LLM 根据最近对话历史生成会话标题。
/// 标题最多 16 个字符，若内容不足以命名则返回错误。
pub(super) async fn generate_session_title(
    provider: &dyn LlmProvider,
    model: &str,
    messages: &[SessionMessage],
    record_health: bool,
) -> Result<String> {
    let history_input = build_title_history_input(messages)?;
    let mut metadata = HashMap::from([("purpose".to_owned(), "session_title".to_owned())]);
    if !record_health {
        // 自动标题是主聊天后的尽力而为步骤，失败不能覆盖主回复的上游状态。
        metadata.insert("health_observation".to_owned(), "ignore".to_owned());
    }
    let request = ChatRequest {
        session_id: "session-title".to_owned(),
        model: Some(model.to_owned()),
        messages: vec![
            ChatMessage::system(
                "你是会话标题生成器。请根据最近对话生成一个简短中文标题。\
                只输出标题本身，不要解释，不要加引号、项目符号、前缀、后缀或换行。\
                标题最多16个汉字或字符。若内容不足以命名，输出：未命名会话。",
            ),
            ChatMessage::user(format!("最近对话：\n{history_input}\n\n请生成标题。")),
        ],
        context_budget: None,
        max_output_tokens: None,
        reasoning_effort: None,
        metadata,
    };
    let outcome = provider.chat(request).await?;

    clean_generated_title(&outcome.reply)
}

/// 将标题格式化为展示文本，空或脏数据时返回默认标题。
pub(super) fn display_session_title(title: Option<&str>) -> String {
    clean_display_title(title).unwrap_or_else(|| DEFAULT_SESSION_TITLE.to_owned())
}

/// 返回用于 LLM 上下文的会话标题。默认标题或脏数据返回 None。
pub(super) fn context_session_title(title: Option<&str>) -> Option<String> {
    let title = normalized_title(title?)?;
    if title == DEFAULT_SESSION_TITLE || is_dirty_title(&title) {
        None
    } else {
        Some(take_chars(&title, DISPLAY_TITLE_CHAR_LIMIT))
    }
}

/// 清洗 LLM 生成的原始标题，验证长度、默认值、脏数据。
fn clean_generated_title(raw: &str) -> Result<String> {
    let Some(title) = normalized_title(raw) else {
        bail!("generated title is empty");
    };
    if title == DEFAULT_SESSION_TITLE {
        bail!("generated title reports insufficient content");
    }
    if is_dirty_title(&title) {
        bail!("generated title contains unsupported structured content");
    }
    let title = take_chars(&title, GENERATED_TITLE_CHAR_LIMIT);
    if title.trim().is_empty() || title == DEFAULT_SESSION_TITLE || is_dirty_title(&title) {
        bail!("generated title is not usable");
    }
    Ok(title)
}

fn clean_display_title(title: Option<&str>) -> Option<String> {
    let title = normalized_title(title?)?;
    if is_dirty_title(&title) {
        None
    } else {
        Some(take_chars(&title, DISPLAY_TITLE_CHAR_LIMIT))
    }
}

fn build_title_history_input(messages: &[SessionMessage]) -> Result<String> {
    let mut total = 0usize;
    let mut selected_rev = Vec::new();

    for message in messages.iter().rev() {
        let Some(line) = title_history_line(message) else {
            continue;
        };
        let separator_len = if selected_rev.is_empty() { 0 } else { 1 };
        let Some(available) = TITLE_INPUT_CHAR_LIMIT.checked_sub(total + separator_len) else {
            break;
        };
        if available == 0 {
            break;
        }
        let line = take_chars(&line, available);
        if line.trim().is_empty() {
            continue;
        }
        total += separator_len + line.chars().count();
        selected_rev.push(line);
        if total >= TITLE_INPUT_CHAR_LIMIT {
            break;
        }
    }

    if selected_rev.is_empty() {
        bail!("no usable session history for title generation");
    }
    selected_rev.reverse();
    Ok(selected_rev.join("\n"))
}

fn title_history_line(message: &SessionMessage) -> Option<String> {
    let role = match message.role.as_str() {
        "user" => "用户",
        "assistant" => "助手",
        _ => return None,
    };
    let content = normalize_history_content(&message.content)?;
    Some(format!("{role}：{content}"))
}

fn normalize_history_content(content: &str) -> Option<String> {
    let content = redact_sensitive_text(content);
    let content = content.replace(['\r', '\n'], " ");
    let content = content.trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_owned())
    }
}

fn normalized_title(value: &str) -> Option<String> {
    let value = strip_wrapping_quotes(value.trim());
    let value = value.replace(['\r', '\n'], " ");
    let value = strip_wrapping_quotes(value.trim());
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn strip_wrapping_quotes(value: &str) -> &str {
    let mut text = value.trim();
    loop {
        let stripped = text
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                text.strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })
            .or_else(|| {
                text.strip_prefix('“')
                    .and_then(|value| value.strip_suffix('”'))
            })
            .or_else(|| {
                text.strip_prefix('‘')
                    .and_then(|value| value.strip_suffix('’'))
            });
        let Some(next) = stripped else {
            return text;
        };
        text = next.trim();
    }
}

/// 检测标题是否包含 QQ 富文本脏数据（如 faceType、CQ 码等）。
fn is_dirty_title(title: &str) -> bool {
    let lowered = title.to_ascii_lowercase();
    title.contains('<')
        || title.contains('>')
        || lowered.contains("facetype")
        || lowered.contains("faceid")
        || lowered.contains("ext=\"eyj")
        || lowered.contains("[cq:")
}

fn take_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::session::now_iso_cn;

    fn msg(role: &str, content: &str) -> SessionMessage {
        SessionMessage {
            role: role.to_owned(),
            content: content.to_owned(),
            ts: now_iso_cn(),
        }
    }

    #[test]
    fn display_title_rejects_dirty_qq_payloads() {
        for title in [
            "",
            "<faceType=1 faceId=2>",
            "faceId=123",
            r#"ext="eyJxxx""#,
            "[CQ:face,id=1]",
        ] {
            assert_eq!(display_session_title(Some(title)), DEFAULT_SESSION_TITLE);
        }
    }

    #[test]
    fn title_history_input_keeps_recent_messages_in_time_order() {
        let history = vec![
            msg("user", "旧消息"),
            msg("assistant", "旧回复"),
            msg("user", "最近问题"),
            msg("assistant", "最近回答"),
        ];

        let input = build_title_history_input(&history).unwrap();

        assert!(input.contains("用户：旧消息"));
        assert!(input.contains("助手：旧回复"));
        assert!(input.contains("用户：最近问题"));
        assert!(input.contains("助手：最近回答"));
        assert!(input.find("用户：旧消息") < input.find("用户：最近问题"));
        assert!(input.chars().count() <= TITLE_INPUT_CHAR_LIMIT);
    }

    #[test]
    fn clean_generated_title_rejects_default_and_dirty_text() {
        assert!(clean_generated_title(DEFAULT_SESSION_TITLE).is_err());
        assert!(clean_generated_title("<faceType=1>").is_err());
        assert_eq!(clean_generated_title("“部署排障”").unwrap(), "部署排障");
    }
}
