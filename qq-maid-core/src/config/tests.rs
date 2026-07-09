use super::*;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvSnapshot {
    values: Vec<(&'static str, Option<String>)>,
}

impl EnvSnapshot {
    fn capture(names: &[&'static str]) -> Self {
        Self {
            values: names
                .iter()
                .map(|name| (*name, env::var(name).ok()))
                .collect(),
        }
    }

    fn restore(self) {
        for (name, value) in self.values {
            restore_env(name, value);
        }
    }
}

#[test]
fn parse_provider_accepts_known_values() {
    assert_eq!(parse_provider("openai").unwrap(), ProviderMode::OpenAi);
    assert_eq!(parse_provider("DEEPSEEK").unwrap(), ProviderMode::DeepSeek);
    assert_eq!(parse_provider("bigmodel").unwrap(), ProviderMode::BigModel);
    assert_eq!(parse_provider("zhipu").unwrap(), ProviderMode::BigModel);
    assert_eq!(parse_provider("gemini").unwrap(), ProviderMode::Gemini);
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

#[test]
fn removed_member_id_mapping_env_returns_upgrade_error() {
    let _guard = ENV_LOCK.lock().unwrap();
    let snapshot = EnvSnapshot::capture(&["MEMBER_ID_MAPPING_FILE"]);
    unsafe {
        env::set_var("MEMBER_ID_MAPPING_FILE", "config/member_id_mapping.json");
    }

    let err = reject_removed_env_vars().unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("MEMBER_ID_MAPPING_FILE"));
    assert!(err.message.contains("removed"));
    assert!(err.message.contains("delete it from config/.env"));

    snapshot.restore();
}

#[test]
fn removed_member_id_mapping_env_allows_missing_or_empty_value() {
    let _guard = ENV_LOCK.lock().unwrap();
    let snapshot = EnvSnapshot::capture(&["MEMBER_ID_MAPPING_FILE"]);

    restore_env("MEMBER_ID_MAPPING_FILE", None);
    reject_removed_env_vars().unwrap();

    unsafe {
        env::set_var("MEMBER_ID_MAPPING_FILE", " ");
    }
    reject_removed_env_vars().unwrap();

    snapshot.restore();
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
fn openai_model_name_accepts_openai_gemini_prefix_and_bare_model() {
    assert_eq!(
        openai_model_name("openai:gpt-5.4-mini", "LLM_MODEL").unwrap(),
        "openai:gpt-5.4-mini"
    );
    assert_eq!(
        openai_model_name("gemini:gemini-2.5-flash", "OPENAI_SEARCH_MODEL").unwrap(),
        "gemini:gemini-2.5-flash"
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
    assert!(err.message.contains("supported: openai, gemini"));

    let err = openai_model_name("bigmodel:glm-5.2", "OPENAI_SEARCH_MODEL").unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("supported: openai, gemini"));
}

#[test]
fn openai_model_name_from_route_uses_first_openai_candidate() {
    assert_eq!(
        openai_model_name_from_route("deepseek:deepseek-chat, openai:gpt-5.4-mini").as_deref(),
        Some("openai:gpt-5.4-mini")
    );
    assert_eq!(
        openai_model_name_from_route("deepseek:deepseek-chat, gemini:gemini-2.5-flash").as_deref(),
        Some("gemini:gemini-2.5-flash")
    );
}

#[test]
fn openai_model_name_from_route_returns_none_without_openai_candidate() {
    assert_eq!(
        openai_model_name_from_route("deepseek:deepseek-chat, bigmodel:glm-5.2"),
        None
    );
}

#[test]
fn env_openai_model_or_falls_back_to_default_for_non_openai_main_route() {
    let _guard = ENV_LOCK.lock().unwrap();
    let snapshot = EnvSnapshot::capture(&["QQ_MAID_TEST_OPENAI_SEARCH_MODEL"]);
    restore_env("QQ_MAID_TEST_OPENAI_SEARCH_MODEL", None);

    let model = env_openai_model_or(
        "QQ_MAID_TEST_OPENAI_SEARCH_MODEL",
        "deepseek:deepseek-chat",
        DEFAULT_SEARCH_MODEL,
    )
    .unwrap();

    assert_eq!(model, DEFAULT_SEARCH_MODEL);
    snapshot.restore();
}

#[test]
fn env_openai_model_or_rejects_explicit_non_openai_search_model() {
    let _guard = ENV_LOCK.lock().unwrap();
    let snapshot = EnvSnapshot::capture(&["QQ_MAID_TEST_OPENAI_SEARCH_MODEL"]);
    unsafe {
        env::set_var("QQ_MAID_TEST_OPENAI_SEARCH_MODEL", "deepseek:deepseek-chat");
    }

    let err = env_openai_model_or(
        "QQ_MAID_TEST_OPENAI_SEARCH_MODEL",
        "deepseek:deepseek-chat",
        DEFAULT_SEARCH_MODEL,
    )
    .unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("supported: openai, gemini"));
    snapshot.restore();
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
fn status_display_name_defaults_and_reads_configured_value() {
    let _guard = ENV_LOCK.lock().unwrap();
    let snapshot = EnvSnapshot::capture(&["QQ_MAID_STATUS_DISPLAY_NAME"]);

    restore_env("QQ_MAID_STATUS_DISPLAY_NAME", None);
    assert_eq!(
        env_status_display_name().unwrap(),
        DEFAULT_STATUS_DISPLAY_NAME
    );

    unsafe {
        env::set_var("QQ_MAID_STATUS_DISPLAY_NAME", " 助手 ");
    }
    assert_eq!(env_status_display_name().unwrap(), "助手");

    snapshot.restore();
}

#[test]
fn status_display_name_rejects_overlong_value() {
    let _guard = ENV_LOCK.lock().unwrap();
    let snapshot = EnvSnapshot::capture(&["QQ_MAID_STATUS_DISPLAY_NAME"]);
    unsafe {
        env::set_var(
            "QQ_MAID_STATUS_DISPLAY_NAME",
            "非常非常非常非常非常非常非常非常非常非常非常非常长的称呼",
        );
    }

    let err = env_status_display_name().unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("QQ_MAID_STATUS_DISPLAY_NAME"));
    snapshot.restore();
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
fn sqlite_pool_size_uses_independent_bounded_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let snapshot = EnvSnapshot::capture(&["QQ_MAID_DB_POOL_MAX_SIZE", "MAX_CONCURRENT_RESPONSES"]);

    restore_env("QQ_MAID_DB_POOL_MAX_SIZE", None);
    unsafe {
        env::set_var("MAX_CONCURRENT_RESPONSES", "0");
    }
    assert_eq!(
        sqlite_pool_size_from_env().unwrap(),
        crate::storage::database::DEFAULT_SQLITE_POOL_SIZE
    );

    unsafe {
        env::set_var("QQ_MAID_DB_POOL_MAX_SIZE", "9");
    }
    assert_eq!(sqlite_pool_size_from_env().unwrap(), 9);

    unsafe {
        env::set_var("QQ_MAID_DB_POOL_MAX_SIZE", "0");
    }
    let err = sqlite_pool_size_from_env().unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("between 1 and 32"));

    unsafe {
        env::set_var("QQ_MAID_DB_POOL_MAX_SIZE", "33");
    }
    let err = sqlite_pool_size_from_env().unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("between 1 and 32"));

    snapshot.restore();
}

#[test]
fn context_budget_config_uses_default_values() {
    let _guard = ENV_LOCK.lock().unwrap();
    let names = [
        "AGENT_CONTEXT_CHAR_LIMIT",
        "AGENT_CONTEXT_OUTPUT_RESERVE_CHARS",
        "AGENT_CONTEXT_PROTECTED_RECENT_TURNS",
    ];
    let snapshot = EnvSnapshot::capture(&names);
    for name in names {
        restore_env(name, None);
    }

    let config = context_budget_from_env().unwrap();

    assert_eq!(
        config,
        ContextBudgetConfig {
            context_window_chars: DEFAULT_AGENT_CONTEXT_CHAR_LIMIT as usize,
            output_reserve_chars: DEFAULT_AGENT_CONTEXT_OUTPUT_RESERVE_CHARS as usize,
            protected_recent_turns: DEFAULT_AGENT_CONTEXT_PROTECTED_RECENT_TURNS as usize,
        }
    );

    snapshot.restore();
}

#[test]
fn context_budget_config_rejects_reserve_not_smaller_than_window() {
    let _guard = ENV_LOCK.lock().unwrap();
    let previous_limit = env::var("AGENT_CONTEXT_CHAR_LIMIT").ok();
    let previous_reserve = env::var("AGENT_CONTEXT_OUTPUT_RESERVE_CHARS").ok();
    let previous_turns = env::var("AGENT_CONTEXT_PROTECTED_RECENT_TURNS").ok();
    unsafe {
        env::set_var("AGENT_CONTEXT_CHAR_LIMIT", "100");
        env::set_var("AGENT_CONTEXT_OUTPUT_RESERVE_CHARS", "100");
        env::set_var("AGENT_CONTEXT_PROTECTED_RECENT_TURNS", "0");
    }

    let err = context_budget_from_env().unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("OUTPUT_RESERVE"));

    restore_env("AGENT_CONTEXT_CHAR_LIMIT", previous_limit);
    restore_env("AGENT_CONTEXT_OUTPUT_RESERVE_CHARS", previous_reserve);
    restore_env("AGENT_CONTEXT_PROTECTED_RECENT_TURNS", previous_turns);
}

#[test]
fn context_budget_config_handles_zero_invalid_and_out_of_range_values() {
    let _guard = ENV_LOCK.lock().unwrap();
    let names = [
        "AGENT_CONTEXT_CHAR_LIMIT",
        "AGENT_CONTEXT_OUTPUT_RESERVE_CHARS",
        "AGENT_CONTEXT_PROTECTED_RECENT_TURNS",
    ];
    let snapshot = EnvSnapshot::capture(&names);

    unsafe {
        env::set_var("AGENT_CONTEXT_CHAR_LIMIT", "0");
        env::set_var("AGENT_CONTEXT_OUTPUT_RESERVE_CHARS", "10");
        env::set_var("AGENT_CONTEXT_PROTECTED_RECENT_TURNS", "4");
    }
    let err = context_budget_from_env().unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("AGENT_CONTEXT_CHAR_LIMIT"));

    unsafe {
        env::set_var("AGENT_CONTEXT_CHAR_LIMIT", "120000");
        env::set_var("AGENT_CONTEXT_OUTPUT_RESERVE_CHARS", "not-a-number");
    }
    let err = context_budget_from_env().unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("AGENT_CONTEXT_OUTPUT_RESERVE_CHARS"));

    unsafe {
        env::set_var("AGENT_CONTEXT_OUTPUT_RESERVE_CHARS", "6000");
        env::set_var("AGENT_CONTEXT_PROTECTED_RECENT_TURNS", "65");
    }
    let err = context_budget_from_env().unwrap_err();
    assert_eq!(err.code, "config");
    assert!(err.message.contains("between 0 and 64"));

    snapshot.restore();
}

#[test]
fn context_budget_config_allows_zero_protected_recent_turns() {
    let _guard = ENV_LOCK.lock().unwrap();
    let names = [
        "AGENT_CONTEXT_CHAR_LIMIT",
        "AGENT_CONTEXT_OUTPUT_RESERVE_CHARS",
        "AGENT_CONTEXT_PROTECTED_RECENT_TURNS",
    ];
    let snapshot = EnvSnapshot::capture(&names);
    unsafe {
        env::set_var("AGENT_CONTEXT_CHAR_LIMIT", "1000");
        env::set_var("AGENT_CONTEXT_OUTPUT_RESERVE_CHARS", "100");
        env::set_var("AGENT_CONTEXT_PROTECTED_RECENT_TURNS", "0");
    }

    let config = context_budget_from_env().unwrap();

    assert_eq!(config.protected_recent_turns, 0);
    assert_eq!(config.effective_input_limit(), 900);

    snapshot.restore();
}

#[test]
fn tool_result_char_limit_is_not_context_budget_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let names = [
        "AGENT_CONTEXT_CHAR_LIMIT",
        "AGENT_CONTEXT_OUTPUT_RESERVE_CHARS",
        "AGENT_CONTEXT_PROTECTED_RECENT_TURNS",
        "AGENT_TOOL_RESULT_CHAR_LIMIT",
    ];
    let snapshot = EnvSnapshot::capture(&names);
    for name in names {
        restore_env(name, None);
    }
    unsafe {
        env::set_var("AGENT_TOOL_RESULT_CHAR_LIMIT", "1234");
    }

    let context_budget = context_budget_from_env().unwrap();
    let tool_result_max_chars = env_u64_bounded_range(
        "AGENT_TOOL_RESULT_CHAR_LIMIT",
        DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT,
        MIN_AGENT_TOOL_RESULT_CHAR_LIMIT,
        200_000,
    )
    .unwrap();

    assert_eq!(tool_result_max_chars, 1234);
    assert_eq!(
        context_budget,
        ContextBudgetConfig {
            context_window_chars: DEFAULT_AGENT_CONTEXT_CHAR_LIMIT as usize,
            output_reserve_chars: DEFAULT_AGENT_CONTEXT_OUTPUT_RESERVE_CHARS as usize,
            protected_recent_turns: DEFAULT_AGENT_CONTEXT_PROTECTED_RECENT_TURNS as usize,
        }
    );

    snapshot.restore();
}

#[test]
fn tool_result_char_limit_rejects_values_below_minimum() {
    let _guard = ENV_LOCK.lock().unwrap();
    let snapshot = EnvSnapshot::capture(&["AGENT_TOOL_RESULT_CHAR_LIMIT"]);
    unsafe {
        env::set_var(
            "AGENT_TOOL_RESULT_CHAR_LIMIT",
            (MIN_AGENT_TOOL_RESULT_CHAR_LIMIT - 1).to_string(),
        );
    }

    let err = env_u64_bounded_range(
        "AGENT_TOOL_RESULT_CHAR_LIMIT",
        DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT,
        MIN_AGENT_TOOL_RESULT_CHAR_LIMIT,
        200_000,
    )
    .unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("AGENT_TOOL_RESULT_CHAR_LIMIT"));
    assert!(
        err.message
            .contains(&MIN_AGENT_TOOL_RESULT_CHAR_LIMIT.to_string())
    );

    snapshot.restore();
}

fn restore_env(name: &str, value: Option<String>) {
    unsafe {
        if let Some(value) = value {
            env::set_var(name, value);
        } else {
            env::remove_var(name);
        }
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
fn env_example_documents_gemini_provider() {
    let env_example = include_str!("../../../runtime/config/.env.example");

    assert!(env_example.contains("GEMINI_API_KEY="));
    assert!(
        env_example
            .contains("GEMINI_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai")
    );
    assert!(env_example.contains("GEMINI_MODEL=gemini:gemini-2.5-flash"));
    assert!(env_example.contains("OPENAI_SEARCH_MODEL=gemini:gemini-2.5-flash"));
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
