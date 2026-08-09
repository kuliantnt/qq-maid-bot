use super::*;

#[test]
fn budgeted_chat_messages_keep_order_when_under_limit() {
    let req = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".to_owned(),
        system_prompts: vec!["固定 prompt".to_owned()],
        knowledge_context: "知识片段".to_owned(),
        memory_context: "长期记忆".to_owned(),
        session_context: "会话摘要".to_owned(),
        history_messages: vec![
            ChatMessage::user("历史用户"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "历史助手".to_owned(),
                content_parts: Vec::new(),
            },
        ],
        ..Default::default()
    };

    let messages = budget_chat_messages(
        &req,
        ContextBudgetConfig {
            context_window_chars: 20_000,
            output_reserve_chars: 100,
            protected_recent_turns: 1,
        },
        true,
    )
    .unwrap();

    assert_eq!(
        message_contents_with_time_marker(&messages),
        vec![
            "固定 prompt",
            "历史用户",
            "历史助手",
            "知识片段",
            "长期记忆",
            "会话摘要",
            "<time_context>",
            "当前问题",
        ]
    );
}

#[test]
fn budgeted_chat_messages_evict_old_history_before_recent_turns() {
    let long_text = "很长的上下文".repeat(80);
    let req = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".to_owned(),
        system_prompts: vec!["固定 prompt".to_owned()],
        knowledge_context: format!("知识片段 {long_text}"),
        memory_context: "长期记忆：保留这个偏好".to_owned(),
        session_context: format!("会话摘要 {long_text}"),
        history_messages: vec![
            ChatMessage::user(format!("旧用户 {long_text}")),
            ChatMessage {
                role: ChatRole::Assistant,
                content: format!("旧助手 {long_text}"),
                content_parts: Vec::new(),
            },
            ChatMessage::user("最近用户".to_owned()),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "最近助手".to_owned(),
                content_parts: Vec::new(),
            },
        ],
        ..Default::default()
    };

    let messages = budget_chat_messages(
        &req,
        ContextBudgetConfig {
            context_window_chars: 600,
            output_reserve_chars: 50,
            protected_recent_turns: 1,
        },
        true,
    )
    .unwrap();
    let contents = messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();

    assert!(contents.iter().any(|content| content == &"固定 prompt"));
    assert!(
        contents
            .iter()
            .any(|content| content.contains("当前本地日期"))
    );
    assert!(
        contents
            .iter()
            .any(|content| content == &"长期记忆：保留这个偏好")
    );
    assert!(contents.iter().any(|content| content == &"最近用户"));
    assert!(contents.iter().any(|content| content == &"最近助手"));
    assert!(contents.iter().any(|content| content == &"当前问题"));
    assert!(!contents.iter().any(|content| content.contains("旧用户")));
    assert!(!contents.iter().any(|content| content.contains("知识片段")));
    assert!(!contents.iter().any(|content| content.contains("会话摘要")));
}

#[test]
fn budgeted_chat_messages_evict_old_turns_from_oldest_to_newest() {
    let long_text = "历史内容".repeat(35);
    let req = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".to_owned(),
        system_prompts: vec!["固定 prompt".to_owned()],
        history_messages: vec![
            ChatMessage::user(format!("旧一用户 {long_text}")),
            ChatMessage {
                role: ChatRole::Assistant,
                content: format!("旧一助手 {long_text}"),
                content_parts: Vec::new(),
            },
            ChatMessage::user(format!("旧二用户 {long_text}")),
            ChatMessage {
                role: ChatRole::Assistant,
                content: format!("旧二助手 {long_text}"),
                content_parts: Vec::new(),
            },
            ChatMessage::user("最近用户".to_owned()),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "最近助手".to_owned(),
                content_parts: Vec::new(),
            },
        ],
        ..Default::default()
    };

    let messages = budget_chat_messages(
        &req,
        ContextBudgetConfig {
            context_window_chars: 900,
            output_reserve_chars: 50,
            protected_recent_turns: 1,
        },
        true,
    )
    .unwrap();
    let contents = message_contents_with_time_marker(&messages);

    assert!(!contents.iter().any(|content| content.contains("旧一")));
    assert!(contents.iter().any(|content| content.contains("旧二用户")));
    assert!(contents.iter().any(|content| content.contains("旧二助手")));
    assert!(contents.iter().any(|content| content == "最近用户"));
    assert!(contents.iter().any(|content| content == "最近助手"));
}

#[test]
fn budgeted_chat_messages_keep_actor_labels_on_retained_group_history() {
    let long_text = "需要裁剪的旧历史".repeat(45);
    let recent_user = "[历史发言人：actor_ref=actor_b，展示名=小B，展示名来源=manual]\n我的昵称呢";
    let recent_assistant =
        "[机器人当时回复给：actor_ref=actor_b，展示名=小B，展示名来源=manual]\n你的展示名是小B";
    let req = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".to_owned(),
        system_prompts: vec!["固定 prompt".to_owned()],
        history_messages: vec![
            ChatMessage::user(format!("[历史发言人：actor_ref=actor_a]\n{long_text}")),
            ChatMessage {
                role: ChatRole::Assistant,
                content: format!("[机器人当时回复给：actor_ref=actor_a]\n{long_text}"),
                content_parts: Vec::new(),
            },
            ChatMessage::user(recent_user),
            ChatMessage {
                role: ChatRole::Assistant,
                content: recent_assistant.to_owned(),
                content_parts: Vec::new(),
            },
        ],
        ..Default::default()
    };

    let messages = budget_chat_messages(
        &req,
        ContextBudgetConfig {
            context_window_chars: 620,
            output_reserve_chars: 50,
            protected_recent_turns: 1,
        },
        true,
    )
    .unwrap();
    let contents = message_contents_with_time_marker(&messages);

    assert!(!contents.iter().any(|content| content.contains("actor_a")));
    assert!(contents.iter().any(|content| content == recent_user));
    assert!(contents.iter().any(|content| content == recent_assistant));
}

#[test]
fn budgeted_chat_messages_evict_context_kinds_after_history() {
    let long_text = "扩展上下文".repeat(45);
    let req = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".to_owned(),
        system_prompts: vec!["固定 prompt".to_owned()],
        knowledge_context: format!("知识片段 {long_text}"),
        session_context: format!("会话摘要 {long_text}"),
        memory_context: format!("长期记忆 {long_text}"),
        history_messages: vec![ChatMessage::user(format!("旧用户 {long_text}"))],
        ..Default::default()
    };

    let messages = budget_chat_messages(
        &req,
        ContextBudgetConfig {
            context_window_chars: 500,
            output_reserve_chars: 50,
            protected_recent_turns: 0,
        },
        true,
    )
    .unwrap();
    let contents = message_contents_with_time_marker(&messages);

    assert!(!contents.iter().any(|content| content.contains("旧用户")));
    assert!(!contents.iter().any(|content| content.contains("知识片段")));
    assert!(!contents.iter().any(|content| content.contains("会话摘要")));
    assert!(!contents.iter().any(|content| content.contains("长期记忆")));
    assert_eq!(contents[0], "固定 prompt");
    assert_eq!(contents[1], "<time_context>");
    assert_eq!(contents.last().map(String::as_str), Some("当前问题"));
}

#[test]
fn budgeted_chat_messages_return_error_when_required_part_exceeds_limit() {
    let req = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".repeat(80),
        system_prompts: vec!["固定 prompt".repeat(80)],
        history_messages: vec![ChatMessage::user("旧历史".repeat(80))],
        ..Default::default()
    };

    let err = budget_chat_messages(
        &req,
        ContextBudgetConfig {
            context_window_chars: 260,
            output_reserve_chars: 50,
            protected_recent_turns: 0,
        },
        true,
    )
    .unwrap_err();

    assert_eq!(err.code, "context_budget_exceeded");
    assert_eq!(err.stage, "context_budget");
}

#[test]
fn build_respond_messages_without_context_budget_keeps_unbudgeted_order() {
    let req = RespondRequest {
        purpose: RespondPurpose::Chat,
        user_text: "当前问题".to_owned(),
        system_prompts: vec!["固定 prompt".to_owned()],
        knowledge_context: "知识片段".to_owned(),
        memory_context: "长期记忆".to_owned(),
        session_context: "会话摘要".to_owned(),
        history_messages: vec![
            ChatMessage::user("连续用户一"),
            ChatMessage::user("连续用户二"),
        ],
        ..Default::default()
    };

    let messages = build_respond_messages(&req);

    assert_eq!(
        message_contents_with_time_marker(&messages),
        vec![
            "固定 prompt",
            "连续用户一",
            "连续用户二",
            "知识片段",
            "长期记忆",
            "会话摘要",
            "<time_context>",
            "当前问题",
        ]
    );
}
