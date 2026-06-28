//! 网关配置模块。从环境变量加载 QQ AppID、AppSecret、gateway API 地址和回调开关。

use std::{collections::HashMap, time::Duration};

use thiserror::Error;

pub const DEFAULT_PROD_API_BASE: &str = "https://api.sgroup.qq.com";
pub const DEFAULT_SANDBOX_API_BASE: &str = "https://sandbox.api.sgroup.qq.com";
pub const DEFAULT_TOKEN_REFRESH_MARGIN_SECONDS: u64 = 60;
pub const DEFAULT_GROUP_ACTIVE_KEYWORDS: &[&str] = &["小女仆"];
pub const DEFAULT_CONVERSATION_QUEUE_CAPACITY: usize = 16;
pub const DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS: usize = 64;
pub const DEFAULT_CONVERSATION_WORKER_IDLE_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMessageMode {
    Off,
    Command,
    Mention,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub app_id: String,
    pub app_secret: String,
    pub sandbox: bool,
    pub api_base: String,
    pub token_refresh_margin: Duration,
    pub enable_markdown: bool,
    pub enable_image: bool,
    pub enable_group_messages: bool,
    pub verbose_log: bool,
    pub group_message_mode: GroupMessageMode,
    pub group_active_keywords: Vec<String>,
    pub conversation_queue_capacity: usize,
    pub max_active_conversation_workers: usize,
    pub conversation_worker_idle_timeout: Duration,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    MissingRequired(&'static str),
    #[error("invalid boolean value for {name}: {value}")]
    InvalidBool { name: &'static str, value: String },
    #[error("invalid integer value for {name}: {value}")]
    InvalidInteger { name: &'static str, value: String },
    #[error("{name} must be between {min} and {max}, got {value}")]
    IntegerOutOfRange {
        name: &'static str,
        value: u64,
        min: u64,
        max: u64,
    },
    #[error("invalid group message mode: {value}")]
    InvalidGroupMessageMode { value: String },
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        // 独立调用 from_env 时也只按当前运行目录加载配置：
        // 先 config/.env，再 .env，保持与启动入口一致。
        let _ = dotenvy::from_path("config/.env");
        let _ = dotenvy::dotenv();
        let env = std::env::vars().collect::<HashMap<_, _>>();
        Self::from_map(&env)
    }

    pub fn from_map(env: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let app_id = required(env, "QQ_BOT_APP_ID", Some("QQ_APPID"))?;
        let app_secret = required(env, "QQ_BOT_APP_SECRET", Some("QQ_SECRET"))?;
        let sandbox = parse_bool(env, "QQ_BOT_SANDBOX")?.unwrap_or(false);
        let default_api_base = if sandbox {
            DEFAULT_SANDBOX_API_BASE
        } else {
            DEFAULT_PROD_API_BASE
        };
        let api_base = optional(env, "QQ_BOT_API_BASE")
            .unwrap_or_else(|| default_api_base.to_owned())
            .trim_end_matches('/')
            .to_owned();
        let margin_seconds = parse_u64(env, "QQ_BOT_TOKEN_REFRESH_MARGIN_SECONDS")?
            .unwrap_or(DEFAULT_TOKEN_REFRESH_MARGIN_SECONDS);
        let enable_markdown = parse_bool(env, "QQ_MAID_ENABLE_MARKDOWN")?.unwrap_or(true);
        let enable_image = parse_bool(env, "QQ_MAID_ENABLE_IMAGE")?.unwrap_or(false);
        let enable_group_messages =
            parse_bool(env, "QQ_MAID_ENABLE_GROUP_MESSAGES")?.unwrap_or(false);
        let verbose_log = parse_bool(env, "QQ_MAID_GATEWAY_VERBOSE_LOG")?.unwrap_or(false);
        let group_message_mode = parse_group_message_mode(env)?;
        let group_active_keywords = parse_csv(
            env,
            "QQ_MAID_GROUP_ACTIVE_KEYWORDS",
            DEFAULT_GROUP_ACTIVE_KEYWORDS,
        );
        let conversation_queue_capacity = parse_ranged_usize(
            env,
            "CONVERSATION_QUEUE_CAPACITY",
            DEFAULT_CONVERSATION_QUEUE_CAPACITY,
            1,
            256,
        )?;
        let max_active_conversation_workers = parse_ranged_usize(
            env,
            "MAX_ACTIVE_CONVERSATION_WORKERS",
            DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS,
            1,
            1024,
        )?;
        let conversation_worker_idle_timeout_seconds = parse_ranged_u64(
            env,
            "CONVERSATION_WORKER_IDLE_TIMEOUT_SECS",
            DEFAULT_CONVERSATION_WORKER_IDLE_TIMEOUT_SECS,
            10,
            3600,
        )?;
        Ok(Self {
            app_id,
            app_secret,
            sandbox,
            api_base,
            token_refresh_margin: Duration::from_secs(margin_seconds),
            enable_markdown,
            enable_image,
            enable_group_messages,
            verbose_log,
            group_message_mode,
            group_active_keywords,
            conversation_queue_capacity,
            max_active_conversation_workers,
            conversation_worker_idle_timeout: Duration::from_secs(
                conversation_worker_idle_timeout_seconds,
            ),
        })
    }
}

fn required(
    env: &HashMap<String, String>,
    name: &'static str,
    alias: Option<&'static str>,
) -> Result<String, ConfigError> {
    optional_with_alias(env, name, alias).ok_or(ConfigError::MissingRequired(name))
}

fn optional(env: &HashMap<String, String>, name: &'static str) -> Option<String> {
    env.get(name).map(|value| value.trim()).and_then(|value| {
        if value.is_empty() {
            None
        } else {
            Some(value.to_owned())
        }
    })
}

fn parse_csv(env: &HashMap<String, String>, name: &'static str, defaults: &[&str]) -> Vec<String> {
    optional(env, name)
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| defaults.iter().map(|value| (*value).to_owned()).collect())
}

fn optional_with_alias(
    env: &HashMap<String, String>,
    name: &'static str,
    alias: Option<&'static str>,
) -> Option<String> {
    optional(env, name).or_else(|| alias.and_then(|alias| optional(env, alias)))
}

fn parse_group_message_mode(
    env: &HashMap<String, String>,
) -> Result<GroupMessageMode, ConfigError> {
    if let Some(raw) = optional(env, "QQ_MAID_GROUP_MESSAGE_MODE") {
        return match raw.to_ascii_lowercase().as_str() {
            "off" => Ok(GroupMessageMode::Off),
            "command" => Ok(GroupMessageMode::Command),
            "mention" => Ok(GroupMessageMode::Mention),
            "active" => Ok(GroupMessageMode::Active),
            _ => Err(ConfigError::InvalidGroupMessageMode { value: raw }),
        };
    }

    Ok(match parse_bool(env, "QQ_MAID_ENABLE_GROUP_MESSAGES")? {
        Some(true) => GroupMessageMode::Active,
        Some(false) => GroupMessageMode::Off,
        // 未设置新旧群聊变量时，默认只响应命令、@ 和回复机器人消息。
        // 这样保持群聊可用，同时避免 active 模式对普通聊天自动插话。
        None => GroupMessageMode::Mention,
    })
}

fn parse_bool(
    env: &HashMap<String, String>,
    name: &'static str,
) -> Result<Option<bool>, ConfigError> {
    let Some(raw) = optional(env, name) else {
        return Ok(None);
    };
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "n" | "off" => Ok(Some(false)),
        _ => Err(ConfigError::InvalidBool { name, value: raw }),
    }
}

fn parse_u64(
    env: &HashMap<String, String>,
    name: &'static str,
) -> Result<Option<u64>, ConfigError> {
    let Some(raw) = optional(env, name) else {
        return Ok(None);
    };
    raw.parse::<u64>()
        .map(Some)
        .map_err(|_| ConfigError::InvalidInteger { name, value: raw })
}

fn parse_ranged_u64(
    env: &HashMap<String, String>,
    name: &'static str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, ConfigError> {
    let value = parse_u64(env, name)?.unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(ConfigError::IntegerOutOfRange {
            name,
            value,
            min,
            max,
        });
    }
    Ok(value)
}

fn parse_ranged_usize(
    env: &HashMap<String, String>,
    name: &'static str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, ConfigError> {
    Ok(parse_ranged_u64(env, name, default as u64, min as u64, max as u64)? as usize)
}

#[cfg(test)]
mod tests {
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
    fn loads_defaults_with_required_values() {
        let config = AppConfig::from_map(&env(&[
            ("QQ_BOT_APP_ID", "appid"),
            ("QQ_BOT_APP_SECRET", "secret"),
        ]))
        .unwrap();

        assert_eq!(config.app_id, "appid");
        assert_eq!(config.app_secret, "secret");
        assert!(!config.sandbox);
        assert_eq!(config.api_base, DEFAULT_PROD_API_BASE);
        assert_eq!(
            config.token_refresh_margin,
            Duration::from_secs(DEFAULT_TOKEN_REFRESH_MARGIN_SECONDS)
        );
        assert!(config.enable_markdown);
        assert!(!config.enable_image);
        assert!(!config.enable_group_messages);
        assert!(!config.verbose_log);
        assert_eq!(config.group_message_mode, GroupMessageMode::Mention);
        assert_eq!(config.group_active_keywords, vec!["小女仆"]);
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
                AppConfig::from_map(&env_with_creds(&[("QQ_MAID_GROUP_MESSAGE_MODE", raw)]))
                    .unwrap();
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
    }

    #[test]
    fn supports_legacy_qq_variable_aliases() {
        let config = AppConfig::from_map(&env(&[
            ("QQ_APPID", "old-appid"),
            ("QQ_SECRET", "old-secret"),
        ]))
        .unwrap();

        assert_eq!(config.app_id, "old-appid");
        assert_eq!(config.app_secret, "old-secret");
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

        assert_eq!(config.app_id, "new-appid");
        assert_eq!(config.app_secret, "new-secret");
    }

    #[test]
    fn loads_optional_values() {
        let config = AppConfig::from_map(&env_with_creds(&[
            ("QQ_BOT_SANDBOX", "yes"),
            ("QQ_BOT_API_BASE", "https://example.test/"),
            ("QQ_BOT_TOKEN_REFRESH_MARGIN_SECONDS", "120"),
            ("QQ_MAID_ENABLE_MARKDOWN", "true"),
            ("QQ_MAID_ENABLE_IMAGE", "1"),
            ("QQ_MAID_ENABLE_GROUP_MESSAGES", "yes"),
            ("QQ_MAID_GATEWAY_VERBOSE_LOG", "on"),
            ("CONVERSATION_QUEUE_CAPACITY", "24"),
            ("MAX_ACTIVE_CONVERSATION_WORKERS", "96"),
            ("CONVERSATION_WORKER_IDLE_TIMEOUT_SECS", "600"),
        ]))
        .unwrap();

        assert!(config.sandbox);
        assert_eq!(config.api_base, "https://example.test");
        assert_eq!(config.token_refresh_margin, Duration::from_secs(120));
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
    }

    /// 合并 2 个 config 错误路径测试为表驱动测试。
    #[test]
    fn config_errors_reported() {
        struct Case {
            name: &'static str,
            map: HashMap<String, String>,
            expected_err: ConfigError,
        }

        let cases = [
            Case {
                name: "requires_credentials",
                map: HashMap::new(),
                expected_err: ConfigError::MissingRequired("QQ_BOT_APP_ID"),
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
                AppConfig::from_map(&env_with_creds(&[("QQ_MAID_GATEWAY_VERBOSE_LOG", raw)]))
                    .unwrap();
            assert!(config.verbose_log, "{raw} should enable verbose logging");
        }

        for raw in ["false", "0", "no", "off"] {
            let config =
                AppConfig::from_map(&env_with_creds(&[("QQ_MAID_GATEWAY_VERBOSE_LOG", raw)]))
                    .unwrap();
            assert!(!config.verbose_log, "{raw} should disable verbose logging");
        }
    }
}
