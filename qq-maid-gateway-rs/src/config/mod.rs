//! 网关配置模块。从环境变量加载可选 QQ 官方 Bot 绑定、Gateway API 地址和回调开关。

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
/// 是否在入站时调用 #229 群成员详情接口补全 actor/mention/引用 sender 的展示字段。
/// 默认开启；拉取失败降级为 source=Event，不阻断主回复。可经环境变量关闭。
pub const DEFAULT_MEMBER_DETAIL_ENRICH_ENABLED: bool = true;
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
pub const DEFAULT_ONEBOT11_BIND_HOST: &str = "127.0.0.1";
pub const DEFAULT_ONEBOT11_BIND_PORT: u16 = 8789;
pub const DEFAULT_ONEBOT11_WEBSOCKET_PATH: &str = "/onebot/v11/ws";
pub const DEFAULT_ONEBOT11_REQUEST_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_ONEBOT11_MAX_MESSAGE_BYTES: u64 = 1024 * 1024;
pub const MIN_ONEBOT11_MAX_MESSAGE_BYTES: u64 = 1024;
pub const MAX_ONEBOT11_MAX_MESSAGE_BYTES: u64 = 16 * 1024 * 1024;
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
    /// QQ 官方 Bot 是否启用。凭证成对存在时默认启用，保持旧配置行为。
    pub qq_official_enabled: bool,
    /// QQ 官方 Bot 凭证必须成对存在；`None` 表示渠道未绑定，不使用空串占位。
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub bot_mention_ids: Vec<String>,
    pub sandbox: bool,
    pub api_base: String,
    pub token_refresh_margin: Duration,
    pub enable_markdown: bool,
    pub enable_image: bool,
    pub enable_group_messages: bool,
    pub verbose_log: bool,
    /// 是否在入站时调用 #229 群成员详情接口补全展示字段（#319）。
    /// 失败降级 source=Event，不阻断主回复。生产默认 true，测试默认 false。
    pub member_detail_enrich_enabled: bool,
    pub group_message_mode: GroupMessageMode,
    /// 程序生成的用户可见文案所使用的机器人主称呼。
    pub bot_display_name: String,
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
    /// OneBot 11 反向 WebSocket 单账号 text-only 入口；复用 Core，不向平台流式发送。
    pub onebot11: OneBot11Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneBot11Config {
    pub enabled: bool,
    pub bind_host: String,
    pub bind_port: u16,
    pub websocket_path: String,
    pub access_token: Option<String>,
    pub request_timeout: Duration,
    pub max_message_bytes: usize,
}

impl Default for OneBot11Config {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_host: DEFAULT_ONEBOT11_BIND_HOST.to_owned(),
            bind_port: DEFAULT_ONEBOT11_BIND_PORT,
            websocket_path: DEFAULT_ONEBOT11_WEBSOCKET_PATH.to_owned(),
            access_token: None,
            request_timeout: Duration::from_millis(DEFAULT_ONEBOT11_REQUEST_TIMEOUT_MS),
            max_message_bytes: DEFAULT_ONEBOT11_MAX_MESSAGE_BYTES as usize,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WechatServiceConfig {
    pub enabled: bool,
    pub token: Option<String>,
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    /// 微信回调消息加解密模式。安全模式必须同时配置 AppID 与 EncodingAESKey。
    pub encryption_mode: WechatServiceEncryptionMode,
    pub encoding_aes_key: Option<String>,
    pub bind_host: String,
    pub bind_port: u16,
    pub callback_path: String,
    pub reply_timeout: Duration,
    pub api_base: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WechatServiceEncryptionMode {
    #[default]
    Plaintext,
    Aes,
}

impl WechatServiceEncryptionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plaintext => "plaintext",
            Self::Aes => "aes",
        }
    }
}

impl Default for WechatServiceConfig {
    fn default() -> Self {
        Self {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QqOfficialBindingState {
    Unbound,
    Disabled,
    Enabled,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
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
    #[error("invalid WECHAT_SERVICE_ENCRYPTION_MODE: {value}; expected plaintext or aes")]
    InvalidWechatServiceEncryptionMode { value: String },
    #[error("WECHAT_SERVICE_ENCODING_AES_KEY must be a valid 43-character EncodingAESKey")]
    InvalidWechatServiceEncodingAesKey,
    #[error("{name} is not supported yet")]
    UnsupportedEnabled { name: &'static str },
    #[error("missing required environment variable {name} when {enabled_by}=true")]
    MissingRequiredWhenEnabled {
        name: &'static str,
        enabled_by: &'static str,
    },
    #[error("missing required environment variable {name} when WECHAT_SERVICE_ENCRYPTION_MODE=aes")]
    MissingRequiredForWechatAesMode { name: &'static str },
    #[error("{name} must be an absolute HTTP path beginning with '/', got {value}")]
    InvalidHttpPath { name: &'static str, value: String },
    #[error(
        "MESSAGE_AGGREGATION_QUIET_MS must be less than or equal to MESSAGE_AGGREGATION_MAX_WAIT_MS"
    )]
    InvalidAggregationWindow,
    #[error(
        "QQ official credentials must be configured together; missing environment variable {missing}"
    )]
    IncompleteQqOfficialCredentials { missing: &'static str },
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

    /// 返回程序生成文案使用的机器人主称呼。
    ///
    /// 主称呼与群聊 active 关键词分别保存，避免旧显示名兼容值意外成为群聊触发词。
    /// 这里仍保留默认回退，避免测试或未来手工构造配置时产生空自称。
    pub fn bot_display_name(&self) -> &str {
        let value = self.bot_display_name.trim();
        if value.is_empty() {
            DEFAULT_GROUP_ACTIVE_KEYWORDS[0]
        } else {
            value
        }
    }

    pub fn from_map(env: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let qq_official_enabled = parse_bool(env, "QQ_BOT_ENABLED")?.unwrap_or(true);
        let app_id = optional_with_alias(env, "QQ_BOT_APP_ID", Some("QQ_APPID"));
        let app_secret = optional_with_alias(env, "QQ_BOT_APP_SECRET", Some("QQ_SECRET"));
        match (&app_id, &app_secret) {
            (Some(_), Some(_)) | (None, None) => {}
            (None, Some(_)) => {
                return Err(ConfigError::IncompleteQqOfficialCredentials {
                    missing: "QQ_BOT_APP_ID",
                });
            }
            (Some(_), None) => {
                return Err(ConfigError::IncompleteQqOfficialCredentials {
                    missing: "QQ_BOT_APP_SECRET",
                });
            }
        }
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
        let member_detail_enrich_enabled = parse_bool(env, "QQ_MAID_MEMBER_DETAIL_ENRICH_ENABLED")?
            .unwrap_or(DEFAULT_MEMBER_DETAIL_ENRICH_ENABLED);
        let group_message_mode = parse_group_message_mode(env)?;
        let group_active_keywords = parse_csv(
            env,
            "QQ_MAID_GROUP_ACTIVE_KEYWORDS",
            DEFAULT_GROUP_ACTIVE_KEYWORDS,
        );
        // 新关键词配置显式存在时（包括空值）不读取旧显示名；只有完全未设置时才兼容
        // QQ_MAID_STATUS_DISPLAY_NAME，且该兼容值只用于文案，不参与 active 匹配。
        let bot_display_name = if env.contains_key("QQ_MAID_GROUP_ACTIVE_KEYWORDS") {
            group_active_keywords
                .first()
                .cloned()
                .unwrap_or_else(|| DEFAULT_GROUP_ACTIVE_KEYWORDS[0].to_owned())
        } else {
            optional(env, "QQ_MAID_STATUS_DISPLAY_NAME")
                .unwrap_or_else(|| DEFAULT_GROUP_ACTIVE_KEYWORDS[0].to_owned())
        };
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
        let onebot11 = parse_onebot11_config(env)?;
        Ok(Self {
            qq_official_enabled,
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
            member_detail_enrich_enabled,
            group_message_mode,
            bot_display_name,
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
            onebot11,
        })
    }

    pub fn qq_official_binding_state(&self) -> QqOfficialBindingState {
        if self.app_id.is_none() {
            QqOfficialBindingState::Unbound
        } else if self.qq_official_enabled {
            QqOfficialBindingState::Enabled
        } else {
            QqOfficialBindingState::Disabled
        }
    }

    /// 只有已绑定且启用时才向初始化链路提供凭证，防止误建 Token/Gateway 任务。
    pub fn enabled_qq_official_credentials(&self) -> Option<(&str, &str)> {
        if self.qq_official_binding_state() != QqOfficialBindingState::Enabled {
            return None;
        }
        Some((self.app_id.as_deref()?, self.app_secret.as_deref()?))
    }
}

fn parse_onebot11_config(env: &HashMap<String, String>) -> Result<OneBot11Config, ConfigError> {
    let enabled = parse_bool(env, "ONEBOT11_ENABLED")?.unwrap_or(false);
    let access_token = optional(env, "ONEBOT11_ACCESS_TOKEN");
    if enabled && access_token.is_none() {
        return Err(ConfigError::MissingRequiredWhenEnabled {
            name: "ONEBOT11_ACCESS_TOKEN",
            enabled_by: "ONEBOT11_ENABLED",
        });
    }
    let websocket_path = optional(env, "ONEBOT11_WEBSOCKET_PATH")
        .unwrap_or_else(|| DEFAULT_ONEBOT11_WEBSOCKET_PATH.to_owned());
    let invalid_static_path = !websocket_path.starts_with('/')
        || websocket_path.contains(['?', '#', '{', '}', '*'])
        || websocket_path.contains(char::is_whitespace);
    if invalid_static_path {
        return Err(ConfigError::InvalidHttpPath {
            name: "ONEBOT11_WEBSOCKET_PATH",
            value: websocket_path,
        });
    }
    let bind_port = parse_ranged_u64(
        env,
        "ONEBOT11_BIND_PORT",
        u64::from(DEFAULT_ONEBOT11_BIND_PORT),
        1,
        u64::from(u16::MAX),
    )? as u16;
    let request_timeout_ms = parse_ranged_u64(
        env,
        "ONEBOT11_REQUEST_TIMEOUT_MS",
        DEFAULT_ONEBOT11_REQUEST_TIMEOUT_MS,
        100,
        120_000,
    )?;
    let max_message_bytes = parse_ranged_u64(
        env,
        "ONEBOT11_MAX_MESSAGE_BYTES",
        DEFAULT_ONEBOT11_MAX_MESSAGE_BYTES,
        MIN_ONEBOT11_MAX_MESSAGE_BYTES,
        MAX_ONEBOT11_MAX_MESSAGE_BYTES,
    )?;

    Ok(OneBot11Config {
        enabled,
        bind_host: optional(env, "ONEBOT11_BIND_HOST")
            .unwrap_or_else(|| DEFAULT_ONEBOT11_BIND_HOST.to_owned()),
        bind_port,
        websocket_path,
        access_token,
        request_timeout: Duration::from_millis(request_timeout_ms),
        max_message_bytes: max_message_bytes as usize,
    })
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
    let encryption_mode = match optional(env, "WECHAT_SERVICE_ENCRYPTION_MODE")
        .unwrap_or_else(|| "plaintext".to_owned())
        .to_ascii_lowercase()
        .as_str()
    {
        "plaintext" => WechatServiceEncryptionMode::Plaintext,
        "aes" => WechatServiceEncryptionMode::Aes,
        value => {
            return Err(ConfigError::InvalidWechatServiceEncryptionMode {
                value: value.to_owned(),
            });
        }
    };
    let app_id = optional(env, "WECHAT_SERVICE_APP_ID");
    let encoding_aes_key = optional(env, "WECHAT_SERVICE_ENCODING_AES_KEY");
    if let Some(key) = encoding_aes_key.as_deref()
        && !valid_wechat_encoding_aes_key(key)
    {
        return Err(ConfigError::InvalidWechatServiceEncodingAesKey);
    }
    if enabled && encryption_mode == WechatServiceEncryptionMode::Aes {
        if app_id.is_none() {
            return Err(ConfigError::MissingRequiredForWechatAesMode {
                name: "WECHAT_SERVICE_APP_ID",
            });
        }
        if encoding_aes_key.is_none() {
            return Err(ConfigError::MissingRequiredForWechatAesMode {
                name: "WECHAT_SERVICE_ENCODING_AES_KEY",
            });
        }
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
        app_id,
        app_secret: optional(env, "WECHAT_SERVICE_APP_SECRET"),
        encryption_mode,
        encoding_aes_key,
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

fn valid_wechat_encoding_aes_key(value: &str) -> bool {
    use base64::{
        Engine as _, alphabet,
        engine::{GeneralPurpose, GeneralPurposeConfig},
    };

    let decoder = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
    );

    value.len() == 43
        && decoder
            .decode(format!("{value}="))
            .is_ok_and(|decoded| decoded.len() == 32)
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
mod tests;
