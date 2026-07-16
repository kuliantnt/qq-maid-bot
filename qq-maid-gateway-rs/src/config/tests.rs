use super::*;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// 带必填 credentials 的 env 构造 helper，消除 5 处重复的 ("QQ_BOT_APP_ID", ...) 输入。
fn env_with_creds(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    let mut map = env(&[("QQ_BOT_APP_ID", "appid"), ("QQ_BOT_APP_SECRET", "secret")]);
    for (k, v) in pairs {
        map.insert((*k).to_owned(), (*v).to_owned());
    }
    map
}

#[test]
fn loads_defaults_with_bound_credentials() {
    let config = AppConfig::from_map(&env(&[
        ("QQ_BOT_APP_ID", "appid"),
        ("QQ_BOT_APP_SECRET", "secret"),
    ]))
    .unwrap();

    assert_eq!(config.app_id.as_deref(), Some("appid"));
    assert_eq!(config.app_secret.as_deref(), Some("secret"));
    assert!(config.qq_official_enabled);
    assert_eq!(
        config.qq_official_binding_state(),
        QqOfficialBindingState::Enabled
    );
    assert!(!config.sandbox);
    assert_eq!(config.api_base, DEFAULT_PROD_API_BASE);
    assert_eq!(
        config.token_refresh_margin,
        Duration::from_secs(DEFAULT_TOKEN_REFRESH_MARGIN_SECONDS)
    );
    assert!(config.enable_markdown);
    assert!(!config.enable_image);
    assert!(config.bot_mention_ids.is_empty());
    assert!(!config.enable_group_messages);
    assert!(!config.verbose_log);
    // 成员详情补全默认开启（生产生效），失败降级不阻断主回复。
    assert!(config.member_detail_enrich_enabled);
    assert_eq!(config.group_message_mode, GroupMessageMode::Mention);
    assert_eq!(config.group_active_keywords, vec!["小女仆"]);
    assert_eq!(config.media_dir, PathBuf::from(DEFAULT_MEDIA_DIR));
    assert_eq!(
        config.media_download_timeout,
        Duration::from_millis(DEFAULT_MEDIA_DOWNLOAD_TIMEOUT_MS)
    );
    assert_eq!(config.media_max_bytes, DEFAULT_MEDIA_MAX_BYTES);
    assert_eq!(
        config.conversation_queue_capacity,
        DEFAULT_CONVERSATION_QUEUE_CAPACITY
    );
    assert_eq!(
        config.max_active_conversation_workers,
        DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS
    );
    assert_eq!(
        config.conversation_worker_idle_timeout,
        Duration::from_secs(DEFAULT_CONVERSATION_WORKER_IDLE_TIMEOUT_SECS)
    );
    assert_eq!(
        config.message_aggregation,
        MessageAggregationConfig {
            private_enabled: true,
            group_enabled: false,
            quiet: Duration::from_millis(DEFAULT_MESSAGE_AGGREGATION_QUIET_MS),
            max_wait: Duration::from_millis(DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS),
            max_messages: DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES,
            max_chars: DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
            max_active_keys: DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS,
        }
    );
    assert_eq!(
        config.c2c_final_reply_stream_enabled,
        DEFAULT_C2C_FINAL_REPLY_STREAM_ENABLED
    );
    assert_eq!(
        config.c2c_visible_progress_status_enabled,
        DEFAULT_C2C_VISIBLE_PROGRESS_STATUS_ENABLED
    );
    assert_eq!(
        config.agent_typing,
        AgentTypingConfig {
            enabled: DEFAULT_AGENT_TYPING_ENABLED,
            delay: Duration::from_millis(DEFAULT_AGENT_TYPING_DELAY_MS),
        }
    );
    assert_eq!(
        config.markdown_chunk_soft_limit,
        DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT
    );
    assert_eq!(config.text_chunk_soft_limit, DEFAULT_TEXT_CHUNK_SOFT_LIMIT);
    assert_eq!(
        config.wechat_service,
        WechatServiceConfig {
            enabled: false,
            token: None,
            app_id: None,
            app_secret: None,
            encryption_mode: WechatServiceEncryptionMode::Plaintext,
            encoding_aes_key: None,
            bind_host: DEFAULT_WECHAT_SERVICE_BIND_HOST.to_owned(),
            bind_port: DEFAULT_WECHAT_SERVICE_BIND_PORT,
            callback_path: DEFAULT_WECHAT_SERVICE_CALLBACK_PATH.to_owned(),
            reply_timeout: Duration::from_millis(DEFAULT_WECHAT_SERVICE_REPLY_TIMEOUT_MS),
            api_base: DEFAULT_WECHAT_SERVICE_API_BASE.to_owned(),
        }
    );
    assert_eq!(config.onebot11, OneBot11Config::default());
}

#[test]
fn loads_onebot11_single_account_config_without_qq_credentials() {
    let config = AppConfig::from_map(&env(&[
        ("QQ_BOT_ENABLED", "false"),
        ("ONEBOT11_ENABLED", "true"),
        ("ONEBOT11_ACCESS_TOKEN", "onebot-token"),
        ("ONEBOT11_BIND_HOST", "0.0.0.0"),
        ("ONEBOT11_BIND_PORT", "19091"),
        ("ONEBOT11_WEBSOCKET_PATH", "/reverse/ws"),
        ("ONEBOT11_REQUEST_TIMEOUT_MS", "2500"),
        ("ONEBOT11_MAX_MESSAGE_BYTES", "2097152"),
    ]))
    .unwrap();

    assert_eq!(
        config.qq_official_binding_state(),
        QqOfficialBindingState::Unbound
    );
    assert_eq!(
        config.onebot11,
        OneBot11Config {
            enabled: true,
            bind_host: "0.0.0.0".to_owned(),
            bind_port: 19091,
            websocket_path: "/reverse/ws".to_owned(),
            access_token: Some("onebot-token".to_owned()),
            request_timeout: Duration::from_millis(2500),
            max_message_bytes: 2 * 1024 * 1024,
        }
    );
}

#[test]
fn rejects_invalid_onebot11_configuration() {
    let cases = [
        (
            env(&[("ONEBOT11_ENABLED", "true")]),
            ConfigError::MissingRequiredWhenEnabled {
                name: "ONEBOT11_ACCESS_TOKEN",
                enabled_by: "ONEBOT11_ENABLED",
            },
        ),
        (
            env(&[("ONEBOT11_WEBSOCKET_PATH", "reverse/ws")]),
            ConfigError::InvalidHttpPath {
                name: "ONEBOT11_WEBSOCKET_PATH",
                value: "reverse/ws".to_owned(),
            },
        ),
        (
            env(&[("ONEBOT11_WEBSOCKET_PATH", "/reverse/{account}")]),
            ConfigError::InvalidHttpPath {
                name: "ONEBOT11_WEBSOCKET_PATH",
                value: "/reverse/{account}".to_owned(),
            },
        ),
        (
            env(&[("ONEBOT11_BIND_PORT", "0")]),
            ConfigError::IntegerOutOfRange {
                name: "ONEBOT11_BIND_PORT",
                value: 0,
                min: 1,
                max: u16::MAX as u64,
            },
        ),
        (
            env(&[("ONEBOT11_REQUEST_TIMEOUT_MS", "99")]),
            ConfigError::IntegerOutOfRange {
                name: "ONEBOT11_REQUEST_TIMEOUT_MS",
                value: 99,
                min: 100,
                max: 120_000,
            },
        ),
        (
            env(&[("ONEBOT11_MAX_MESSAGE_BYTES", "512")]),
            ConfigError::IntegerOutOfRange {
                name: "ONEBOT11_MAX_MESSAGE_BYTES",
                value: 512,
                min: MIN_ONEBOT11_MAX_MESSAGE_BYTES,
                max: MAX_ONEBOT11_MAX_MESSAGE_BYTES,
            },
        ),
    ];

    for (map, expected) in cases {
        assert_eq!(AppConfig::from_map(&map).unwrap_err(), expected);
    }
}

#[test]
fn parses_group_message_mode() {
    for (raw, expected) in [
        ("off", GroupMessageMode::Off),
        ("command", GroupMessageMode::Command),
        ("mention", GroupMessageMode::Mention),
        ("active", GroupMessageMode::Active),
    ] {
        let config =
            AppConfig::from_map(&env_with_creds(&[("QQ_MAID_GROUP_MESSAGE_MODE", raw)])).unwrap();
        assert_eq!(config.group_message_mode, expected);
    }
}

#[test]
fn group_message_mode_prefers_new_variable_over_legacy_bool() {
    let config = AppConfig::from_map(&env_with_creds(&[
        ("QQ_MAID_GROUP_MESSAGE_MODE", "command"),
        ("QQ_MAID_ENABLE_GROUP_MESSAGES", "true"),
    ]))
    .unwrap();

    assert_eq!(config.group_message_mode, GroupMessageMode::Command);
}

#[test]
fn legacy_group_messages_bool_maps_to_active_or_off() {
    let enabled = AppConfig::from_map(&env_with_creds(&[(
        "QQ_MAID_ENABLE_GROUP_MESSAGES",
        "true",
    )]))
    .unwrap();
    let disabled = AppConfig::from_map(&env_with_creds(&[(
        "QQ_MAID_ENABLE_GROUP_MESSAGES",
        "false",
    )]))
    .unwrap();

    assert_eq!(enabled.group_message_mode, GroupMessageMode::Active);
    assert_eq!(disabled.group_message_mode, GroupMessageMode::Off);
}

#[test]
fn group_message_mode_defaults_to_mention_when_no_legacy_bool_is_set() {
    let config = AppConfig::from_map(&env_with_creds(&[])).unwrap();

    assert_eq!(config.group_message_mode, GroupMessageMode::Mention);
}

#[test]
fn group_active_keywords_default_to_maid_keyword_and_parse_csv() {
    let defaulted = AppConfig::from_map(&env_with_creds(&[])).unwrap();
    let custom = AppConfig::from_map(&env_with_creds(&[(
        "QQ_MAID_GROUP_ACTIVE_KEYWORDS",
        " 小女仆, bot ,  ,召唤",
    )]))
    .unwrap();

    assert_eq!(defaulted.group_active_keywords, vec!["小女仆"]);
    assert_eq!(custom.group_active_keywords, vec!["小女仆", "bot", "召唤"]);
    assert_eq!(defaulted.bot_display_name(), "小女仆");
    assert_eq!(custom.bot_display_name(), "小女仆");

    let renamed = AppConfig::from_map(&env_with_creds(&[(
        "QQ_MAID_GROUP_ACTIVE_KEYWORDS",
        " , 小助手, 助手, bot ",
    )]))
    .unwrap();
    assert_eq!(renamed.group_active_keywords, vec!["小助手", "助手", "bot"]);
    assert_eq!(renamed.bot_display_name(), "小助手");
}

#[test]
fn legacy_display_name_only_changes_bot_display_name() {
    let config = AppConfig::from_map(&env_with_creds(&[(
        "QQ_MAID_STATUS_DISPLAY_NAME",
        " 小管家 ",
    )]))
    .unwrap();

    assert_eq!(config.bot_display_name(), "小管家");
    assert_eq!(config.group_active_keywords, vec!["小女仆"]);
}

#[test]
fn explicit_empty_active_keywords_ignore_legacy_display_name() {
    let config = AppConfig::from_map(&env_with_creds(&[
        ("QQ_MAID_GROUP_ACTIVE_KEYWORDS", " , , "),
        ("QQ_MAID_STATUS_DISPLAY_NAME", "小管家"),
    ]))
    .unwrap();

    assert_eq!(config.bot_display_name(), "小女仆");
    assert_eq!(config.group_active_keywords, vec!["小女仆"]);
}

#[test]
fn bot_mention_ids_parse_csv() {
    let config = AppConfig::from_map(&env_with_creds(&[(
        "QQ_MAID_BOT_MENTION_IDS",
        " bot-openid, member-openid , ",
    )]))
    .unwrap();

    assert_eq!(config.bot_mention_ids, vec!["bot-openid", "member-openid"]);
}

#[test]
fn supports_legacy_qq_variable_aliases() {
    let config = AppConfig::from_map(&env(&[
        ("QQ_APPID", "old-appid"),
        ("QQ_SECRET", "old-secret"),
    ]))
    .unwrap();

    assert_eq!(config.app_id.as_deref(), Some("old-appid"));
    assert_eq!(config.app_secret.as_deref(), Some("old-secret"));
}

#[test]
fn primary_variables_win_over_aliases() {
    let config = AppConfig::from_map(&env(&[
        ("QQ_BOT_APP_ID", "new-appid"),
        ("QQ_BOT_APP_SECRET", "new-secret"),
        ("QQ_APPID", "old-appid"),
        ("QQ_SECRET", "old-secret"),
    ]))
    .unwrap();

    assert_eq!(config.app_id.as_deref(), Some("new-appid"));
    assert_eq!(config.app_secret.as_deref(), Some("new-secret"));
}

#[test]
fn loads_optional_values() {
    let config = AppConfig::from_map(&env_with_creds(&[
        ("QQ_BOT_SANDBOX", "yes"),
        ("QQ_BOT_API_BASE", "https://example.test/"),
        ("QQ_BOT_TOKEN_REFRESH_MARGIN_SECONDS", "120"),
        ("QQ_MAID_BOT_MENTION_IDS", "bot-openid,member-openid"),
        ("QQ_MAID_ENABLE_MARKDOWN", "true"),
        ("QQ_MAID_ENABLE_IMAGE", "1"),
        ("QQ_MAID_ENABLE_GROUP_MESSAGES", "yes"),
        ("QQ_MAID_GATEWAY_VERBOSE_LOG", "on"),
        ("CONVERSATION_QUEUE_CAPACITY", "24"),
        ("MAX_ACTIVE_CONVERSATION_WORKERS", "96"),
        ("CONVERSATION_WORKER_IDLE_TIMEOUT_SECS", "600"),
        ("MESSAGE_AGGREGATION_PRIVATE_ENABLED", "false"),
        ("MESSAGE_AGGREGATION_QUIET_MS", "800"),
        ("MESSAGE_AGGREGATION_MAX_WAIT_MS", "2000"),
        ("MESSAGE_AGGREGATION_MAX_MESSAGES", "5"),
        ("MESSAGE_AGGREGATION_MAX_CHARS", "4096"),
        ("MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS", "32"),
        ("QQ_MAID_C2C_FINAL_REPLY_STREAM_ENABLED", "true"),
        ("QQ_MAID_C2C_VISIBLE_PROGRESS_STATUS_ENABLED", "false"),
        ("QQ_MAID_AGENT_TYPING_ENABLED", "false"),
        ("QQ_MAID_AGENT_TYPING_DELAY_MS", "1500"),
        ("QQ_MAID_MEDIA_MAX_BYTES", "1048576"),
        ("QQ_MARKDOWN_CHUNK_SOFT_LIMIT", "1600"),
        ("QQ_TEXT_CHUNK_SOFT_LIMIT", "1500"),
        ("WECHAT_SERVICE_ENABLED", "true"),
        ("WECHAT_SERVICE_TOKEN", "wechat-token"),
        ("WECHAT_SERVICE_APP_ID", "wx-app"),
        ("WECHAT_SERVICE_APP_SECRET", "wx-secret"),
        ("WECHAT_SERVICE_ENCRYPTION_MODE", "aes"),
        (
            "WECHAT_SERVICE_ENCODING_AES_KEY",
            "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
        ),
        ("WECHAT_SERVICE_BIND_HOST", "0.0.0.0"),
        ("WECHAT_SERVICE_BIND_PORT", "19090"),
        ("WECHAT_SERVICE_CALLBACK_PATH", "/wx/callback"),
        ("WECHAT_SERVICE_REPLY_TIMEOUT_MS", "3500"),
        (
            "WECHAT_SERVICE_API_BASE",
            "https://wechat-api.example.test/",
        ),
    ]))
    .unwrap();

    assert!(config.sandbox);
    assert_eq!(config.api_base, "https://example.test");
    assert_eq!(config.token_refresh_margin, Duration::from_secs(120));
    assert_eq!(config.bot_mention_ids, vec!["bot-openid", "member-openid"]);
    assert!(config.enable_markdown);
    assert!(config.enable_image);
    assert!(config.enable_group_messages);
    assert!(config.verbose_log);
    assert_eq!(config.conversation_queue_capacity, 24);
    assert_eq!(config.max_active_conversation_workers, 96);
    assert_eq!(
        config.conversation_worker_idle_timeout,
        Duration::from_secs(600)
    );
    assert_eq!(
        config.message_aggregation,
        MessageAggregationConfig {
            private_enabled: false,
            group_enabled: false,
            quiet: Duration::from_millis(800),
            max_wait: Duration::from_millis(2000),
            max_messages: 5,
            max_chars: 4096,
            max_active_keys: 32,
        }
    );
    assert!(config.c2c_final_reply_stream_enabled);
    assert!(!config.c2c_visible_progress_status_enabled);
    assert_eq!(
        config.agent_typing,
        AgentTypingConfig {
            enabled: false,
            delay: Duration::from_millis(1500),
        }
    );
    assert_eq!(config.markdown_chunk_soft_limit, 1600);
    assert_eq!(config.text_chunk_soft_limit, 1500);
    assert_eq!(config.media_max_bytes, 1_048_576);
    assert_eq!(
        config.wechat_service,
        WechatServiceConfig {
            enabled: true,
            token: Some("wechat-token".to_owned()),
            app_id: Some("wx-app".to_owned()),
            app_secret: Some("wx-secret".to_owned()),
            encryption_mode: WechatServiceEncryptionMode::Aes,
            encoding_aes_key: Some("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG".to_owned(),),
            bind_host: "0.0.0.0".to_owned(),
            bind_port: 19090,
            callback_path: "/wx/callback".to_owned(),
            reply_timeout: Duration::from_millis(3500),
            api_base: "https://wechat-api.example.test".to_owned(),
        }
    );
}

#[test]
fn qq_official_binding_states_are_explicit_and_keep_wechat_independent() {
    let unbound = AppConfig::from_map(&env(&[
        ("WECHAT_SERVICE_ENABLED", "true"),
        ("WECHAT_SERVICE_TOKEN", "wechat-token"),
    ]))
    .unwrap();
    assert_eq!(
        unbound.qq_official_binding_state(),
        QqOfficialBindingState::Unbound
    );
    assert!(unbound.enabled_qq_official_credentials().is_none());
    assert!(unbound.wechat_service.enabled);

    let disabled = AppConfig::from_map(&env_with_creds(&[("QQ_BOT_ENABLED", "false")])).unwrap();
    assert_eq!(
        disabled.qq_official_binding_state(),
        QqOfficialBindingState::Disabled
    );
    assert!(disabled.enabled_qq_official_credentials().is_none());

    let enabled = AppConfig::from_map(&env_with_creds(&[])).unwrap();
    assert_eq!(
        enabled.enabled_qq_official_credentials(),
        Some(("appid", "secret"))
    );
}

/// 合并 config 错误路径测试为表驱动测试。
#[test]
fn config_errors_reported() {
    struct Case {
        name: &'static str,
        map: HashMap<String, String>,
        expected_err: ConfigError,
    }

    let cases = [
        Case {
            name: "rejects_app_id_without_secret",
            map: env(&[("QQ_BOT_APP_ID", "appid")]),
            expected_err: ConfigError::IncompleteQqOfficialCredentials {
                missing: "QQ_BOT_APP_SECRET",
            },
        },
        Case {
            name: "rejects_secret_without_app_id",
            map: env(&[("QQ_BOT_APP_SECRET", "secret")]),
            expected_err: ConfigError::IncompleteQqOfficialCredentials {
                missing: "QQ_BOT_APP_ID",
            },
        },
        Case {
            name: "rejects_invalid_verbose_log_boolean",
            map: env_with_creds(&[("QQ_MAID_GATEWAY_VERBOSE_LOG", "sometimes")]),
            expected_err: ConfigError::InvalidBool {
                name: "QQ_MAID_GATEWAY_VERBOSE_LOG",
                value: "sometimes".to_owned(),
            },
        },
        Case {
            name: "rejects_zero_conversation_queue_capacity",
            map: env_with_creds(&[("CONVERSATION_QUEUE_CAPACITY", "0")]),
            expected_err: ConfigError::IntegerOutOfRange {
                name: "CONVERSATION_QUEUE_CAPACITY",
                value: 0,
                min: 1,
                max: 256,
            },
        },
        Case {
            name: "rejects_group_message_aggregation",
            map: env_with_creds(&[("MESSAGE_AGGREGATION_GROUP_ENABLED", "true")]),
            expected_err: ConfigError::UnsupportedEnabled {
                name: "MESSAGE_AGGREGATION_GROUP_ENABLED",
            },
        },
        Case {
            name: "rejects_media_max_bytes_below_minimum",
            map: env_with_creds(&[("QQ_MAID_MEDIA_MAX_BYTES", "1024")]),
            expected_err: ConfigError::IntegerOutOfRange {
                name: "QQ_MAID_MEDIA_MAX_BYTES",
                value: 1024,
                min: MIN_MEDIA_MAX_BYTES,
                max: MAX_MEDIA_MAX_BYTES,
            },
        },
        Case {
            name: "rejects_quiet_window_larger_than_max_wait",
            map: env_with_creds(&[
                ("MESSAGE_AGGREGATION_QUIET_MS", "3001"),
                ("MESSAGE_AGGREGATION_MAX_WAIT_MS", "3000"),
            ]),
            expected_err: ConfigError::InvalidAggregationWindow,
        },
        Case {
            name: "rejects_chunk_soft_limit_below_minimum",
            map: env_with_creds(&[("QQ_MARKDOWN_CHUNK_SOFT_LIMIT", "16")]),
            expected_err: ConfigError::IntegerOutOfRange {
                name: "QQ_MARKDOWN_CHUNK_SOFT_LIMIT",
                value: 16,
                min: MIN_CHUNK_SOFT_LIMIT as u64,
                max: 64_000,
            },
        },
        Case {
            name: "rejects_agent_typing_delay_below_minimum",
            map: env_with_creds(&[("QQ_MAID_AGENT_TYPING_DELAY_MS", "10")]),
            expected_err: ConfigError::IntegerOutOfRange {
                name: "QQ_MAID_AGENT_TYPING_DELAY_MS",
                value: 10,
                min: 100,
                max: 60_000,
            },
        },
        Case {
            name: "requires_wechat_token_when_enabled",
            map: env_with_creds(&[("WECHAT_SERVICE_ENABLED", "true")]),
            expected_err: ConfigError::MissingRequiredWhenEnabled {
                name: "WECHAT_SERVICE_TOKEN",
                enabled_by: "WECHAT_SERVICE_ENABLED",
            },
        },
        Case {
            name: "rejects_relative_wechat_callback_path",
            map: env_with_creds(&[
                ("WECHAT_SERVICE_ENABLED", "true"),
                ("WECHAT_SERVICE_TOKEN", "token"),
                ("WECHAT_SERVICE_CALLBACK_PATH", "wechat"),
            ]),
            expected_err: ConfigError::InvalidHttpPath {
                name: "WECHAT_SERVICE_CALLBACK_PATH",
                value: "wechat".to_owned(),
            },
        },
        Case {
            name: "rejects_too_large_wechat_reply_timeout",
            map: env_with_creds(&[
                ("WECHAT_SERVICE_ENABLED", "true"),
                ("WECHAT_SERVICE_TOKEN", "token"),
                ("WECHAT_SERVICE_REPLY_TIMEOUT_MS", "5000"),
            ]),
            expected_err: ConfigError::IntegerOutOfRange {
                name: "WECHAT_SERVICE_REPLY_TIMEOUT_MS",
                value: 5000,
                min: 500,
                max: 4500,
            },
        },
        Case {
            name: "rejects_unknown_wechat_encryption_mode",
            map: env_with_creds(&[
                ("WECHAT_SERVICE_ENABLED", "true"),
                ("WECHAT_SERVICE_TOKEN", "token"),
                ("WECHAT_SERVICE_ENCRYPTION_MODE", "compatibility"),
            ]),
            expected_err: ConfigError::InvalidWechatServiceEncryptionMode {
                value: "compatibility".to_owned(),
            },
        },
        Case {
            name: "requires_wechat_app_id_in_aes_mode",
            map: env_with_creds(&[
                ("WECHAT_SERVICE_ENABLED", "true"),
                ("WECHAT_SERVICE_TOKEN", "token"),
                ("WECHAT_SERVICE_ENCRYPTION_MODE", "aes"),
                (
                    "WECHAT_SERVICE_ENCODING_AES_KEY",
                    "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
                ),
            ]),
            expected_err: ConfigError::MissingRequiredForWechatAesMode {
                name: "WECHAT_SERVICE_APP_ID",
            },
        },
        Case {
            name: "requires_wechat_encoding_aes_key_in_aes_mode",
            map: env_with_creds(&[
                ("WECHAT_SERVICE_ENABLED", "true"),
                ("WECHAT_SERVICE_TOKEN", "token"),
                ("WECHAT_SERVICE_APP_ID", "wx-app"),
                ("WECHAT_SERVICE_ENCRYPTION_MODE", "aes"),
            ]),
            expected_err: ConfigError::MissingRequiredForWechatAesMode {
                name: "WECHAT_SERVICE_ENCODING_AES_KEY",
            },
        },
        Case {
            name: "rejects_invalid_wechat_encoding_aes_key",
            map: env_with_creds(&[
                ("WECHAT_SERVICE_ENABLED", "true"),
                ("WECHAT_SERVICE_TOKEN", "token"),
                ("WECHAT_SERVICE_APP_ID", "wx-app"),
                ("WECHAT_SERVICE_ENCRYPTION_MODE", "aes"),
                ("WECHAT_SERVICE_ENCODING_AES_KEY", "too-short"),
            ]),
            expected_err: ConfigError::InvalidWechatServiceEncodingAesKey,
        },
    ];

    for case in &cases {
        let err = match AppConfig::from_map(&case.map) {
            Err(e) => e,
            Ok(_) => panic!("case '{}' failed: expected Err, got Ok", case.name),
        };
        assert_eq!(
            err, case.expected_err,
            "case '{}' failed: error mismatch",
            case.name
        );
    }
}

#[test]
fn parses_verbose_log_boolean_values() {
    for raw in ["true", "1", "yes", "on"] {
        let config =
            AppConfig::from_map(&env_with_creds(&[("QQ_MAID_GATEWAY_VERBOSE_LOG", raw)])).unwrap();
        assert!(config.verbose_log, "{raw} should enable verbose logging");
    }

    for raw in ["false", "0", "no", "off"] {
        let config =
            AppConfig::from_map(&env_with_creds(&[("QQ_MAID_GATEWAY_VERBOSE_LOG", raw)])).unwrap();
        assert!(!config.verbose_log, "{raw} should disable verbose logging");
    }
}

#[test]
fn member_detail_enrich_enabled_defaults_true_and_can_be_disabled() {
    // 默认开启（生产生效）。
    let config = AppConfig::from_map(&env_with_creds(&[])).unwrap();
    assert!(config.member_detail_enrich_enabled);

    // 环境变量可关闭。
    let config = AppConfig::from_map(&env_with_creds(&[(
        "QQ_MAID_MEMBER_DETAIL_ENRICH_ENABLED",
        "false",
    )]))
    .unwrap();
    assert!(!config.member_detail_enrich_enabled);
}
