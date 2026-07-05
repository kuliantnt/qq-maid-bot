//! 网关配置模块。从环境变量加载 QQ AppID、AppSecret、gateway API 地址和回调开关。

use std::{collections::HashMap, path::PathBuf, time::Duration};

use thiserror::Error;

pub const DEFAULT_PROD_API_BASE: &str = "https://api.sgroup.qq.com";
pub const DEFAULT_SANDBOX_API_BASE: &str = "https://sandbox.api.sgroup.qq.com";
pub const DEFAULT_TOKEN_REFRESH_MARGIN_SECONDS: u64 = 60;
pub const DEFAULT_GROUP_ACTIVE_KEYWORDS: &[&str] = &["小女仆"];
pub const DEFAULT_CONVERSATION_QUEUE_CAPACITY: usize = 16;
pub const DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS: usize = 64;
pub const DEFAULT_CONVERSATION_WORKER_IDLE_TIMEOUT_SECS: u64 = 300;
pub const DEFAULT_MESSAGE_AGGREGATION_QUIET_MS: u64 = 1200;
pub const DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS: u64 = 3000;
pub const DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES: usize = 10;
pub const DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS: usize = 12000;
pub const DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS: usize = 1024;
pub const DEFAULT_C2C_FINAL_REPLY_STREAM_ENABLED: bool = true;
pub const DEFAULT_C2C_VISIBLE_PROGRESS_STATUS_ENABLED: bool = true;
pub const DEFAULT_AGENT_TYPING_ENABLED: bool = true;
pub const DEFAULT_AGENT_TYPING_DELAY_MS: u64 = 1000;
/// 普通回复分段软限制默认值（非平台硬上限，仅保守软限制）。
/// 默认对齐官方非流式长消息分段的 5000 字符基线，尽量减少段数；
/// 真实 QQ 单条限制仍需真机验证后再校准。
pub const DEFAULT_TEXT_CHUNK_SOFT_LIMIT: usize = 5000;
pub const DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT: usize = 5000;
pub const DEFAULT_WECHAT_SERVICE_BIND_HOST: &str = "127.0.0.1";
pub const DEFAULT_WECHAT_SERVICE_BIND_PORT: u16 = 8788;
pub const DEFAULT_WECHAT_SERVICE_CALLBACK_PATH: &str = "/wechat/service";
pub const DEFAULT_WECHAT_SERVICE_REPLY_TIMEOUT_MS: u64 = 4000;
pub const DEFAULT_WECHAT_SERVICE_API_BASE: &str = "https://api.weixin.qq.com";
pub const DEFAULT_MEDIA_DIR: &str = "media/inbound";
pub const DEFAULT_MEDIA_DOWNLOAD_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_MEDIA_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const MIN_MEDIA_MAX_BYTES: u64 = 64 * 1024;
pub const MAX_MEDIA_MAX_BYTES: u64 = 100 * 1024 * 1024;
/// 分段软限制允许的下限；低于此值没有实际分段意义且无法容纳 synthetic fence。
pub const MIN_CHUNK_SOFT_LIMIT: usize = 64;

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
    pub bot_mention_ids: Vec<String>,
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
    pub message_aggregation: MessageAggregationConfig,
    /// 私聊 Agent 最终回复是否接入 QQ C2C Markdown 流式发送；默认启用，可关闭回滚。
    pub c2c_final_reply_stream_enabled: bool,
    /// 私聊 Tool Loop 是否发送一次可见进度提示；独立于 QQ 原生 typing 状态。
    pub c2c_visible_progress_status_enabled: bool,
    pub agent_typing: AgentTypingConfig,
    /// 普通回复 Markdown 通道分段软限制（非平台硬上限）。
    pub markdown_chunk_soft_limit: usize,
    /// 普通回复纯文本通道分段软限制（非平台硬上限）。
    pub text_chunk_soft_limit: usize,
    /// QQ 官方入站附件下载目录；相对路径按运行目录解析，不写入日志。
    pub media_dir: PathBuf,
    pub media_download_timeout: Duration,
    /// QQ 官方入站图片及本地 data URL 允许处理的单文件最大体积。
    pub media_max_bytes: u64,
    /// 微信服务号最小文本回调入口；默认关闭，不影响现有 QQ Gateway。
    pub wechat_service: WechatServiceConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WechatServiceConfig {
    pub enabled: bool,
    pub token: Option<String>,
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub bind_host: String,
    pub bind_port: u16,
    pub callback_path: String,
    pub reply_timeout: Duration,
    pub api_base: String,
}

impl Default for WechatServiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: None,
            app_id: None,
            app_secret: None,
            bind_host: DEFAULT_WECHAT_SERVICE_BIND_HOST.to_owned(),
            bind_port: DEFAULT_WECHAT_SERVICE_BIND_PORT,
            callback_path: DEFAULT_WECHAT_SERVICE_CALLBACK_PATH.to_owned(),
            reply_timeout: Duration::from_millis(DEFAULT_WECHAT_SERVICE_REPLY_TIMEOUT_MS),
            api_base: DEFAULT_WECHAT_SERVICE_API_BASE.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageAggregationConfig {
    pub private_enabled: bool,
    pub group_enabled: bool,
    pub quiet: Duration,
    pub max_wait: Duration,
    pub max_messages: usize,
    pub max_chars: usize,
    pub max_active_keys: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTypingConfig {
    pub enabled: bool,
    pub delay: Duration,
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
    #[error("{name} is not supported yet")]
    UnsupportedEnabled { name: &'static str },
    #[error("missing required environment variable {name} when {enabled_by}=true")]
    MissingRequiredWhenEnabled {
        name: &'static str,
        enabled_by: &'static str,
    },
    #[error("{name} must be an absolute HTTP path beginning with '/', got {value}")]
    InvalidHttpPath { name: &'static str, value: String },
    #[error(
        "MESSAGE_AGGREGATION_QUIET_MS must be less than or equal to MESSAGE_AGGREGATION_MAX_WAIT_MS"
    )]
    InvalidAggregationWindow,
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
        let bot_mention_ids = parse_csv(env, "QQ_MAID_BOT_MENTION_IDS", &[]);
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
        let message_aggregation = parse_message_aggregation_config(env)?;
        let c2c_final_reply_stream_enabled =
            parse_bool(env, "QQ_MAID_C2C_FINAL_REPLY_STREAM_ENABLED")?
                .unwrap_or(DEFAULT_C2C_FINAL_REPLY_STREAM_ENABLED);
        let c2c_visible_progress_status_enabled =
            parse_bool(env, "QQ_MAID_C2C_VISIBLE_PROGRESS_STATUS_ENABLED")?
                .unwrap_or(DEFAULT_C2C_VISIBLE_PROGRESS_STATUS_ENABLED);
        let agent_typing = parse_agent_typing_config(env)?;
        let markdown_chunk_soft_limit = parse_ranged_usize(
            env,
            "QQ_MARKDOWN_CHUNK_SOFT_LIMIT",
            DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT,
            MIN_CHUNK_SOFT_LIMIT,
            64_000,
        )?;
        let text_chunk_soft_limit = parse_ranged_usize(
            env,
            "QQ_TEXT_CHUNK_SOFT_LIMIT",
            DEFAULT_TEXT_CHUNK_SOFT_LIMIT,
            MIN_CHUNK_SOFT_LIMIT,
            64_000,
        )?;
        let media_dir = PathBuf::from(
            optional(env, "QQ_MAID_MEDIA_DIR").unwrap_or_else(|| DEFAULT_MEDIA_DIR.to_owned()),
        );
        let media_download_timeout = Duration::from_millis(parse_ranged_u64(
            env,
            "QQ_MAID_MEDIA_DOWNLOAD_TIMEOUT_MS",
            DEFAULT_MEDIA_DOWNLOAD_TIMEOUT_MS,
            100,
            120_000,
        )?);
        let media_max_bytes = parse_ranged_u64(
            env,
            "QQ_MAID_MEDIA_MAX_BYTES",
            DEFAULT_MEDIA_MAX_BYTES,
            MIN_MEDIA_MAX_BYTES,
            MAX_MEDIA_MAX_BYTES,
        )?;
        let wechat_service = parse_wechat_service_config(env)?;
        Ok(Self {
            app_id,
            app_secret,
            bot_mention_ids,
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
            message_aggregation,
            c2c_final_reply_stream_enabled,
            c2c_visible_progress_status_enabled,
            agent_typing,
            markdown_chunk_soft_limit,
            text_chunk_soft_limit,
            media_dir,
            media_download_timeout,
            media_max_bytes,
            wechat_service,
        })
    }
}

fn parse_wechat_service_config(
    env: &HashMap<String, String>,
) -> Result<WechatServiceConfig, ConfigError> {
    let enabled = parse_bool(env, "WECHAT_SERVICE_ENABLED")?.unwrap_or(false);
    let token = optional(env, "WECHAT_SERVICE_TOKEN");
    if enabled && token.is_none() {
        return Err(ConfigError::MissingRequiredWhenEnabled {
            name: "WECHAT_SERVICE_TOKEN",
            enabled_by: "WECHAT_SERVICE_ENABLED",
        });
    }
    let callback_path = optional(env, "WECHAT_SERVICE_CALLBACK_PATH")
        .unwrap_or_else(|| DEFAULT_WECHAT_SERVICE_CALLBACK_PATH.to_owned());
    if !callback_path.starts_with('/') {
        return Err(ConfigError::InvalidHttpPath {
            name: "WECHAT_SERVICE_CALLBACK_PATH",
            value: callback_path,
        });
    }
    Ok(WechatServiceConfig {
        enabled,
        token,
        app_id: optional(env, "WECHAT_SERVICE_APP_ID"),
        app_secret: optional(env, "WECHAT_SERVICE_APP_SECRET"),
        bind_host: optional(env, "WECHAT_SERVICE_BIND_HOST")
            .unwrap_or_else(|| DEFAULT_WECHAT_SERVICE_BIND_HOST.to_owned()),
        bind_port: parse_u16(env, "WECHAT_SERVICE_BIND_PORT")?
            .unwrap_or(DEFAULT_WECHAT_SERVICE_BIND_PORT),
        callback_path,
        reply_timeout: Duration::from_millis(parse_ranged_u64(
            env,
            "WECHAT_SERVICE_REPLY_TIMEOUT_MS",
            DEFAULT_WECHAT_SERVICE_REPLY_TIMEOUT_MS,
            500,
            4500,
        )?),
        api_base: optional(env, "WECHAT_SERVICE_API_BASE")
            .unwrap_or_else(|| DEFAULT_WECHAT_SERVICE_API_BASE.to_owned())
            .trim_end_matches('/')
            .to_owned(),
    })
}

fn parse_agent_typing_config(
    env: &HashMap<String, String>,
) -> Result<AgentTypingConfig, ConfigError> {
    let enabled =
        parse_bool(env, "QQ_MAID_AGENT_TYPING_ENABLED")?.unwrap_or(DEFAULT_AGENT_TYPING_ENABLED);
    let delay_ms = parse_ranged_u64(
        env,
        "QQ_MAID_AGENT_TYPING_DELAY_MS",
        DEFAULT_AGENT_TYPING_DELAY_MS,
        100,
        60_000,
    )?;
    Ok(AgentTypingConfig {
        enabled,
        delay: Duration::from_millis(delay_ms),
    })
}

fn parse_message_aggregation_config(
    env: &HashMap<String, String>,
) -> Result<MessageAggregationConfig, ConfigError> {
    let private_enabled = parse_bool(env, "MESSAGE_AGGREGATION_PRIVATE_ENABLED")?.unwrap_or(true);
    let group_enabled = parse_bool(env, "MESSAGE_AGGREGATION_GROUP_ENABLED")?.unwrap_or(false);
    if group_enabled {
        return Err(ConfigError::UnsupportedEnabled {
            name: "MESSAGE_AGGREGATION_GROUP_ENABLED",
        });
    }
    let quiet_ms = parse_ranged_u64(
        env,
        "MESSAGE_AGGREGATION_QUIET_MS",
        DEFAULT_MESSAGE_AGGREGATION_QUIET_MS,
        1,
        60_000,
    )?;
    let max_wait_ms = parse_ranged_u64(
        env,
        "MESSAGE_AGGREGATION_MAX_WAIT_MS",
        DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS,
        1,
        300_000,
    )?;
    if quiet_ms > max_wait_ms {
        return Err(ConfigError::InvalidAggregationWindow);
    }
    Ok(MessageAggregationConfig {
        private_enabled,
        group_enabled,
        quiet: Duration::from_millis(quiet_ms),
        max_wait: Duration::from_millis(max_wait_ms),
        max_messages: parse_ranged_usize(
            env,
            "MESSAGE_AGGREGATION_MAX_MESSAGES",
            DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES,
            1,
            100,
        )?,
        max_chars: parse_ranged_usize(
            env,
            "MESSAGE_AGGREGATION_MAX_CHARS",
            DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
            1,
            1_000_000,
        )?,
        max_active_keys: parse_ranged_usize(
            env,
            "MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS",
            DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS,
            1,
            100_000,
        )?,
    })
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

fn parse_u16(
    env: &HashMap<String, String>,
    name: &'static str,
) -> Result<Option<u16>, ConfigError> {
    let Some(value) = parse_u64(env, name)? else {
        return Ok(None);
    };
    u16::try_from(value)
        .map(Some)
        .map_err(|_| ConfigError::IntegerOutOfRange {
            name,
            value,
            min: 0,
            max: u16::MAX as u64,
        })
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
        assert!(config.bot_mention_ids.is_empty());
        assert!(!config.enable_group_messages);
        assert!(!config.verbose_log);
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
                bind_host: DEFAULT_WECHAT_SERVICE_BIND_HOST.to_owned(),
                bind_port: DEFAULT_WECHAT_SERVICE_BIND_PORT,
                callback_path: DEFAULT_WECHAT_SERVICE_CALLBACK_PATH.to_owned(),
                reply_timeout: Duration::from_millis(DEFAULT_WECHAT_SERVICE_REPLY_TIMEOUT_MS),
                api_base: DEFAULT_WECHAT_SERVICE_API_BASE.to_owned(),
            }
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
                bind_host: "0.0.0.0".to_owned(),
                bind_port: 19090,
                callback_path: "/wx/callback".to_owned(),
                reply_timeout: Duration::from_millis(3500),
                api_base: "https://wechat-api.example.test".to_owned(),
            }
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
