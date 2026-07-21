//! 会话压缩、历史保留与上下文预算的回归测试。

use super::*;

#[test]
fn compact_group_history_keeps_turn_actor_annotations() {
    let req = RespondRequest {
        purpose: RespondPurpose::Compact,
        session: serde_json::json!({
            "scope": "group",
            "summary": "",
            "history": [
                {
                    "role": "user",
                    "content": "我的昵称是什么",
                    "ts": "2026-07-15T10:00:00+08:00",
                    "turn_actor": {
                        "actor_ref": "actor_a",
                        "display_name": "初墨",
                        "display_name_source": "manual",
                        "group_member_role": "member",
                        "identity_source": "event"
                    }
                },
                {
                    "role": "assistant",
                    "content": "你的展示名是初墨",
                    "ts": "2026-07-15T10:00:01+08:00",
                    "turn_actor": {
                        "actor_ref": "actor_a",
                        "display_name": "初墨",
                        "display_name_source": "manual",
                        "group_member_role": "member",
                        "identity_source": "event"
                    }
                }
            ]
        }),
        ..Default::default()
    };

    let messages = build_respond_messages(&req);
    let prompt = messages.last().unwrap().content.as_str();

    assert!(prompt.contains("[历史发言人：actor_ref=actor_a"));
    assert!(prompt.contains("[机器人当时回复给：actor_ref=actor_a"));
    assert!(prompt.contains("展示名来源=manual"));
    assert!(prompt.contains("成员专属事实必须保留对应 actor_ref"));
    assert!(prompt.contains("不得把多个成员统一写成“用户”"));
    assert!(prompt.contains("展示名、身份声明、偏好、纠正和个人事项必须绑定对应 actor_ref"));
    assert!(prompt.contains("- actor_ref=actor_xxx"));
    assert!(prompt.contains("压缩后的摘要必须让下一轮仍能区分不同成员"));
}

#[test]
fn compact_guild_channel_history_is_actor_aware() {
    let req = RespondRequest {
        purpose: RespondPurpose::Compact,
        session: serde_json::json!({
            "scope": "guild_channel",
            "summary": "",
            "history": [{
                "role": "user",
                "content": "频道消息",
                "ts": "2026-07-15T10:00:00+08:00",
                "turn_actor": { "actor_ref": "actor_guild_a" }
            }]
        }),
        ..Default::default()
    };

    let messages = build_respond_messages(&req);
    let prompt = messages.last().unwrap().content.as_str();

    assert!(prompt.contains("[历史发言人：actor_ref=actor_guild_a"));
    assert!(prompt.contains("成员事实："));
    assert!(prompt.contains("成员专属事实必须保留对应 actor_ref"));
}

#[test]
fn compact_private_history_keeps_single_user_format() {
    let req = RespondRequest {
        purpose: RespondPurpose::Compact,
        session: serde_json::json!({
            "scope": "private",
            "summary": "",
            "history": [{
                "role": "user",
                "content": "私聊消息",
                "ts": "2026-07-15T10:00:00+08:00",
                "turn_actor": { "actor_ref": "actor_should_not_render" }
            }]
        }),
        ..Default::default()
    };

    let messages = build_respond_messages(&req);
    let prompt = messages.last().unwrap().content.as_str();

    assert!(prompt.contains("user: 私聊消息"));
    assert!(!prompt.contains("[历史发言人："));
    assert!(!prompt.contains("成员事实："));
    assert!(!prompt.contains("actor_should_not_render"));
}

#[test]
fn budgeted_chat_messages_handles_non_standard_history_sequences() {
    let req = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".to_owned(),
        system_prompts: vec!["固定 prompt".to_owned()],
        history_messages: vec![
            ChatMessage {
                role: ChatRole::Assistant,
                content: "孤立助手".to_owned(),
                content_parts: Vec::new(),
            },
            ChatMessage::user("连续用户一"),
            ChatMessage::user("连续用户二"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "连续用户后的助手".to_owned(),
                content_parts: Vec::new(),
            },
        ],
        ..Default::default()
    };

    let messages = budget_chat_messages(
        &req,
        ContextBudgetConfig {
            context_window_chars: 10_000,
            output_reserve_chars: 100,
            protected_recent_turns: 2,
        },
        true,
    )
    .unwrap();

    assert_eq!(
        message_contents_with_time_marker(&messages),
        vec![
            "固定 prompt",
            "孤立助手",
            "连续用户一",
            "连续用户二",
            "连续用户后的助手",
            "<time_context>",
            "当前问题",
        ]
    );
}

fn cache_layout_request() -> RespondRequest {
    RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".to_owned(),
        system_prompts: vec!["固定 prompt".to_owned()],
        history_summary: "稳定摘要".to_owned(),
        history_messages: vec![
            ChatMessage::user("历史用户"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "历史助手".to_owned(),
                content_parts: Vec::new(),
            },
        ],
        ..Default::default()
    }
}

#[test]
fn different_request_times_only_change_bytes_after_stable_history_prefix() {
    let req = cache_layout_request();
    let offset = qq_maid_common::time_context::shanghai_offset();
    let first_time =
        RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 7, 21, 10, 0, 0).unwrap());
    let second_time =
        RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 7, 21, 10, 1, 0).unwrap());

    let first = build_chat_messages_with_time_context(&req, true, &first_time);
    let second = build_chat_messages_with_time_context(&req, true, &second_time);

    assert_eq!(
        serde_json::to_vec(&first[..4]).unwrap(),
        serde_json::to_vec(&second[..4]).unwrap()
    );
    assert_ne!(first[4].content, second[4].content);
}

#[test]
fn dynamic_context_changes_do_not_rewrite_summary_or_history_prefix() {
    let baseline = cache_layout_request();
    let offset = qq_maid_common::time_context::shanghai_offset();
    let time =
        RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 7, 21, 10, 0, 0).unwrap());

    for mutate in [
        |req: &mut RespondRequest| req.knowledge_context = "新知识".to_owned(),
        |req: &mut RespondRequest| req.memory_context = "新记忆".to_owned(),
        |req: &mut RespondRequest| req.session_context = "新 session 状态".to_owned(),
    ] {
        let mut changed = baseline.clone();
        mutate(&mut changed);
        let first = build_chat_messages_with_time_context(&baseline, true, &time);
        let second = build_chat_messages_with_time_context(&changed, true, &time);

        assert_eq!(
            serde_json::to_vec(&first[..4]).unwrap(),
            serde_json::to_vec(&second[..4]).unwrap()
        );
        assert_ne!(first, second);
    }
}

#[test]
fn budgeted_chat_messages_protect_stable_summary_while_evicting_dynamic_context() {
    let long_text = "预算淘汰内容".repeat(80);
    let baseline = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".to_owned(),
        system_prompts: vec!["稳定 system".to_owned()],
        history_summary: "稳定历史摘要".to_owned(),
        knowledge_context: format!("动态知识 {long_text}"),
        memory_context: format!("动态记忆 {long_text}"),
        session_context: format!("动态 session {long_text}"),
        history_messages: vec![
            ChatMessage::user(format!("可淘汰旧用户 {long_text}")),
            ChatMessage {
                role: ChatRole::Assistant,
                content: format!("可淘汰旧助手 {long_text}"),
                content_parts: Vec::new(),
            },
            ChatMessage::user("受保护最近用户"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "受保护最近助手".to_owned(),
                content_parts: Vec::new(),
            },
        ],
        ..Default::default()
    };
    let protected_groups = [
        vec![ChatMessage::system("稳定 system")],
        vec![ChatMessage::system("稳定历史摘要")],
        vec![
            normalize_user_message_for_provider(ChatMessage::user("受保护最近用户")),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "受保护最近助手".to_owned(),
                content_parts: Vec::new(),
            },
        ],
        vec![ChatMessage::system(llm_time_context_prompt(
            &request_time_context(),
        ))],
        vec![normalize_user_message_for_provider(
            ChatMessage::user_with_parts("当前问题", current_user_parts_for_model(&baseline, true)),
        )],
    ];
    let protected_chars = protected_groups
        .iter()
        .map(|messages| estimated_json_chars(messages, "context_budget").unwrap())
        .sum::<usize>();
    let config = ContextBudgetConfig {
        context_window_chars: protected_chars + 50,
        output_reserve_chars: 50,
        protected_recent_turns: 1,
    };

    for (kind, changed_context) in [
        ("knowledge", "变化后的动态知识".repeat(90)),
        ("memory", "变化后的动态记忆".repeat(90)),
        ("session", "变化后的动态 session".repeat(90)),
    ] {
        let mut req = baseline.clone();
        match kind {
            "knowledge" => req.knowledge_context = changed_context,
            "memory" => req.memory_context = changed_context,
            "session" => req.session_context = changed_context,
            _ => unreachable!(),
        }

        let messages = budget_chat_messages(&req, config, true).unwrap();
        let contents = message_contents_with_time_marker(&messages);

        assert_eq!(&contents[..2], ["稳定 system", "稳定历史摘要"]);
        assert!(contents.iter().any(|content| content == "受保护最近用户"));
        assert!(contents.iter().any(|content| content == "受保护最近助手"));
        assert!(!contents.iter().any(|content| content.contains("可淘汰旧")));
        assert!(!contents.iter().any(|content| content.contains("动态知识")));
        assert!(!contents.iter().any(|content| content.contains("动态记忆")));
        assert!(
            !contents
                .iter()
                .any(|content| content.contains("动态 session"))
        );
    }
}

#[test]
fn cache_diagnostics_never_retain_prompt_or_context_bodies() {
    let mut req = cache_layout_request();
    req.history_summary = "私密摘要正文".to_owned();
    req.knowledge_context = "知识正文不能进日志".to_owned();
    req.memory_context = "记忆正文不能进日志".to_owned();
    req.session_context = "session 正文不能进日志".to_owned();
    let messages = build_respond_messages(&req);

    let diagnostics = prompt_cache_diagnostics(&req, &messages, None).unwrap();
    let debug = format!("{diagnostics:?}");

    for secret in [
        "固定 prompt",
        "历史用户",
        "历史助手",
        "当前问题",
        "私密摘要正文",
        "知识正文不能进日志",
        "记忆正文不能进日志",
        "session 正文不能进日志",
    ] {
        assert!(!debug.contains(secret), "diagnostics leaked `{secret}`");
    }
}

#[test]
fn llm_time_context_prompt_is_built_in_llm_layer() {
    let offset = qq_maid_common::time_context::shanghai_offset();
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
