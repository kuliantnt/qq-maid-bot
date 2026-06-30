use super::*;
// `strip_markdown_for_chat` 已提取到 `markdown_strip` 模块，这里显式引入，
// 因为 `use super::*` 不会带入父模块的私有 `use` 导入。
use super::strip_markdown_for_chat;
use crate::{provider::types::TokenUsage, util::metrics::LlmMetrics};
use chrono::TimeZone;

#[test]
fn strip_markdown_removes_chat_decoration() {
    let text = "# 标题\n- A\n`code`\n[link](https://example.test)";
    let stripped = strip_markdown_for_chat(text);
    assert!(stripped.contains("标题"));
    assert!(stripped.contains("· A"));
    assert!(stripped.contains("code"));
    assert!(stripped.contains("link（https://example.test）"));
}

#[test]
fn structured_chat_reply_returns_markdown_and_plaintext_channels() {
    let reply = "# 文档\n- item";
    let (text, markdown) = format_chat_reply_channels(reply);

    assert_eq!(text, "文档\n· item");
    assert_eq!(markdown.as_deref(), Some("# 文档\n- item"));
}

#[test]
fn plain_chat_reply_only_returns_text_channel() {
    let reply = "普通回复";
    let (text, markdown) = format_chat_reply_channels(reply);

    assert_eq!(text, "普通回复");
    assert_eq!(markdown.as_deref(), Some("普通回复"));
}

#[test]
fn structured_chat_reply_keeps_code_blocks_in_plaintext() {
    let reply = "```rust\nfn main() {}\n```";
    let (text, markdown) = format_chat_reply_channels(reply);

    assert_eq!(text, "fn main() {}");
    assert_eq!(markdown.as_deref(), Some("```rust\nfn main() {}\n```"));
}

#[test]
fn structured_chat_reply_keeps_link_title_and_url_in_plaintext() {
    let reply = "[OpenAI](https://openai.com)";
    let (text, markdown) = format_chat_reply_channels(reply);

    assert_eq!(text, "OpenAI（https://openai.com）");
    assert_eq!(markdown.as_deref(), Some("[OpenAI](https://openai.com)"));
}

#[test]
fn strip_markdown_keeps_fenced_code_symbols_untouched() {
    let reply = "```rust\nfn main() { println!(\"*_#[]()\"); }\n```";
    assert_eq!(
        strip_markdown_for_chat(reply),
        "fn main() { println!(\"*_#[]()\"); }"
    );
}

#[test]
fn strip_markdown_keeps_inline_code() {
    let reply = "执行 `cargo test -p qq-maid-core` 再看。";
    assert_eq!(
        strip_markdown_for_chat(reply),
        "执行 cargo test -p qq-maid-core 再看。"
    );
}

#[test]
fn strip_markdown_keeps_links_with_underscores_and_parentheses() {
    let reply = "[wiki](https://example.test/Function_(mathematics)?q=a_b#part_(1))";
    assert_eq!(
        strip_markdown_for_chat(reply),
        "wiki（https://example.test/Function_(mathematics)?q=a_b#part_(1)）"
    );
}

#[test]
fn strip_markdown_uses_image_alt_text_without_bang_marker() {
    let reply = "![流程图](https://example.test/a_(b).png)";
    assert_eq!(
        strip_markdown_for_chat(reply),
        "流程图（https://example.test/a_(b).png）"
    );
}

#[test]
fn strip_markdown_keeps_nested_lists_and_paragraphs_split() {
    let reply = "- 第一项\n  - 子项 A\n  - 子项 B\n\n第二段";
    assert_eq!(
        strip_markdown_for_chat(reply),
        "· 第一项\n  · 子项 A\n  · 子项 B\n\n第二段"
    );
}

#[test]
fn strip_markdown_flattens_tables_without_collapsing_lines() {
    let reply = "| 名称 | 状态 |\n| --- | --- |\n| RSS | 正常 |\n| Memory | 待确认 |";
    assert_eq!(
        strip_markdown_for_chat(reply),
        "名称 / 状态\nRSS / 正常\nMemory / 待确认"
    );
}

#[test]
fn strip_markdown_keeps_quotes_emphasis_and_mixed_language() {
    let reply = "> **中文** and *English* __Mixed__ _text_";
    assert_eq!(
        strip_markdown_for_chat(reply),
        "中文 and English Mixed text"
    );
}

#[test]
fn strip_markdown_removes_escape_noise() {
    let reply = "\\*不是列表\\*，\\_也不是斜体\\_";
    assert_eq!(strip_markdown_for_chat(reply), "*不是列表*，_也不是斜体_");
}

#[test]
fn memory_draft_is_cleaned() {
    assert_eq!(
        clean_memory_draft_output("记忆草稿：需要礼貌确认前台。"),
        "需要礼貌确认前台"
    );
}

#[test]
fn respond_response_only_exposes_text_for_python() {
    let chat = ChatResponse::ok(
        "raw",
        LlmMetrics {
            provider: "mock".to_owned(),
            model: "mock".to_owned(),
            stream: true,
            ttfe_ms: Some(1),
            ttft_ms: Some(2),
            total_latency_ms: 3,
        },
        Some(TokenUsage {
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            total_tokens: None,
        }),
    );
    let response = RespondResponse::from_chat(chat, Some("reply".to_owned()), None);
    let json = serde_json::to_value(response).unwrap();
    assert_eq!(json["text"], "reply");
    assert!(json.get("markdown").is_none());
    assert!(json.get("reply").is_none());
    assert!(json.get("raw_reply").is_none());
    assert!(json.get("deltas").is_none());
}

#[test]
fn respond_messages_include_request_time_context_once() {
    let req = RespondRequest {
        session_id: "group:g1".to_owned(),
        purpose: RespondPurpose::Chat,
        user_text: "今天有什么安排".to_owned(),
        system_prompts: vec!["角色设定".to_owned(), "固定规则".to_owned()],
        memory_context: String::new(),
        session_context: String::new(),
        history_messages: Vec::new(),
        session: serde_json::Value::Null,
        metadata: std::collections::HashMap::new(),
        ..Default::default()
    };

    let messages = build_respond_messages(&req);

    assert_eq!(messages[0].role, ChatRole::System);
    assert_eq!(messages[0].content, "角色设定");
    assert_eq!(messages[1].content, "固定规则");
    assert!(messages[2].content.contains("当前本地日期："));
    assert!(messages[2].content.contains("当前时区：Asia/Shanghai"));
    assert!(messages[2].content.contains("不要自行猜测当前日期"));
}

#[test]
fn chat_messages_keep_stable_system_prefix_before_time_context() {
    let req = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "继续".to_owned(),
        system_prompts: vec!["固定 prompt".to_owned(), "成员映射".to_owned()],
        knowledge_context: "知识片段".to_owned(),
        memory_context: "长期记忆".to_owned(),
        session_context: "会话上下文".to_owned(),
        history_messages: vec![
            ChatMessage::user("上一轮用户"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "上一轮助手".to_owned(),
            },
        ],
        ..Default::default()
    };

    let messages = build_respond_messages(&req);
    let contents = messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        contents,
        vec![
            "固定 prompt",
            "成员映射",
            messages[2].content.as_str(),
            "知识片段",
            "长期记忆",
            "会话上下文",
            "上一轮用户",
            "上一轮助手",
            "继续",
        ]
    );
    assert!(messages[2].content.contains("请求时间上下文："));
}

#[test]
fn llm_time_context_prompt_is_built_in_llm_layer() {
    let offset = crate::util::time_context::shanghai_offset();
    let ctx =
        RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 6, 9, 18, 40, 0).unwrap());

    let prompt = llm_time_context_prompt(&ctx);

    assert!(prompt.contains("当前本地日期：2026-06-09"));
    assert!(prompt.contains("当前本地时间：2026-06-09 18:40:00"));
    assert!(prompt.contains("当前时区：Asia/Shanghai"));
    assert!(prompt.contains("不要自行猜测当前日期"));
}

#[test]
fn request_time_context_is_not_duplicated() {
    let existing = ChatMessage::system(
        "请求时间上下文：\n当前本地日期：2026-06-09\n当前时区：Asia/Shanghai\n不要自行猜测当前日期",
    );
    let messages = with_request_time_context(vec![existing.clone(), ChatMessage::user("hi")]);

    assert_eq!(messages[0], existing);
    assert_eq!(messages.len(), 2);
}

#[test]
fn todo_parse_keeps_single_time_context_in_user_instruction() {
    let req = RespondRequest {
        purpose: RespondPurpose::TodoParse,
        user_text: "明天提醒我".to_owned(),
        metadata: std::collections::HashMap::from([(
            "todo_operation".to_owned(),
            "add".to_owned(),
        )]),
        ..Default::default()
    };

    let messages = build_respond_messages(&req);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, ChatRole::System);
    assert!(!messages[0].content.contains("请求时间上下文："));
    assert_eq!(messages[1].role, ChatRole::User);
    assert!(messages[1].content.contains("当前本地日期："));
}

#[test]
fn trace_text_redacts_secret_like_content() {
    let text = "OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz123456";
    let traced = trace_text(text);

    assert!(traced.contains("<redacted>") || traced.contains("<redacted:openai_api_key>"));
    assert!(!traced.contains("abcdefghijklmnopqrstuvwxyz123456"));
}

#[test]
fn trace_text_truncates_long_content() {
    let text = "甲".repeat(CHAT_TRACE_TEXT_LIMIT + 20);
    let traced = trace_text(&text);

    assert!(traced.ends_with('…'));
    assert!(traced.chars().count() <= CHAT_TRACE_TEXT_LIMIT);
}
