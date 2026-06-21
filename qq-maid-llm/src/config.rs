//! 应用配置模块。从环境变量加载 LLM 供应商、模型、服务器端口等配置，
//! 提供 `AppConfig` 结构体及其构造方法。

use std::env;

use crate::{
    error::LlmError,
    provider::types::{ModelId, ModelProvider, ModelRoute},
    runtime::weather::{
        default_qweather_api_host, default_qweather_geo_host, qweather_geo_host_from_api_host,
    },
};

// ---- 默认常量 ----
pub const DEFAULT_PROVIDER: &str = "openai"; // 默认 LLM 供应商
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-5.5"; // 默认 OpenAI 模型
pub const DEFAULT_SEARCH_MODEL: &str = "gpt-5.5"; // 默认联网搜索模型
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com"; // DeepSeek 默认 API 地址
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-chat"; // 默认 DeepSeek 模型
pub const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 90; // LLM 请求超时（秒）
pub const DEFAULT_TTFT_WARN_SECONDS: u64 = 30; // 首 token 到达告警阈值（秒）
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 1200; // LLM 输出最大 token 数
pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1"; // 监听地址
pub const DEFAULT_SERVER_PORT: u16 = 8787; // 监听端口
pub const DEFAULT_APP_DB_FILE: &str = "data/storage/app.db"; // 项目通用 SQLite 文件
pub const DEFAULT_PROMPT_DIR: &str = "config/prompts"; // 提示词模板目录
pub const DEFAULT_MEMBER_ID_MAPPING_FILE: &str = "config/member_id_mapping.json"; // 成员 ID 映射文件
pub const DEFAULT_RSS_PUSH_URL: &str = "http://127.0.0.1:8788/internal/push"; // gateway 内部推送入口
pub const DEFAULT_RSS_POLL_INTERVAL_SECONDS: u64 = 300; // RSS 轮询间隔
pub const DEFAULT_RSS_HTTP_TIMEOUT_SECONDS: u64 = 15; // RSS HTTP 请求超时
pub const DEFAULT_RSS_MAX_BODY_BYTES: u64 = 2 * 1024 * 1024; // RSS 响应体大小上限
pub const DEFAULT_RSS_MAX_PUSH_PER_FEED: u64 = 3; // 单订阅单轮最大推送条数
pub const DEFAULT_RSS_SUMMARY_MAX_CHARS: u64 = 500; // RSS 摘要最大 Unicode 字符数
pub const DEFAULT_RSS_SEEN_RETENTION: u64 = 500; // 每订阅保留的去重指纹数
pub const DEFAULT_RSS_PUSH_MAX_FAILURES: u64 = 3; // 单条目推送失败上限
pub const DEFAULT_RSS_PUSH_MESSAGE_TYPE: &str = "markdown"; // RSS 主动推送消息类型

/// LLM 供应商选择模式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderMode {
    /// 使用 OpenAI 兼容 API
    OpenAi,
    /// 使用 DeepSeek API
    DeepSeek,
    /// 根据模型 ID 自动选择
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiApiMode {
    Auto,
    ChatOnly,
}

impl ProviderMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::Auto => "auto",
        }
    }
}

/// 完整应用配置，全部从环境变量读取。
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// LLM 供应商（openai / deepseek / auto）
    pub provider: ProviderMode,
    /// 主 LLM 模型名
    pub model: String,
    /// 主 LLM 模型候选链，顺序与 `LLM_MODEL` 配置一致。
    pub model_route: ModelRoute,
    /// 标题生成模型（可选）
    pub title_model: Option<String>,
    /// 内部待办解析、待办 pending 修订使用的可选模型；未配置时沿用 LLM_MODEL。
    pub todo_model: Option<String>,
    /// 内部记忆草稿和记忆 pending 修订使用的可选模型；未配置时沿用 LLM_MODEL。
    pub memory_model: Option<String>,
    /// 内部会话压缩使用的可选模型；未配置时沿用 LLM_MODEL。
    pub compact_model: Option<String>,
    /// 翻译命令和 RSS 翻译使用的可选模型；未配置时沿用 LLM_MODEL。
    pub translation_model: Option<String>,
    /// 联网搜索模型
    pub openai_search_model: String,
    /// OpenAI API 密钥
    pub openai_api_key: Option<String>,
    /// OpenAI API 基础地址
    pub openai_base_url: Option<String>,
    pub openai_api_mode: OpenAiApiMode,
    /// DeepSeek API 密钥
    pub deepseek_api_key: Option<String>,
    /// DeepSeek API 基础地址
    pub deepseek_base_url: String,
    /// DeepSeek 模型名
    pub deepseek_model: String,
    /// 是否启用流式输出
    pub stream: bool,
    /// 发送模式（final / streaming 等）
    pub send_mode: String,
    /// LLM 请求超时秒数
    pub request_timeout_seconds: u64,
    /// 首 token 到达告警阈值（秒）
    pub ttft_warn_seconds: u64,
    /// LLM 输出最大 token 数
    pub max_output_tokens: u64,
    /// HTTP 监听地址
    pub server_host: String,
    /// HTTP 监听端口
    pub server_port: u16,
    /// 项目通用 SQLite 文件路径；RSS、Todo、Session 和 Memory 共用该数据库。
    pub app_db_file: String,
    /// 是否启用 RSS 后台轮询
    pub rss_enabled: bool,
    /// RSS 轮询间隔（秒）
    pub rss_poll_interval_seconds: u64,
    /// RSS HTTP 请求超时（秒）
    pub rss_http_timeout_seconds: u64,
    /// RSS 响应体大小上限（字节）
    pub rss_max_body_bytes: u64,
    /// 单订阅单轮最大推送条数
    pub rss_max_push_per_feed: u64,
    /// RSS 摘要最大字符数
    pub rss_summary_max_chars: u64,
    /// 每订阅保留的去重记录数
    pub rss_seen_retention: u64,
    /// 单条目推送失败次数上限
    pub rss_push_max_failures: u64,
    /// gateway 内部主动推送入口
    pub rss_push_url: String,
    /// gateway 内部主动推送共享 token；空值表示不发送 token
    pub rss_push_token: Option<String>,
    /// RSS 主动推送消息类型：markdown / text
    pub rss_push_message_type: String,
    /// 是否允许 RSS 访问内网地址；默认关闭，仅测试或受控内网部署可开启。
    pub rss_allow_private_urls: bool,
    /// 提示词模板目录
    pub prompt_dir: String,
    /// 是否使用默认提示词目录；默认目录缺私有 prompt 时允许回退到公开内置提示词。
    pub prompt_dir_uses_builtin_defaults: bool,
    /// 可选世界观提示词文件；未配置时按通用助手运行。
    pub world_file: Option<String>,
    /// 群成员 ID 映射文件路径
    pub member_id_mapping_file: String,
    /// 和风天气 API 密钥
    pub qweather_api_key: String,
    /// 和风天气 API 主机地址
    pub qweather_api_host: String,
    /// 和风天气地理编码 API 主机地址
    pub qweather_geo_host: String,
    /// 是否启用本地 Web 控制台和 Markdown 预览接口。
    pub web_console_enabled: bool,
    /// 控制台跨域 allowlist；为空时仅同源访问。
    pub web_console_allowed_origins: Vec<String>,
}

impl AppConfig {
    /// 从环境变量构造配置对象。关键词必须配置，其余有默认值。
    pub fn from_env() -> Result<Self, LlmError> {
        let provider = parse_provider(&env_string("LLM_PROVIDER", DEFAULT_PROVIDER))?;
        let model = env_model_string(
            "LLM_MODEL",
            &env_string("OPENAI_MODEL", DEFAULT_OPENAI_MODEL),
        )?;
        let model_route = ModelRoute::parse_config(&model, "LLM_MODEL")?;
        let deepseek_model = env_string("DEEPSEEK_MODEL", DEFAULT_DEEPSEEK_MODEL);
        let openai_search_model =
            env_openai_model_or("OPENAI_SEARCH_MODEL", &model, DEFAULT_SEARCH_MODEL)?;
        let title_model = env_optional_model("TITLE_MODEL")?;
        let todo_model = env_optional_model("TODO_MODEL")?;
        let memory_model = env_optional_model("MEMORY_MODEL")?;
        let compact_model = env_optional_model("COMPACT_MODEL")?;
        let translation_model = translation_model_from_env()?;

        let qweather_api_key = env_required("QWEATHER_API_KEY")?;
        let configured_qweather_api_host = env_optional("QWEATHER_API_HOST");
        let qweather_geo_host = env_optional("QWEATHER_GEO_HOST").unwrap_or_else(|| {
            configured_qweather_api_host
                .as_deref()
                .map(qweather_geo_host_from_api_host)
                .unwrap_or_else(default_qweather_geo_host)
        });
        let qweather_api_host =
            configured_qweather_api_host.unwrap_or_else(default_qweather_api_host);

        let configured_prompt_dir = env_optional("PROMPT_DIR");
        let web_console_allowed_origins = env_list("WEB_CONSOLE_ALLOWED_ORIGINS");

        Ok(Self {
            provider,
            model,
            model_route,
            title_model,
            todo_model,
            memory_model,
            compact_model,
            translation_model,
            openai_search_model,
            openai_api_key: env_optional("OPENAI_API_KEY"),
            openai_base_url: openai_base_url_from_env(),
            openai_api_mode: parse_openai_api_mode(&env_string("OPENAI_API_MODE", "auto"))?,
            deepseek_api_key: env_optional("DEEPSEEK_API_KEY"),
            deepseek_base_url: env_string("DEEPSEEK_BASE_URL", DEFAULT_DEEPSEEK_BASE_URL),
            deepseek_model,
            stream: env_bool("LLM_STREAM", true)?,
            send_mode: env_string("LLM_SEND_MODE", "final"),
            request_timeout_seconds: env_u64(
                "LLM_REQUEST_TIMEOUT_SECONDS",
                DEFAULT_REQUEST_TIMEOUT_SECONDS,
            )?,
            ttft_warn_seconds: env_u64("LLM_TTFT_WARN_SECONDS", DEFAULT_TTFT_WARN_SECONDS)?,
            max_output_tokens: env_u64("LLM_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS)?,
            server_host: env_string("LLM_SERVER_HOST", DEFAULT_SERVER_HOST),
            server_port: env_u16("LLM_SERVER_PORT", DEFAULT_SERVER_PORT)?,
            app_db_file: env_optional("APP_DB_FILE").unwrap_or_else(default_app_db_file),
            rss_enabled: env_bool("RSS_ENABLED", true)?,
            rss_poll_interval_seconds: env_u64(
                "RSS_POLL_INTERVAL_SECONDS",
                DEFAULT_RSS_POLL_INTERVAL_SECONDS,
            )?,
            rss_http_timeout_seconds: env_u64(
                "RSS_HTTP_TIMEOUT_SECONDS",
                DEFAULT_RSS_HTTP_TIMEOUT_SECONDS,
            )?,
            rss_max_body_bytes: env_u64("RSS_MAX_BODY_BYTES", DEFAULT_RSS_MAX_BODY_BYTES)?,
            rss_max_push_per_feed: env_u64("RSS_MAX_PUSH_PER_FEED", DEFAULT_RSS_MAX_PUSH_PER_FEED)?,
            rss_summary_max_chars: env_u64("RSS_SUMMARY_MAX_CHARS", DEFAULT_RSS_SUMMARY_MAX_CHARS)?,
            rss_seen_retention: env_u64("RSS_SEEN_RETENTION", DEFAULT_RSS_SEEN_RETENTION)?,
            rss_push_max_failures: env_u64("RSS_PUSH_MAX_FAILURES", DEFAULT_RSS_PUSH_MAX_FAILURES)?,
            rss_push_url: env_string("RSS_PUSH_URL", DEFAULT_RSS_PUSH_URL),
            rss_push_token: env_optional("RSS_PUSH_TOKEN"),
            rss_push_message_type: env_string(
                "RSS_PUSH_MESSAGE_TYPE",
                DEFAULT_RSS_PUSH_MESSAGE_TYPE,
            ),
            rss_allow_private_urls: env_bool("RSS_ALLOW_PRIVATE_URLS", false)?,
            prompt_dir: configured_prompt_dir
                .clone()
                .unwrap_or_else(default_prompt_dir),
            prompt_dir_uses_builtin_defaults: configured_prompt_dir.is_none(),
            world_file: env_optional("WORLD_FILE"),
            member_id_mapping_file: env_optional("MEMBER_ID_MAPPING_FILE")
                .unwrap_or_else(|| DEFAULT_MEMBER_ID_MAPPING_FILE.to_owned()),
            qweather_api_key,
            qweather_api_host,
            qweather_geo_host,
            web_console_enabled: env_bool("WEB_CONSOLE_ENABLED", false)?,
            web_console_allowed_origins,
        })
    }

    /// 返回所有可能作为 `ChatRequest.model` 传入 provider 层的模型候选链。
    ///
    /// 这个列表用于启动阶段校验和 provider 初始化；新增内部专项模型配置时，
    /// 需要同步加入这里，避免首次执行业务任务时才发现 provider 不可用。
    pub fn configured_model_routes(&self) -> Result<Vec<(&'static str, ModelRoute)>, LlmError> {
        let mut routes = vec![("LLM_MODEL", self.model_route.clone())];
        for (name, value) in [
            ("TITLE_MODEL", self.title_model.as_deref()),
            ("TODO_MODEL", self.todo_model.as_deref()),
            ("MEMORY_MODEL", self.memory_model.as_deref()),
            ("COMPACT_MODEL", self.compact_model.as_deref()),
            ("TRANSLATION_MODEL", self.translation_model.as_deref()),
        ] {
            if let Some(value) = value {
                routes.push((name, ModelRoute::parse_config(value, name)?));
            }
        }
        Ok(routes)
    }
}

/// 默认项目通用 SQLite 文件路径。
fn default_app_db_file() -> String {
    DEFAULT_APP_DB_FILE.to_owned()
}

/// 默认提示词模板目录。
fn default_prompt_dir() -> String {
    DEFAULT_PROMPT_DIR.to_owned()
}

/// 将字符串解析为 ProviderMode，仅接受 openai / deepseek / auto。
fn parse_provider(value: &str) -> Result<ProviderMode, LlmError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok(ProviderMode::OpenAi),
        "deepseek" => Ok(ProviderMode::DeepSeek),
        "auto" => Ok(ProviderMode::Auto),
        other => Err(LlmError::config(format!(
            "unsupported LLM_PROVIDER `{other}`; supported: openai, deepseek, auto"
        ))),
    }
}

pub(crate) fn parse_openai_api_mode(value: &str) -> Result<OpenAiApiMode, LlmError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(OpenAiApiMode::Auto),
        "chat_only" | "chat-only" => Ok(OpenAiApiMode::ChatOnly),
        other => Err(LlmError::config(format!(
            "unsupported OPENAI_API_MODE `{other}`; supported: auto, chat_only"
        ))),
    }
}

/// 读取可选环境变量，返回 trimmed 后的值；未设置或为空则返回 None。
fn env_optional(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 翻译命令和 RSS 翻译共用的模型配置；空值保持 None，由 provider 回退主模型。
fn translation_model_from_env() -> Result<Option<String>, LlmError> {
    env_optional_model("TRANSLATION_MODEL")
}

/// 读取必选环境变量，缺失则返回配置错误。
fn env_required(name: &str) -> Result<String, LlmError> {
    env_optional(name).ok_or_else(|| LlmError::config(format!("{name} must be configured")))
}

/// 读取环境变量，未设置时返回默认值。
fn env_string(name: &str, default: &str) -> String {
    env_optional(name).unwrap_or_else(|| default.to_owned())
}

fn env_list(name: &str) -> Vec<String> {
    env_optional(name)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// 读取模型配置；显式配置为空时返回错误，避免把 `LLM_MODEL=` 静默当作默认模型。
fn env_model_string(name: &str, default: &str) -> Result<String, LlmError> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => {
            Err(LlmError::config(format!("{name} must not be empty")))
        }
        Ok(value) => Ok(value.trim().to_owned()),
        Err(_) => Ok(default.to_owned()),
    }
}

/// 读取并校验可选模型候选链；空值表示未配置。
fn env_optional_model(name: &str) -> Result<Option<String>, LlmError> {
    let Some(value) = env_optional(name) else {
        return Ok(None);
    };
    ModelRoute::parse_config(&value, name)?;
    Ok(Some(value))
}

/// 尝试读取 OpenAI 查询模型环境变量：优先使用指定变量，回退 LLM_MODEL，最后使用默认值。
fn env_openai_model_or(name: &str, llm_model: &str, default: &str) -> Result<String, LlmError> {
    if let Some(value) = env_optional(name) {
        return openai_model_name(&value, name);
    }
    openai_model_name_from_route(llm_model, "LLM_MODEL").or_else(|_| Ok(default.to_owned()))
}

/// 校验模型名：允许纯模型名或 `openai:` 前缀，拒绝 `deepseek:` 前缀。
fn openai_model_name(value: &str, name: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, name)?;
    match model.provider {
        Some(ModelProvider::OpenAi) | None => Ok(model.name),
        Some(ModelProvider::DeepSeek) => Err(LlmError::config(format!(
            "{name} cannot use deepseek: prefix for OpenAI query model"
        ))),
    }
}

/// 从主模型候选链中取第一个 OpenAI 兼容候选，用作查询模型的兼容默认值。
///
/// `/查` 当前仍是 OpenAI Responses web_search 直连，不能直接复用聊天候选链；
/// 因此当 `LLM_MODEL` 同时配置 DeepSeek 候选时，这里只取 OpenAI 可用项。
fn openai_model_name_from_route(value: &str, name: &str) -> Result<String, LlmError> {
    let route = ModelRoute::parse_config(value, name)?;
    route
        .candidates()
        .iter()
        .find_map(|model| match model.provider {
            Some(ModelProvider::OpenAi) | None => Some(model.name.clone()),
            Some(ModelProvider::DeepSeek) => None,
        })
        .ok_or_else(|| {
            LlmError::config(format!(
                "{name} must include an OpenAI-compatible model for OpenAI query model fallback"
            ))
        })
}

/// 从环境变量读取 OpenAI 基础地址：优先 `OPENAI_BASE_URLS`（逗号分隔），回退 `OPENAI_BASE_URL`。
fn openai_base_url_from_env() -> Option<String> {
    first_openai_base_url(
        env_optional("OPENAI_BASE_URLS").as_deref(),
        env_optional("OPENAI_BASE_URL").as_deref(),
    )
}

/// 从多个 URL 中取第一个非空值：base_urls（逗号分隔）优先于 base_url。
fn first_openai_base_url(base_urls: Option<&str>, base_url: Option<&str>) -> Option<String> {
    if let Some(url) = base_urls
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .find(|value| !value.is_empty())
    {
        return Some(url.to_owned());
    }

    base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 读取布尔型环境变量。接受的 true 值：1/true/on/yes/enabled。
fn env_bool(name: &str, default: bool) -> Result<bool, LlmError> {
    let Some(value) = env_optional(name) else {
        return Ok(default);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" | "enabled" => Ok(true),
        "0" | "false" | "off" | "no" | "disabled" | "none" => Ok(false),
        _ => Err(LlmError::config(format!(
            "unsupported boolean value for {name}: {value}"
        ))),
    }
}

/// 读取 u64 型环境变量，必须为正整数。
fn env_u64(name: &str, default: u64) -> Result<u64, LlmError> {
    let Some(value) = env_optional(name) else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u64>()
        .map_err(|_| LlmError::config(format!("unsupported integer value for {name}: {value}")))?;
    if parsed == 0 {
        return Err(LlmError::config(format!(
            "{name} must be a positive integer"
        )));
    }
    Ok(parsed)
}

/// 读取 u16 型环境变量，必须为正整数（用于端口号）。
fn env_u16(name: &str, default: u16) -> Result<u16, LlmError> {
    let Some(value) = env_optional(name) else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u16>()
        .map_err(|_| LlmError::config(format!("unsupported port value for {name}: {value}")))?;
    if parsed == 0 {
        return Err(LlmError::config(format!(
            "{name} must be a positive integer"
        )));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_accepts_known_values() {
        assert_eq!(parse_provider("openai").unwrap(), ProviderMode::OpenAi);
        assert_eq!(parse_provider("DEEPSEEK").unwrap(), ProviderMode::DeepSeek);
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
    fn openai_model_name_rejects_deepseek_prefix() {
        let err = openai_model_name("deepseek:deepseek-chat", "OPENAI_SEARCH_MODEL").unwrap_err();
        assert_eq!(err.code, "config");
        assert!(err.message.contains("deepseek:"));
    }

    #[test]
    fn openai_model_name_from_route_uses_first_openai_candidate() {
        assert_eq!(
            openai_model_name_from_route(
                "deepseek:deepseek-chat, openai:gpt-5.4-mini",
                "LLM_MODEL"
            )
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
    fn env_example_documents_rss_summary_limit_default() {
        let env_example = include_str!("../../runtime/.env.example");

        assert!(env_example.contains("RSS_SUMMARY_MAX_CHARS=500"));
    }

    #[test]
    fn env_optional_trims_values_and_treats_empty_as_unset() {
        unsafe {
            env::set_var("QQ_MAID_TEST_OPTIONAL_VALUE", "  /tmp/world.md  ");
            env::set_var("QQ_MAID_TEST_EMPTY_VALUE", "  \n ");
        }

        assert_eq!(
            env_optional("QQ_MAID_TEST_OPTIONAL_VALUE").as_deref(),
            Some("/tmp/world.md")
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
    fn env_example_documents_optional_world_file() {
        let env_example = include_str!("../../runtime/.env.example");

        assert!(env_example.contains("WORLD_FILE="));
    }

    #[test]
    fn env_example_documents_translation_model() {
        let env_example = include_str!("../../runtime/.env.example");

        assert!(env_example.contains("TRANSLATION_MODEL="));
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
}
