//! 应用配置模块。从环境变量加载 LLM 供应商、模型、服务器端口等配置，
//! 提供 `AppConfig` 结构体及其构造方法。

use std::{env, fmt, path::Path};

use qq_maid_llm::config::{HttpAuthConfig, OpenAiCompatibleProviderConfig};
use qq_maid_llm::context_budget::ContextBudgetConfig;
use qq_maid_llm::provider::types::{ModelId, ModelProvider, ModelRoute};

use crate::{
    error::LlmError,
    runtime::weather::{
        default_qweather_api_host, default_qweather_geo_host, qweather_geo_host_from_api_host,
    },
    storage::database::{DEFAULT_SQLITE_POOL_SIZE, MAX_SQLITE_POOL_SIZE, MIN_SQLITE_POOL_SIZE},
};

pub mod agent;
pub use agent::{AgentRuntimeConfig, ChatScene, LegacyAgentConfig, ResolvedAgentPolicy};

// ---- 默认常量 ----
pub const DEFAULT_PROVIDER: &str = "openai"; // 默认 LLM 供应商
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-5.5"; // 默认 OpenAI 模型
pub const DEFAULT_SEARCH_MODEL: &str = "gpt-5.5"; // 默认联网搜索模型
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com"; // DeepSeek 默认 API 地址
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-chat"; // 默认 DeepSeek 模型
pub const DEFAULT_BIGMODEL_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4"; // BigModel 通用 API 地址
pub const DEFAULT_BIGMODEL_MODEL: &str = "glm-5.2"; // 默认 BigModel 模型
pub const DEFAULT_GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai"; // Gemini OpenAI-compatible API 地址
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-2.5-flash"; // 默认 Gemini 模型
pub const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 90; // LLM 请求超时（秒）
pub const DEFAULT_TTFT_WARN_SECONDS: u64 = 30; // 首 token 到达告警阈值（秒）
pub const DEFAULT_MEDIA_MAX_BYTES: u64 = 10 * 1024 * 1024; // 单张图片最大处理字节数
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 1200; // LLM 输出最大 token 数
pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1"; // 监听地址
pub const DEFAULT_SERVER_PORT: u16 = 8787; // 监听端口
pub const DEFAULT_APP_DB_FILE: &str = "data/storage/app.db"; // 项目通用 SQLite 文件
pub const DEFAULT_PROMPT_DIR: &str = "config/prompts"; // 提示词模板目录
pub const DEFAULT_KNOWLEDGE_DIR: &str = "config/knowledge"; // Markdown 知识目录
const REMOVED_MEMBER_ID_MAPPING_FILE: &str = "config/member_id_mapping.json";
pub const DEFAULT_RSS_POLL_INTERVAL_SECONDS: u64 = 300; // RSS 轮询间隔
pub const DEFAULT_RSS_HTTP_TIMEOUT_SECONDS: u64 = 15; // RSS HTTP 请求超时
pub const DEFAULT_RSS_MAX_BODY_BYTES: u64 = 2 * 1024 * 1024; // RSS 响应体大小上限
pub const DEFAULT_RSS_MAX_PUSH_PER_FEED: u64 = 3; // 单订阅单轮最大推送条数
pub const DEFAULT_RSS_SUMMARY_MAX_CHARS: u64 = 500; // RSS 摘要最大 Unicode 字符数
pub const DEFAULT_RSS_SEEN_RETENTION: u64 = 500; // 每订阅保留的去重指纹数
pub const DEFAULT_RSS_PUSH_MAX_FAILURES: u64 = 3; // 单条目推送失败上限
pub const DEFAULT_RSS_PUSH_MESSAGE_TYPE: &str = "markdown"; // RSS 主动推送消息类型
pub const DEFAULT_TODO_DAILY_REMINDER_TIME: &str = "09:00"; // Todo 每日提醒默认时间
pub const DEFAULT_MAX_CONCURRENT_RESPONSES: u64 = 8; // 全局 LLM / Web Search 最大并发
pub const DEFAULT_TOOL_CALLING_MAX_ROUNDS: u64 = 5; // 原生 Tool Calling 最大工具调用轮数（每轮可含多个工具）
pub const DEFAULT_AGENT_CONTEXT_CHAR_LIMIT: u64 = 48_000; // 本地上下文窗口字符预算
pub const DEFAULT_AGENT_CONTEXT_OUTPUT_RESERVE_CHARS: u64 = 6_000; // 为模型输出预留的字符预算
pub const DEFAULT_AGENT_CONTEXT_PROTECTED_RECENT_TURNS: u64 = 4; // 最近完整对话轮次保护数量
pub const DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT: u64 =
    qq_maid_llm::tool::DEFAULT_TOOL_OUTPUT_MAX_CHARS as u64; // 单项工具结果最大字符数
pub const MIN_AGENT_TOOL_RESULT_CHAR_LIMIT: u64 =
    qq_maid_llm::tool::MIN_TOOL_OUTPUT_MAX_CHARS as u64; // 至少能表达 {"truncated":true}
pub const DEFAULT_STATUS_DISPLAY_NAME: &str = "小女仆"; // 私聊状态提示使用的前台称呼
pub const MAX_STATUS_DISPLAY_NAME_CHARS: usize = 24; // 避免配置过长导致状态提示刷屏
pub const MIN_MEDIA_MAX_BYTES: u64 = 64 * 1024;
pub const MAX_MEDIA_MAX_BYTES: u64 = 100 * 1024 * 1024;

/// LLM 供应商选择模式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderMode {
    /// 使用 OpenAI 兼容 API
    OpenAi,
    /// 使用 DeepSeek API
    DeepSeek,
    /// 使用智谱 BigModel API
    BigModel,
    /// 使用 Google Gemini API
    Gemini,
    /// 根据模型 ID 自动选择
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiApiMode {
    Auto,
    ChatOnly,
}

/// Todo 每日提醒使用的本地时刻，固定按 Asia/Shanghai 解释。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DailyReminderTime {
    pub hour: u8,
    pub minute: u8,
}

impl ProviderMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::BigModel => "bigmodel",
            Self::Gemini => "gemini",
            Self::Auto => "auto",
        }
    }
}

impl DailyReminderTime {
    /// 严格解析 `HH:MM` 24 小时制，避免 `9:00` 之类的宽松格式混入配置。
    pub fn parse_config(value: &str, name: &str) -> Result<Self, LlmError> {
        let value = value.trim();
        let [hour_a, hour_b, colon, minute_a, minute_b] = value.as_bytes() else {
            return Err(LlmError::config(format!(
                "{name} must use HH:MM format, got `{value}`"
            )));
        };
        if *colon != b':' {
            return Err(LlmError::config(format!(
                "{name} must use HH:MM format, got `{value}`"
            )));
        }

        let Some(hour) = parse_two_ascii_digits(*hour_a, *hour_b) else {
            return Err(LlmError::config(format!(
                "{name} must use HH:MM format, got `{value}`"
            )));
        };
        let Some(minute) = parse_two_ascii_digits(*minute_a, *minute_b) else {
            return Err(LlmError::config(format!(
                "{name} must use HH:MM format, got `{value}`"
            )));
        };
        if hour > 23 || minute > 59 {
            return Err(LlmError::config(format!(
                "{name} must use HH:MM format, got `{value}`"
            )));
        }
        Ok(Self { hour, minute })
    }
}

impl fmt::Display for DailyReminderTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

/// 完整应用配置，全部从环境变量读取。
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// LLM 供应商（openai / deepseek / bigmodel / auto）
    pub provider: ProviderMode,
    /// 主 LLM 模型名
    pub model: String,
    /// 主 LLM 模型候选链，顺序与 `LLM_MODEL` 配置一致。
    pub model_route: ModelRoute,
    /// 统一 Agent 场景运行策略，启动阶段完成解析和校验。
    pub agent_config: AgentRuntimeConfig,
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
    /// 智谱 BigModel API 密钥
    pub bigmodel_api_key: Option<String>,
    /// 智谱 BigModel API 基础地址
    pub bigmodel_base_url: String,
    /// 智谱 BigModel 模型名
    pub bigmodel_model: String,
    /// Google Gemini API 密钥
    pub gemini_api_key: Option<String>,
    /// Google Gemini OpenAI-compatible API 基础地址
    pub gemini_base_url: String,
    /// Google Gemini 模型名
    pub gemini_model: String,
    /// 是否启用流式输出
    pub stream: bool,
    /// LLM 请求超时秒数
    pub request_timeout_seconds: u64,
    /// 首 token 到达告警阈值（秒）
    pub ttft_warn_seconds: u64,
    /// 单张图片允许转成本地 data URL 的最大字节数。
    pub media_max_bytes: u64,
    /// LLM 输出最大 token 数
    pub max_output_tokens: u64,
    /// 全局 LLM 与 `/查` 共享的最大并发数；0 表示不限制。
    pub max_concurrent_responses: u64,
    /// 是否启用普通聊天的模型原生 Tool Calling 总开关。
    pub tool_calling_enabled: bool,
    /// 是否允许群聊普通聊天进入 Tool Calling；默认关闭，避免工具调用阻塞群聊。
    pub tool_calling_group_enabled: bool,
    /// 单次 Tool Loop 最多允许的工具调用轮数。
    pub tool_calling_max_rounds: u64,
    /// 聊天输入上下文预算；由 Core 装配层读取并传给 LLM 请求。
    pub context_budget: ContextBudgetConfig,
    /// 单项 Tool 输出最大字符数；不属于上下文预算，直接注入 ToolRegistry。
    pub tool_result_max_chars: usize,
    /// 私聊用户可见状态提示中的前台称呼。
    pub status_display_name: String,
    /// HTTP 监听地址
    pub server_host: String,
    /// HTTP 监听端口
    pub server_port: u16,
    /// 项目通用 SQLite 文件路径；RSS、Todo、Session 和 Memory 共用该数据库。
    pub app_db_file: String,
    /// 本地 SQLite 连接池大小，独立于 LLM / Web Search 并发限制。
    pub sqlite_pool_size: usize,
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
    /// RSS 主动推送消息类型：markdown / text
    pub rss_push_message_type: String,
    /// 是否启用 Todo 每日提醒调度。
    pub todo_daily_reminder_enabled: bool,
    /// Todo 每日提醒本地时间，固定按 Asia/Shanghai 解释。
    pub todo_daily_reminder_time: DailyReminderTime,
    /// 是否允许 RSS 访问内网地址；默认关闭，仅测试或受控内网部署可开启。
    pub rss_allow_private_urls: bool,
    /// 提示词模板目录
    pub prompt_dir: String,
    /// 是否使用默认提示词目录；默认目录缺私有 prompt 时允许回退到公开内置提示词。
    pub prompt_dir_uses_builtin_defaults: bool,
    /// Markdown 知识目录；普通聊天会从已同步索引中按需检索相关片段。
    pub knowledge_dir: String,
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
        reject_removed_env_vars()?;
        warn_removed_member_id_mapping_file();
        let provider = parse_provider(&env_string("LLM_PROVIDER", DEFAULT_PROVIDER))?;
        let model = env_model_string(
            "LLM_MODEL",
            &env_string("OPENAI_MODEL", DEFAULT_OPENAI_MODEL),
        )?;
        let model_route = ModelRoute::parse_config(&model, "LLM_MODEL")?;
        let deepseek_model = env_string("DEEPSEEK_MODEL", DEFAULT_DEEPSEEK_MODEL);
        let bigmodel_model = env_string("BIGMODEL_MODEL", DEFAULT_BIGMODEL_MODEL);
        let gemini_model = env_string("GEMINI_MODEL", DEFAULT_GEMINI_MODEL);
        let openai_search_model =
            env_openai_model_or("OPENAI_SEARCH_MODEL", &model, DEFAULT_SEARCH_MODEL)?;
        let group_llm_model = env_optional_model("GROUP_LLM_MODEL")?;
        let private_llm_model = env_optional_model("PRIVATE_LLM_MODEL")?;
        let group_openai_search_model = env_optional_openai_model("GROUP_OPENAI_SEARCH_MODEL")?;
        let private_openai_search_model = env_optional_openai_model("PRIVATE_OPENAI_SEARCH_MODEL")?;
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
        let context_budget = context_budget_from_env()?;
        let tool_result_max_chars = env_u64_bounded_range(
            "AGENT_TOOL_RESULT_CHAR_LIMIT",
            DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT,
            MIN_AGENT_TOOL_RESULT_CHAR_LIMIT,
            200_000,
        )? as usize;
        let tool_calling_enabled = env_bool("TOOL_CALLING_ENABLED", true)?;
        let tool_calling_group_enabled = env_bool("TOOL_CALLING_GROUP_ENABLED", false)?;
        let tool_calling_max_rounds = env_u64_bounded(
            "TOOL_CALLING_MAX_ROUNDS",
            DEFAULT_TOOL_CALLING_MAX_ROUNDS,
            8,
        )?;
        let media_max_bytes = env_u64_bounded_range(
            "QQ_MAID_MEDIA_MAX_BYTES",
            DEFAULT_MEDIA_MAX_BYTES,
            MIN_MEDIA_MAX_BYTES,
            MAX_MEDIA_MAX_BYTES,
        )?;
        let max_output_tokens = env_u64("LLM_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS)?;
        let agent_config = AgentRuntimeConfig::load(LegacyAgentConfig {
            main_model: model.clone(),
            max_output_tokens,
            openai_search_model: openai_search_model.clone(),
            tool_calling_enabled,
            group_tool_calling_enabled: tool_calling_group_enabled,
            tool_calling_max_rounds,
            group_llm_model: group_llm_model.clone(),
            private_llm_model: private_llm_model.clone(),
            group_openai_search_model: group_openai_search_model.clone(),
            private_openai_search_model: private_openai_search_model.clone(),
        })?;

        Ok(Self {
            provider,
            model,
            model_route,
            agent_config,
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
            bigmodel_api_key: env_optional("BIGMODEL_API_KEY"),
            bigmodel_base_url: env_string("BIGMODEL_BASE_URL", DEFAULT_BIGMODEL_BASE_URL),
            bigmodel_model,
            gemini_api_key: env_optional("GEMINI_API_KEY"),
            gemini_base_url: env_string("GEMINI_BASE_URL", DEFAULT_GEMINI_BASE_URL),
            gemini_model,
            stream: env_bool("LLM_STREAM", true)?,
            request_timeout_seconds: env_u64(
                "LLM_REQUEST_TIMEOUT_SECONDS",
                DEFAULT_REQUEST_TIMEOUT_SECONDS,
            )?,
            ttft_warn_seconds: env_u64("LLM_TTFT_WARN_SECONDS", DEFAULT_TTFT_WARN_SECONDS)?,
            media_max_bytes,
            max_output_tokens,
            max_concurrent_responses: env_u64_bounded_zero_allowed(
                "MAX_CONCURRENT_RESPONSES",
                DEFAULT_MAX_CONCURRENT_RESPONSES,
                256,
            )?,
            tool_calling_enabled,
            tool_calling_group_enabled,
            tool_calling_max_rounds,
            context_budget,
            tool_result_max_chars,
            status_display_name: env_status_display_name()?,
            server_host: env_string("LLM_SERVER_HOST", DEFAULT_SERVER_HOST),
            server_port: env_u16("LLM_SERVER_PORT", DEFAULT_SERVER_PORT)?,
            app_db_file: env_optional("APP_DB_FILE").unwrap_or_else(default_app_db_file),
            sqlite_pool_size: sqlite_pool_size_from_env()?,
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
            rss_push_message_type: env_string(
                "RSS_PUSH_MESSAGE_TYPE",
                DEFAULT_RSS_PUSH_MESSAGE_TYPE,
            ),
            todo_daily_reminder_enabled: env_bool("TODO_DAILY_REMINDER_ENABLED", false)?,
            todo_daily_reminder_time: env_daily_reminder_time(
                "TODO_DAILY_REMINDER_TIME",
                DEFAULT_TODO_DAILY_REMINDER_TIME,
            )?,
            rss_allow_private_urls: env_bool("RSS_ALLOW_PRIVATE_URLS", false)?,
            prompt_dir: configured_prompt_dir
                .clone()
                .unwrap_or_else(default_prompt_dir),
            prompt_dir_uses_builtin_defaults: configured_prompt_dir.is_none(),
            knowledge_dir: env_optional("KNOWLEDGE_DIR").unwrap_or_else(default_knowledge_dir),
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
    pub fn configured_model_routes(&self) -> Result<Vec<(String, ModelRoute)>, LlmError> {
        let mut routes = vec![("LLM_MODEL".to_owned(), self.model_route.clone())];
        for (name, value) in [
            ("TITLE_MODEL", self.title_model.as_deref()),
            ("TODO_MODEL", self.todo_model.as_deref()),
            ("MEMORY_MODEL", self.memory_model.as_deref()),
            ("COMPACT_MODEL", self.compact_model.as_deref()),
            ("TRANSLATION_MODEL", self.translation_model.as_deref()),
        ] {
            if let Some(value) = value {
                routes.push((name.to_owned(), ModelRoute::parse_config(value, name)?));
            }
        }
        for (name, route) in self.agent_config.configured_model_routes() {
            routes.push((name, route));
        }
        Ok(routes)
    }

    /// 提取 LLM crate 所需的 Provider 基础配置。
    ///
    /// Todo、标题、记忆、压缩和翻译模型仍由 Core 解析和管理，这里只把对应
    /// `ModelRoute` 作为可用候选链传给 LLM 层做启动期 provider 校验。
    pub fn llm_config(&self) -> qq_maid_llm::config::LlmConfig {
        qq_maid_llm::config::LlmConfig {
            provider: match self.provider {
                ProviderMode::OpenAi => qq_maid_llm::config::ProviderMode::OpenAi,
                ProviderMode::DeepSeek => qq_maid_llm::config::ProviderMode::DeepSeek,
                ProviderMode::BigModel => qq_maid_llm::config::ProviderMode::BigModel,
                ProviderMode::Gemini => qq_maid_llm::config::ProviderMode::Gemini,
                ProviderMode::Auto => qq_maid_llm::config::ProviderMode::Auto,
            },
            model_route: self.model_route.clone(),
            configured_model_routes: self
                .configured_model_routes()
                .expect("model routes are validated when AppConfig is created")
                .into_iter()
                .collect(),
            openai_api_key: self.openai_api_key.clone(),
            openai_base_url: self.openai_base_url.clone(),
            openai_api_mode: match self.openai_api_mode {
                OpenAiApiMode::Auto => qq_maid_llm::config::OpenAiApiMode::Auto,
                OpenAiApiMode::ChatOnly => qq_maid_llm::config::OpenAiApiMode::ChatOnly,
            },
            deepseek_api_key: self.deepseek_api_key.clone(),
            deepseek_base_url: self.deepseek_base_url.clone(),
            deepseek_model: self.deepseek_model.clone(),
            bigmodel_api_key: self.bigmodel_api_key.clone(),
            bigmodel_base_url: self.bigmodel_base_url.clone(),
            bigmodel_model: self.bigmodel_model.clone(),
            gemini_api_key: self.gemini_api_key.clone(),
            gemini_base_url: self.gemini_base_url.clone(),
            gemini_model: self.gemini_model.clone(),
            openai_compatible_providers: self
                .agent_config
                .provider_configs()
                .into_iter()
                .map(|provider| OpenAiCompatibleProviderConfig {
                    id: provider.id,
                    base_url: provider.base_url,
                    api_key_env: provider.api_key_env.clone(),
                    api_key: env_optional(&provider.api_key_env),
                    auth: HttpAuthConfig {
                        header: provider.auth_header,
                        scheme: provider.auth_scheme,
                    },
                    request_timeout_seconds: provider.request_timeout_seconds,
                })
                .collect(),
            stream: self.stream,
            request_timeout_seconds: self.request_timeout_seconds,
            media_max_bytes: self.media_max_bytes,
            max_output_tokens: self.max_output_tokens,
            openai_search_model: self.openai_search_model.clone(),
        }
    }
}

/// 默认项目通用 SQLite 文件路径。
fn default_app_db_file() -> String {
    DEFAULT_APP_DB_FILE.to_owned()
}

fn sqlite_pool_size_from_env() -> Result<usize, LlmError> {
    Ok(env_u64_bounded_range(
        "QQ_MAID_DB_POOL_MAX_SIZE",
        DEFAULT_SQLITE_POOL_SIZE as u64,
        MIN_SQLITE_POOL_SIZE as u64,
        MAX_SQLITE_POOL_SIZE as u64,
    )? as usize)
}

/// 默认提示词模板目录。
fn default_prompt_dir() -> String {
    DEFAULT_PROMPT_DIR.to_owned()
}

/// 默认 Markdown 知识目录。
fn default_knowledge_dir() -> String {
    DEFAULT_KNOWLEDGE_DIR.to_owned()
}

/// 将字符串解析为 ProviderMode，仅接受 openai / deepseek / bigmodel / gemini / auto。
fn parse_provider(value: &str) -> Result<ProviderMode, LlmError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok(ProviderMode::OpenAi),
        "deepseek" => Ok(ProviderMode::DeepSeek),
        "bigmodel" | "zhipu" | "glm" => Ok(ProviderMode::BigModel),
        "gemini" | "google" => Ok(ProviderMode::Gemini),
        "auto" => Ok(ProviderMode::Auto),
        other => Err(LlmError::config(format!(
            "unsupported LLM_PROVIDER `{other}`; supported: openai, deepseek, bigmodel, gemini, auto"
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

fn reject_removed_env_vars() -> Result<(), LlmError> {
    if env_optional("MEMBER_ID_MAPPING_FILE").is_some() {
        return Err(LlmError::config(
            "MEMBER_ID_MAPPING_FILE has been removed in this version; delete it from config/.env. \
             member_id_mapping.json is no longer read by Core.",
        ));
    }
    Ok(())
}

fn warn_removed_member_id_mapping_file() {
    if Path::new(REMOVED_MEMBER_ID_MAPPING_FILE).exists() {
        tracing::warn!(
            path = REMOVED_MEMBER_ID_MAPPING_FILE,
            "member_id_mapping.json is no longer read; remove this file from the runtime config directory"
        );
    }
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

fn env_status_display_name() -> Result<String, LlmError> {
    let value = env_optional("QQ_MAID_STATUS_DISPLAY_NAME")
        .unwrap_or_else(|| DEFAULT_STATUS_DISPLAY_NAME.to_owned());
    if value.chars().count() > MAX_STATUS_DISPLAY_NAME_CHARS {
        return Err(LlmError::config(format!(
            "QQ_MAID_STATUS_DISPLAY_NAME must be at most {MAX_STATUS_DISPLAY_NAME_CHARS} characters"
        )));
    }
    Ok(value)
}

fn env_daily_reminder_time(name: &str, default: &str) -> Result<DailyReminderTime, LlmError> {
    let value = env_optional(name).unwrap_or_else(|| default.to_owned());
    DailyReminderTime::parse_config(&value, name)
}

fn context_budget_from_env() -> Result<ContextBudgetConfig, LlmError> {
    let config = ContextBudgetConfig {
        context_window_chars: env_u64_bounded(
            "AGENT_CONTEXT_CHAR_LIMIT",
            DEFAULT_AGENT_CONTEXT_CHAR_LIMIT,
            2_000_000,
        )? as usize,
        output_reserve_chars: env_u64_bounded(
            "AGENT_CONTEXT_OUTPUT_RESERVE_CHARS",
            DEFAULT_AGENT_CONTEXT_OUTPUT_RESERVE_CHARS,
            500_000,
        )? as usize,
        protected_recent_turns: env_u64_bounded_zero_allowed(
            "AGENT_CONTEXT_PROTECTED_RECENT_TURNS",
            DEFAULT_AGENT_CONTEXT_PROTECTED_RECENT_TURNS,
            64,
        )? as usize,
    };
    config.validate()?;
    Ok(config)
}

fn parse_two_ascii_digits(high: u8, low: u8) -> Option<u8> {
    if !high.is_ascii_digit() || !low.is_ascii_digit() {
        return None;
    }
    Some((high - b'0') * 10 + (low - b'0'))
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

fn env_optional_openai_model(name: &str) -> Result<Option<String>, LlmError> {
    let Some(value) = env_optional(name) else {
        return Ok(None);
    };
    openai_model_name(&value, name).map(Some)
}

/// 尝试读取联网查询模型环境变量：优先使用指定变量，回退 LLM_MODEL，最后使用默认值。
fn env_openai_model_or(name: &str, llm_model: &str, default: &str) -> Result<String, LlmError> {
    if let Some(value) = env_optional(name) {
        return openai_model_name(&value, name);
    }
    Ok(openai_model_name_from_route(llm_model).unwrap_or_else(|| default.to_owned()))
}

/// 校验查询模型名：允许纯模型名、`openai:` 或 `gemini:` 前缀，拒绝不支持查询工具的 provider。
fn openai_model_name(value: &str, name: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, name)?;
    match model.provider {
        Some(ModelProvider::OpenAi) | Some(ModelProvider::Gemini) | None => {
            Ok(model.to_request_model())
        }
        Some(ModelProvider::DeepSeek)
        | Some(ModelProvider::BigModel)
        | Some(ModelProvider::Custom(_)) => Err(LlmError::config(format!(
            "{name} cannot use provider prefix without supported query tool; supported: openai, gemini"
        ))),
    }
}

/// 从主模型候选链中取第一个支持联网查询工具的候选，用作查询模型的兼容默认值。
///
/// `/查` 不能直接复用普通聊天候选链；因此当 `LLM_MODEL` 同时配置 DeepSeek 等
/// 候选时，这里只取 OpenAI / Gemini 可用项；如果没有可用候选，则由调用方回退
/// 搜索默认模型，避免非查询 provider 部署在未使用 `/查` 时被兼容默认阻塞启动。
fn openai_model_name_from_route(value: &str) -> Option<String> {
    let route = ModelRoute::parse_config(value, "LLM_MODEL").ok()?;
    route
        .candidates()
        .iter()
        .find_map(|model| match model.provider.as_ref() {
            Some(ModelProvider::OpenAi) | Some(ModelProvider::Gemini) | None => {
                Some(model.to_request_model())
            }
            Some(ModelProvider::DeepSeek)
            | Some(ModelProvider::BigModel)
            | Some(ModelProvider::Custom(_)) => None,
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

/// 读取允许 0 的 u64 配置，0 表示禁用限制；非 0 时必须落在硬上限内。
fn env_u64_bounded_zero_allowed(name: &str, default: u64, max: u64) -> Result<u64, LlmError> {
    let Some(value) = env_optional(name) else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u64>()
        .map_err(|_| LlmError::config(format!("unsupported integer value for {name}: {value}")))?;
    if parsed > max {
        return Err(LlmError::config(format!(
            "{name} must be between 0 and {max}"
        )));
    }
    Ok(parsed)
}

/// 读取有硬上限的正整数配置，避免工具循环等能力被配置成过大资源消耗。
fn env_u64_bounded(name: &str, default: u64, max: u64) -> Result<u64, LlmError> {
    let Some(raw) = env_optional(name) else {
        return Ok(default);
    };
    let value = raw
        .parse::<u64>()
        .map_err(|_| LlmError::config(format!("unsupported integer value for {name}: {raw}")))?;
    if value == 0 || value > max {
        return Err(LlmError::config(format!(
            "{name} must be between 1 and {max}"
        )));
    }
    Ok(value)
}

/// 读取带上下限的正整数配置，用于需要保留最小语义表达能力的限制项。
fn env_u64_bounded_range(name: &str, default: u64, min: u64, max: u64) -> Result<u64, LlmError> {
    let Some(raw) = env_optional(name) else {
        return Ok(default);
    };
    let value = raw
        .parse::<u64>()
        .map_err(|_| LlmError::config(format!("unsupported integer value for {name}: {raw}")))?;
    if value < min || value > max {
        return Err(LlmError::config(format!(
            "{name} must be between {min} and {max}"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
