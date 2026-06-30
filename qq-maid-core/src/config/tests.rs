use super::*;

#[test]
fn parse_provider_accepts_known_values() {
    assert_eq!(parse_provider("openai").unwrap(), ProviderMode::OpenAi);
    assert_eq!(parse_provider("DEEPSEEK").unwrap(), ProviderMode::DeepSeek);
    assert_eq!(parse_provider("bigmodel").unwrap(), ProviderMode::BigModel);
    assert_eq!(parse_provider("zhipu").unwrap(), ProviderMode::BigModel);
    assert_eq!(parse_provider("auto").unwrap(), ProviderMode::Auto);
}

#[test]
fn parse_provider_rejects_unknown_values() {
    let err = parse_provider("both").unwrap_err();
    assert_eq!(err.code, "config");
    assert_eq!(err.stage, "config");
}

#[test]
fn parse_openai_api_mode_accepts_known_values() {
    assert_eq!(parse_openai_api_mode("auto").unwrap(), OpenAiApiMode::Auto);
    assert_eq!(
        parse_openai_api_mode("CHAT_ONLY").unwrap(),
        OpenAiApiMode::ChatOnly
    );
    assert_eq!(
        parse_openai_api_mode("chat-only").unwrap(),
        OpenAiApiMode::ChatOnly
    );
}

#[test]
fn parse_openai_api_mode_rejects_unknown_values() {
    let err = parse_openai_api_mode("responses").unwrap_err();
    assert_eq!(err.code, "config");
    assert_eq!(err.stage, "config");
}

/// 合并 2 个 first_openai_base_url 测试为表驱动测试。
#[test]
fn openai_base_urls_resolve_precedence() {
    struct Case {
        name: &'static str,
        urls: Option<&'static str>,
        fallback: Option<&'static str>,
        expected: Option<&'static str>,
    }

    let cases = [
        Case {
            name: "openai_base_urls_take_precedence_over_single_base_url",
            urls: Some(" https://first.example/v1, https://second.example/v1 "),
            fallback: Some("https://single.example/v1"),
            expected: Some("https://first.example/v1"),
        },
        Case {
            name: "empty_openai_base_urls_falls_back_to_single_base_url",
            urls: Some(" , "),
            fallback: Some(" https://single.example/v1 "),
            expected: Some("https://single.example/v1"),
        },
    ];

    for case in &cases {
        let actual = first_openai_base_url(case.urls, case.fallback);
        assert_eq!(
            actual.as_deref(),
            case.expected,
            "case '{}' failed",
            case.name
        );
    }
}

#[test]
fn openai_model_name_accepts_openai_prefix_and_bare_model() {
    assert_eq!(
        openai_model_name("openai:gpt-5.4-mini", "LLM_MODEL").unwrap(),
        "gpt-5.4-mini"
    );
    assert_eq!(
        openai_model_name("gpt-5.4-mini", "OPENAI_SEARCH_MODEL").unwrap(),
        "gpt-5.4-mini"
    );
}

#[test]
fn openai_model_name_rejects_non_openai_prefix() {
    let err = openai_model_name("deepseek:deepseek-chat", "OPENAI_SEARCH_MODEL").unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("non-openai"));

    let err = openai_model_name("bigmodel:glm-5.2", "OPENAI_SEARCH_MODEL").unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("non-openai"));
}

#[test]
fn openai_model_name_from_route_uses_first_openai_candidate() {
    assert_eq!(
        openai_model_name_from_route("deepseek:deepseek-chat, openai:gpt-5.4-mini", "LLM_MODEL")
            .unwrap(),
        "gpt-5.4-mini"
    );
}

#[test]
fn env_model_string_rejects_explicit_empty_model() {
    let previous = env::var("QQ_MAID_TEST_LLM_MODEL").ok();
    unsafe {
        env::set_var("QQ_MAID_TEST_LLM_MODEL", "  ");
    }

    let err = env_model_string("QQ_MAID_TEST_LLM_MODEL", "fallback").unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("QQ_MAID_TEST_LLM_MODEL"));

    unsafe {
        if let Some(value) = previous {
            env::set_var("QQ_MAID_TEST_LLM_MODEL", value);
        } else {
            env::remove_var("QQ_MAID_TEST_LLM_MODEL");
        }
    }
}

#[test]
fn optional_model_accepts_candidate_route_and_rejects_invalid_route() {
    let previous = env::var("QQ_MAID_TEST_OPTIONAL_MODEL").ok();
    unsafe {
        env::set_var(
            "QQ_MAID_TEST_OPTIONAL_MODEL",
            "openai:gpt-5.4-mini, deepseek:deepseek-chat",
        );
    }
    assert_eq!(
        env_optional_model("QQ_MAID_TEST_OPTIONAL_MODEL")
            .unwrap()
            .as_deref(),
        Some("openai:gpt-5.4-mini, deepseek:deepseek-chat")
    );

    unsafe {
        env::set_var("QQ_MAID_TEST_OPTIONAL_MODEL", "openai:gpt,,deepseek:chat");
    }
    let err = env_optional_model("QQ_MAID_TEST_OPTIONAL_MODEL").unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("QQ_MAID_TEST_OPTIONAL_MODEL"));

    unsafe {
        if let Some(value) = previous {
            env::set_var("QQ_MAID_TEST_OPTIONAL_MODEL", value);
        } else {
            env::remove_var("QQ_MAID_TEST_OPTIONAL_MODEL");
        }
    }
}

#[test]
fn rss_summary_default_limit_is_500_unicode_chars() {
    assert_eq!(DEFAULT_RSS_SUMMARY_MAX_CHARS, 500);
}

#[test]
fn daily_reminder_time_parser_accepts_strict_hhmm() {
    assert_eq!(
        DailyReminderTime::parse_config("09:00", "TODO_DAILY_REMINDER_TIME").unwrap(),
        DailyReminderTime { hour: 9, minute: 0 }
    );
    assert_eq!(
        DailyReminderTime::parse_config("23:59", "TODO_DAILY_REMINDER_TIME").unwrap(),
        DailyReminderTime {
            hour: 23,
            minute: 59
        }
    );
}

#[test]
fn daily_reminder_time_parser_rejects_invalid_value() {
    for value in ["9:00", "24:00", "12:60", "ab:cd"] {
        let err = DailyReminderTime::parse_config(value, "TODO_DAILY_REMINDER_TIME").unwrap_err();
        assert_eq!(err.code, "config");
        assert!(err.message.contains("TODO_DAILY_REMINDER_TIME"));
    }
}

#[test]
fn daily_reminder_env_helpers_use_expected_defaults() {
    unsafe {
        env::remove_var("QQ_MAID_TEST_TODO_REMINDER_ENABLED");
        env::remove_var("QQ_MAID_TEST_TODO_REMINDER_TIME");
    }

    assert!(!env_bool("QQ_MAID_TEST_TODO_REMINDER_ENABLED", false).unwrap());
    assert_eq!(
        env_daily_reminder_time(
            "QQ_MAID_TEST_TODO_REMINDER_TIME",
            DEFAULT_TODO_DAILY_REMINDER_TIME
        )
        .unwrap(),
        DailyReminderTime { hour: 9, minute: 0 }
    );
}

#[test]
fn max_concurrent_responses_allows_zero_and_rejects_large_values() {
    unsafe {
        env::set_var("QQ_MAID_TEST_MAX_CONCURRENT_RESPONSES", "0");
    }
    assert_eq!(
        env_u64_bounded_zero_allowed("QQ_MAID_TEST_MAX_CONCURRENT_RESPONSES", 4, 256).unwrap(),
        0
    );

    unsafe {
        env::set_var("QQ_MAID_TEST_MAX_CONCURRENT_RESPONSES", "257");
    }
    let err =
        env_u64_bounded_zero_allowed("QQ_MAID_TEST_MAX_CONCURRENT_RESPONSES", 4, 256).unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("between 0 and 256"));

    unsafe {
        env::remove_var("QQ_MAID_TEST_MAX_CONCURRENT_RESPONSES");
    }
}

#[test]
fn tool_calling_defaults_are_enabled_and_bounded() {
    unsafe {
        env::remove_var("QQ_MAID_TEST_TOOL_CALLING_ENABLED");
        env::remove_var("QQ_MAID_TEST_TOOL_CALLING_MAX_ROUNDS");
    }

    assert!(env_bool("QQ_MAID_TEST_TOOL_CALLING_ENABLED", true).unwrap());
    assert_eq!(
        env_u64_bounded(
            "QQ_MAID_TEST_TOOL_CALLING_MAX_ROUNDS",
            DEFAULT_TOOL_CALLING_MAX_ROUNDS,
            8,
        )
        .unwrap(),
        DEFAULT_TOOL_CALLING_MAX_ROUNDS
    );

    unsafe {
        env::set_var("QQ_MAID_TEST_TOOL_CALLING_MAX_ROUNDS", "0");
    }
    let err = env_u64_bounded(
        "QQ_MAID_TEST_TOOL_CALLING_MAX_ROUNDS",
        DEFAULT_TOOL_CALLING_MAX_ROUNDS,
        8,
    )
    .unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("between 1 and 8"));

    unsafe {
        env::set_var("QQ_MAID_TEST_TOOL_CALLING_MAX_ROUNDS", "8");
    }
    assert_eq!(
        env_u64_bounded(
            "QQ_MAID_TEST_TOOL_CALLING_MAX_ROUNDS",
            DEFAULT_TOOL_CALLING_MAX_ROUNDS,
            8,
        )
        .unwrap(),
        8
    );

    unsafe {
        env::set_var("QQ_MAID_TEST_TOOL_CALLING_MAX_ROUNDS", "9");
    }
    let err = env_u64_bounded(
        "QQ_MAID_TEST_TOOL_CALLING_MAX_ROUNDS",
        DEFAULT_TOOL_CALLING_MAX_ROUNDS,
        8,
    )
    .unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("between 1 and 8"));

    unsafe {
        env::remove_var("QQ_MAID_TEST_TOOL_CALLING_MAX_ROUNDS");
    }
}

#[test]
fn env_example_documents_rss_summary_limit_default() {
    let env_example = include_str!("../../../runtime/config/.env.example");

    assert!(env_example.contains("RSS_SUMMARY_MAX_CHARS=500"));
}

#[test]
fn env_optional_trims_values_and_treats_empty_as_unset() {
    unsafe {
        env::set_var("QQ_MAID_TEST_OPTIONAL_VALUE", "  /tmp/knowledge  ");
        env::set_var("QQ_MAID_TEST_EMPTY_VALUE", "  \n ");
    }

    assert_eq!(
        env_optional("QQ_MAID_TEST_OPTIONAL_VALUE").as_deref(),
        Some("/tmp/knowledge")
    );
    assert_eq!(env_optional("QQ_MAID_TEST_EMPTY_VALUE"), None);

    unsafe {
        env::remove_var("QQ_MAID_TEST_OPTIONAL_VALUE");
        env::remove_var("QQ_MAID_TEST_EMPTY_VALUE");
    }
}

#[test]
fn translation_model_from_env_trims_and_treats_empty_as_unset() {
    let previous = env::var("TRANSLATION_MODEL").ok();
    unsafe {
        env::set_var("TRANSLATION_MODEL", "  deepseek:deepseek-chat  ");
    }
    assert_eq!(
        translation_model_from_env().unwrap().as_deref(),
        Some("deepseek:deepseek-chat")
    );

    unsafe {
        env::set_var("TRANSLATION_MODEL", "  ");
    }
    assert_eq!(translation_model_from_env().unwrap(), None);

    unsafe {
        if let Some(value) = previous {
            env::set_var("TRANSLATION_MODEL", value);
        } else {
            env::remove_var("TRANSLATION_MODEL");
        }
    }
}

#[test]
fn env_example_documents_knowledge_dir() {
    let env_example = include_str!("../../../runtime/config/.env.example");

    assert!(env_example.contains("KNOWLEDGE_DIR="));
}

#[test]
fn env_example_documents_translation_model() {
    let env_example = include_str!("../../../runtime/config/.env.example");

    assert!(env_example.contains("TRANSLATION_MODEL="));
}

#[test]
fn env_example_documents_bigmodel_provider() {
    let env_example = include_str!("../../../runtime/config/.env.example");

    assert!(env_example.contains("LLM_PROVIDER=auto"));
    assert!(env_example.contains("BIGMODEL_API_KEY="));
    assert!(env_example.contains("BIGMODEL_BASE_URL=https://open.bigmodel.cn/api/paas/v4"));
    assert!(env_example.contains("BIGMODEL_MODEL=bigmodel:glm-5.2"));
}

#[test]
fn env_example_documents_todo_daily_reminder() {
    let env_example = include_str!("../../../runtime/config/.env.example");

    assert!(env_example.contains("TODO_DAILY_REMINDER_ENABLED=false"));
    assert!(env_example.contains("TODO_DAILY_REMINDER_TIME=09:00"));
}

#[test]
fn env_required_rejects_missing_value() {
    unsafe {
        env::remove_var("QQ_MAID_TEST_REQUIRED_VALUE");
    }
    let err = env_required("QQ_MAID_TEST_REQUIRED_VALUE").unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("QQ_MAID_TEST_REQUIRED_VALUE"));
}
