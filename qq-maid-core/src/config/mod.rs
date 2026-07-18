//! 应用配置模块。从环境变量加载 LLM 供应商、模型、服务器端口等配置，
//! 提供 `AppConfig` 结构体及其构造方法。

use std::{cell::RefCell, collections::HashMap, env, fmt, net::IpAddr, path::Path, sync::OnceLock};

use qq_maid_common::command_prefix::CommandPrefix;
use qq_maid_llm::config::{HttpAuthConfig, OpenAiCompatibleProviderConfig};
use qq_maid_llm::context_budget::ContextBudgetConfig;
use qq_maid_llm::provider::types::{ModelId, ModelProvider, ModelRoute};

use crate::{
    error::LlmError,
    runtime::tools::weather::{
        default_qweather_api_host, default_qweather_geo_host, qweather_geo_host_from_api_host,
    },
    storage::database::{DEFAULT_SQLITE_POOL_SIZE, MAX_SQLITE_POOL_SIZE, MIN_SQLITE_POOL_SIZE},
};

pub mod agent;
pub mod center;
mod managed;
pub use agent::{
    AgentProfileConfig, AgentRuntimeConfig, AgentSceneConfig, ChatScene, ResolvedAgentPolicy,
    ensure_default_agent_config,
};
pub use managed::managed_config_fields;

// ---- 默认常量 ----
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com"; // DeepSeek 默认 API 地址
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-chat"; // 默认 DeepSeek 模型
pub const DEFAULT_BIGMODEL_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4"; // BigModel 通用 API 地址
pub const DEFAULT_BIGMODEL_MODEL: &str = "glm-5.2"; // 默认 BigModel 模型
pub const DEFAULT_GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai"; // Gemini OpenAI-compatible API 地址
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-2.5-flash"; // 默认 Gemini 模型
pub const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 180; // LLM 请求超时（秒）
pub const DEFAULT_AGENT_FINALIZATION_RESERVE_SECONDS: u64 = 45; // Agent 最终无工具回答预留（秒）
pub const DEFAULT_WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT_SECONDS: u64 = 60; // 搜索首个有效增量超时（秒）
pub const DEFAULT_WEB_SEARCH_IDLE_TIMEOUT_SECONDS: u64 = 30; // 搜索首活动后静默超时（秒）
pub const DEFAULT_WEB_SEARCH_ABSOLUTE_TIMEOUT_SECONDS: u64 = 120; // 单次搜索独立绝对上限（秒）
pub const DEFAULT_TTFT_WARN_SECONDS: u64 = 30; // 首 token 到达告警阈值（秒）
pub const DEFAULT_MEDIA_MAX_BYTES: u64 = 10 * 1024 * 1024; // 单张图片最大处理字节数
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 1200; // LLM 输出最大 token 数
pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1"; // 监听地址
pub const DEFAULT_SERVER_PORT: u16 = 8787; // 监听端口
pub const DEFAULT_APP_DB_FILE: &str = "data/storage/app.db"; // 项目通用 SQLite 文件
pub const DEFAULT_PROMPT_DIR: &str = "config/prompts"; // 提示词模板目录
pub const DEFAULT_KNOWLEDGE_DIR: &str = "config/knowledge"; // Markdown 知识目录
const REMOVED_MEMBER_ID_MAPPING_FILE: &str = "config/member_id_mapping.json";
static RESOLVED_ENVIRONMENT: OnceLock<HashMap<String, String>> = OnceLock::new();
thread_local! {
    /// 配置中心保存前的同步校验只在当前线程临时覆盖 resolver，不写真实进程环境，
    /// 也不会改变启动时安装到 OnceLock 的运行配置快照。
    static VALIDATION_ENVIRONMENT: RefCell<Option<HashMap<String, String>>> = const { RefCell::new(None) };
}
pub const DEFAULT_RSS_POLL_INTERVAL_SECONDS: u64 = 300; // RSS 轮询间隔
pub const DEFAULT_RSS_HTTP_TIMEOUT_SECONDS: u64 = 15; // RSS HTTP 请求超时
pub const DEFAULT_RSS_MAX_BODY_BYTES: u64 = 2 * 1024 * 1024; // RSS 响应体大小上限
pub const DEFAULT_RSS_MAX_PUSH_PER_FEED: u64 = 3; // 单订阅单轮最大推送条数
pub const DEFAULT_RSS_SUMMARY_MAX_CHARS: u64 = 500; // RSS 摘要最大 Unicode 字符数
pub const DEFAULT_RSS_SEEN_RETENTION: u64 = 500; // 每订阅保留的去重指纹数
pub const DEFAULT_RSS_PUSH_MAX_FAILURES: u64 = 3; // 单条目推送失败上限
pub const DEFAULT_RSS_PUSH_MESSAGE_TYPE: &str = "markdown"; // RSS 默认发送安全渲染的 QQ Markdown
pub const DEFAULT_TODO_DAILY_REMINDER_TIME: &str = "09:00"; // Todo 每日提醒默认时间
pub const DEFAULT_MAX_CONCURRENT_RESPONSES: u64 = 8; // 全局 LLM / Web Search 最大并发
pub const DEFAULT_AGENT_CONTEXT_CHAR_LIMIT: u64 = 48_000; // 本地上下文窗口字符预算
pub const DEFAULT_AGENT_CONTEXT_OUTPUT_RESERVE_CHARS: u64 = 6_000; // 为模型输出预留的字符预算
pub const DEFAULT_AGENT_CONTEXT_PROTECTED_RECENT_TURNS: u64 = 4; // 最近完整对话轮次保护数量
pub const DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT: u64 =
    qq_maid_llm::tool::DEFAULT_TOOL_OUTPUT_MAX_CHARS as u64; // 单项工具结果最大字符数
pub const MIN_AGENT_TOOL_RESULT_CHAR_LIMIT: u64 =
    qq_maid_llm::tool::MIN_TOOL_OUTPUT_MAX_CHARS as u64; // 至少能表达 {"truncated":true}
pub const DEFAULT_BOT_DISPLAY_NAME: &str = "小女仆"; // 主动关键词未配置时使用的机器人主称呼
pub const DEFAULT_MEMORY_CONSOLIDATION_CHECK_INTERVAL_SECONDS: u64 = 3_600;
pub const DEFAULT_MEMORY_CONSOLIDATION_MIN_INTERVAL_SECONDS: u64 = 86_400;
pub const DEFAULT_MEMORY_CONSOLIDATION_MIN_NEW_RECORDS: u64 = 10;
pub const DEFAULT_MEMORY_CONSOLIDATION_MIN_DISTINCT_SOURCES: u64 = 3;
pub const DEFAULT_MEMORY_CONSOLIDATION_MAX_RECORDS: u64 = 100;
pub const DEFAULT_MEMORY_CONSOLIDATION_MAX_INPUT_CHARS: u64 = 32_000;
pub const DEFAULT_MEMORY_DREAM_MIN_INTERVAL_SECONDS: u64 = 86_400;
pub const DEFAULT_MEMORY_DREAM_MIN_NEW_SESSIONS: u64 = 5;
pub const DEFAULT_MEMORY_DREAM_MAX_SESSIONS: u64 = 20;
pub const DEFAULT_MEMORY_DREAM_MAX_INPUT_CHARS: u64 = 32_000;
pub const DEFAULT_MEMORY_DREAM_MAX_OUTPUT_MEMORIES: u64 = 8;
pub const MAX_BOT_DISPLAY_NAME_CHARS: usize = 24; // 避免配置过长导致状态提示刷屏
pub const MIN_MEDIA_MAX_BYTES: u64 = 64 * 1024;
pub const MAX_MEDIA_MAX_BYTES: u64 = 100 * 1024 * 1024;

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

/// 完整应用配置：普通运行项从环境读取，Agent 策略从 `agent.toml` 读取。
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// 统一 Agent 场景运行策略，启动阶段完成解析和校验。
    pub agent_config: AgentRuntimeConfig,
    /// 配置驱动的 `/ops` 白名单；文件缺失或总开关关闭时不会执行任何程序。
    pub ops_config: crate::runtime::tools::ops::OpsConfig,
    /// 所有聊天入口共用的单字符命令前缀；只在进程启动时读取。
    pub command_prefix: CommandPrefix,
    /// OpenAI API 密钥
    pub openai_api_key: Option<String>,
    /// OpenAI API 基础地址
    pub openai_base_url: Option<String>,
    pub openai_api_mode: OpenAiApiMode,
    /// DeepSeek API 密钥
    pub deepseek_api_key: Option<String>,
    /// DeepSeek API 基础地址
    pub deepseek_base_url: String,
    /// 智谱 BigModel API 密钥
    pub bigmodel_api_key: Option<String>,
    /// 智谱 BigModel API 基础地址
    pub bigmodel_base_url: String,
    /// Google Gemini API 密钥
    pub gemini_api_key: Option<String>,
    /// Google Gemini OpenAI-compatible API 基础地址
    pub gemini_base_url: String,
    /// 是否启用流式输出
    pub stream: bool,
    /// LLM 请求超时秒数
    pub request_timeout_seconds: u64,
    /// Agent 最后一轮无工具回答的配置预留；运行时还会按总请求预算裁剪。
    pub agent_finalization_reserve_seconds: u64,
    /// 搜索等待首个非空增量的超时秒数。
    pub web_search_first_activity_timeout_seconds: u64,
    /// 搜索收到首个非空增量后的静默超时秒数。
    pub web_search_idle_timeout_seconds: u64,
    /// 单次搜索独立于整体请求的绝对超时秒数。
    pub web_search_absolute_timeout_seconds: u64,
    /// 首 token 到达告警阈值（秒）
    pub ttft_warn_seconds: u64,
    /// 单张图片允许转成本地 data URL 的最大字节数。
    pub media_max_bytes: u64,
    /// 全局 LLM 与 `/查` 共享的最大并发数；0 表示不限制。
    pub max_concurrent_responses: u64,
    /// 聊天输入上下文预算；由 Core 装配层读取并传给 LLM 请求。
    pub context_budget: ContextBudgetConfig,
    /// 单项 Tool 输出最大字符数；不属于上下文预算，直接注入 ToolRegistry。
    pub tool_result_max_chars: usize,
    /// 机器人对外主称呼，取群聊主动关键词中的第一个有效值。
    pub bot_display_name: String,
    /// HTTP 监听地址
    pub server_host: String,
    /// HTTP 监听端口
    pub server_port: u16,
    /// 项目通用 SQLite 文件路径；RSS、Todo、Session 和 Memory 共用该数据库。
    pub app_db_file: String,
    /// 本地 SQLite 连接池大小，独立于 LLM / Web Search 并发限制。
    pub sqlite_pool_size: usize,
    /// 是否启用确定性 Memory 后台整理；默认关闭，避免升级后隐式改变长期记忆。
    pub memory_consolidation_enabled: bool,
    pub memory_consolidation_check_interval_seconds: u64,
    pub memory_consolidation_min_interval_seconds: u64,
    pub memory_consolidation_min_new_records: u64,
    /// 只统计非空安全 source_ref；缺失来源不会用 Memory ID 兜底。
    pub memory_consolidation_min_distinct_sources: u64,
    pub memory_consolidation_max_records: u64,
    pub memory_consolidation_max_input_chars: u64,
    /// 是否启用模型 Session Dream 与自动长期记忆；与确定性整理独立控制。
    pub memory_dream_enabled: bool,
    /// 以下参数只控制 Dream 门槛和批次。
    pub memory_dream_min_interval_seconds: u64,
    pub memory_dream_min_new_sessions: u64,
    pub memory_dream_max_sessions: u64,
    pub memory_dream_max_input_chars: u64,
    pub memory_dream_max_output_memories: u64,
    /// 是否启用 RSS 后台轮询
    pub rss_enabled: bool,
    /// 是否启用 RSS 推送前模型翻译；默认关闭以避免后台隐式消耗 token。
    pub rss_translation_enabled: bool,
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
    /// 和风天气 API 密钥；为空时天气能力关闭。
    pub qweather_api_key: String,
    /// 和风天气 API 主机地址
    pub qweather_api_host: String,
    /// 和风天气地理编码 API 主机地址
    pub qweather_geo_host: String,
    /// 是否启用本地 Web 控制台和 Markdown 预览接口。
    pub web_console_enabled: bool,
    /// 控制台跨域 allowlist；为空时仅同源访问。
    pub web_console_allowed_origins: Vec<String>,
    /// 只有 TCP 对端命中此列表时才信任其转发来源头。
    pub web_console_trusted_proxy_ips: Vec<IpAddr>,
    /// 生产 HTTPS 模式显式开启；同时使用 Secure 与 __Host- Cookie。
    pub web_console_secure_cookies: bool,
}

/// 业务配置尚未完成时也足以启动健康检查与受保护管理入口的最小配置。
#[derive(Debug, Clone)]
pub struct ManagementBootstrapConfig {
    pub server_host: String,
    pub server_port: u16,
    pub app_db_file: String,
    pub sqlite_pool_size: usize,
    pub web_console_enabled: bool,
    pub web_console_allowed_origins: Vec<String>,
    pub web_console_trusted_proxy_ips: Vec<IpAddr>,
    pub web_console_secure_cookies: bool,
}

impl AppConfig {
    /// 从环境变量构造配置对象。关键词必须配置，其余有默认值。
    pub fn from_env() -> Result<Self, LlmError> {
        reject_removed_env_vars()?;
        warn_removed_member_id_mapping_file();
        let qweather_api_key = env_optional("QWEATHER_API_KEY").unwrap_or_default();
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
        let web_console_trusted_proxy_ips = env_ip_list("WEB_CONSOLE_TRUSTED_PROXY_IPS")?;
        let context_budget = context_budget_from_env()?;
        let tool_result_max_chars = env_u64_bounded_range(
            "AGENT_TOOL_RESULT_CHAR_LIMIT",
            DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT,
            MIN_AGENT_TOOL_RESULT_CHAR_LIMIT,
            200_000,
        )? as usize;
        let media_max_bytes = env_u64_bounded_range(
            "QQ_MAID_MEDIA_MAX_BYTES",
            DEFAULT_MEDIA_MAX_BYTES,
            MIN_MEDIA_MAX_BYTES,
            MAX_MEDIA_MAX_BYTES,
        )?;
        let effective_environment = effective_environment();
        let agent_config = AgentRuntimeConfig::load_from_environment(&effective_environment)?;
        let ops_config =
            crate::runtime::tools::ops::OpsConfig::load_from_environment(&effective_environment)?;
        let command_prefix = CommandPrefix::parse(&env_string("CHAT_COMMAND_PREFIX", "/"))
            .map_err(|error| LlmError::config(format!("invalid CHAT_COMMAND_PREFIX: {error}")))?;

        Ok(Self {
            agent_config,
            ops_config,
            command_prefix,
            openai_api_key: env_optional("OPENAI_API_KEY"),
            openai_base_url: openai_base_url_from_env(),
            openai_api_mode: parse_openai_api_mode(&env_string("OPENAI_API_MODE", "auto"))?,
            deepseek_api_key: env_optional("DEEPSEEK_API_KEY"),
            deepseek_base_url: env_string("DEEPSEEK_BASE_URL", DEFAULT_DEEPSEEK_BASE_URL),
            bigmodel_api_key: env_optional("BIGMODEL_API_KEY"),
            bigmodel_base_url: env_string("BIGMODEL_BASE_URL", DEFAULT_BIGMODEL_BASE_URL),
            gemini_api_key: env_optional("GEMINI_API_KEY"),
            gemini_base_url: env_string("GEMINI_BASE_URL", DEFAULT_GEMINI_BASE_URL),
            stream: env_bool("LLM_STREAM", true)?,
            request_timeout_seconds: env_u64(
                "LLM_REQUEST_TIMEOUT_SECONDS",
                DEFAULT_REQUEST_TIMEOUT_SECONDS,
            )?,
            agent_finalization_reserve_seconds: env_u64(
                "AGENT_FINALIZATION_RESERVE_SECONDS",
                DEFAULT_AGENT_FINALIZATION_RESERVE_SECONDS,
            )?,
            web_search_first_activity_timeout_seconds: env_u64(
                "WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT_SECONDS",
                DEFAULT_WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT_SECONDS,
            )?,
            web_search_idle_timeout_seconds: env_u64(
                "WEB_SEARCH_IDLE_TIMEOUT_SECONDS",
                DEFAULT_WEB_SEARCH_IDLE_TIMEOUT_SECONDS,
            )?,
            web_search_absolute_timeout_seconds: env_u64(
                "WEB_SEARCH_ABSOLUTE_TIMEOUT_SECONDS",
                DEFAULT_WEB_SEARCH_ABSOLUTE_TIMEOUT_SECONDS,
            )?,
            ttft_warn_seconds: env_u64("LLM_TTFT_WARN_SECONDS", DEFAULT_TTFT_WARN_SECONDS)?,
            media_max_bytes,
            max_concurrent_responses: env_u64_bounded_zero_allowed(
                "MAX_CONCURRENT_RESPONSES",
                DEFAULT_MAX_CONCURRENT_RESPONSES,
                256,
            )?,
            context_budget,
            tool_result_max_chars,
            bot_display_name: env_bot_display_name()?,
            server_host: env_string("LLM_SERVER_HOST", DEFAULT_SERVER_HOST),
            server_port: env_u16("LLM_SERVER_PORT", DEFAULT_SERVER_PORT)?,
            app_db_file: env_optional("APP_DB_FILE").unwrap_or_else(default_app_db_file),
            sqlite_pool_size: sqlite_pool_size_from_env()?,
            memory_consolidation_enabled: env_bool("MEMORY_CONSOLIDATION_ENABLED", false)?,
            memory_consolidation_check_interval_seconds: env_u64_bounded_range(
                "MEMORY_CONSOLIDATION_CHECK_INTERVAL_SECONDS",
                DEFAULT_MEMORY_CONSOLIDATION_CHECK_INTERVAL_SECONDS,
                60,
                86_400,
            )?,
            memory_consolidation_min_interval_seconds: env_u64_bounded_range(
                "MEMORY_CONSOLIDATION_MIN_INTERVAL_SECONDS",
                DEFAULT_MEMORY_CONSOLIDATION_MIN_INTERVAL_SECONDS,
                60,
                2_592_000,
            )?,
            memory_consolidation_min_new_records: env_u64_bounded_range(
                "MEMORY_CONSOLIDATION_MIN_NEW_RECORDS",
                DEFAULT_MEMORY_CONSOLIDATION_MIN_NEW_RECORDS,
                2,
                1_000,
            )?,
            memory_consolidation_min_distinct_sources: env_u64_bounded_range(
                "MEMORY_CONSOLIDATION_MIN_DISTINCT_SOURCES",
                DEFAULT_MEMORY_CONSOLIDATION_MIN_DISTINCT_SOURCES,
                1,
                1_000,
            )?,
            memory_consolidation_max_records: env_u64_bounded_range(
                "MEMORY_CONSOLIDATION_MAX_RECORDS",
                DEFAULT_MEMORY_CONSOLIDATION_MAX_RECORDS,
                2,
                500,
            )?,
            memory_consolidation_max_input_chars: env_u64_bounded_range(
                "MEMORY_CONSOLIDATION_MAX_INPUT_CHARS",
                DEFAULT_MEMORY_CONSOLIDATION_MAX_INPUT_CHARS,
                1_000,
                200_000,
            )?,
            memory_dream_enabled: env_bool("MEMORY_DREAM_ENABLED", false)?,
            memory_dream_min_interval_seconds: env_u64_bounded_range(
                "MEMORY_DREAM_MIN_INTERVAL_SECONDS",
                DEFAULT_MEMORY_DREAM_MIN_INTERVAL_SECONDS,
                60,
                2_592_000,
            )?,
            memory_dream_min_new_sessions: env_u64_bounded_range(
                "MEMORY_DREAM_MIN_NEW_SESSIONS",
                DEFAULT_MEMORY_DREAM_MIN_NEW_SESSIONS,
                1,
                1_000,
            )?,
            memory_dream_max_sessions: env_u64_bounded_range(
                "MEMORY_DREAM_MAX_SESSIONS",
                DEFAULT_MEMORY_DREAM_MAX_SESSIONS,
                1,
                500,
            )?,
            memory_dream_max_input_chars: env_u64_bounded_range(
                "MEMORY_DREAM_MAX_INPUT_CHARS",
                DEFAULT_MEMORY_DREAM_MAX_INPUT_CHARS,
                1_000,
                200_000,
            )?,
            memory_dream_max_output_memories: env_u64_bounded_range(
                "MEMORY_DREAM_MAX_OUTPUT_MEMORIES",
                DEFAULT_MEMORY_DREAM_MAX_OUTPUT_MEMORIES,
                1,
                50,
            )?,
            rss_enabled: env_bool("RSS_ENABLED", true)?,
            rss_translation_enabled: env_bool("RSS_TRANSLATION_ENABLED", false)?,
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
            // 未配置的新实例必须能进入受保护的首次向导；监听仍默认只绑定回环地址。
            // 高级部署可显式设置 false，使页面、认证和全部配置 API 都不注册。
            web_console_enabled: env_bool("WEB_CONSOLE_ENABLED", true)?,
            web_console_allowed_origins,
            web_console_trusted_proxy_ips,
            web_console_secure_cookies: env_bool("WEB_CONSOLE_SECURE_COOKIES", false)?,
        })
    }

    /// 使用与正式启动完全相同的 Core 解析规则校验候选环境。
    ///
    /// 候选视图通过线程局部作用域注入，避免把解密 secret 写入 `std::env` 或全局运行快照。
    pub fn validate_environment(environment: &HashMap<String, String>) -> Result<(), LlmError> {
        let _guard = ValidationEnvironmentGuard::install(environment.clone());
        Self::from_env().map(|_| ())
    }

    /// 使用候选环境与可选的候选 Agent 文档执行无网络启动预检。
    ///
    /// Agent 配置中心保存时传入尚未落盘的候选对象；runtime/secret 保存传 `None`，
    /// 由正式加载器读取当前 `AGENT_CONFIG_FILE`。Provider 路由判断直接复用 LLM crate
    /// 中 `build_provider` 使用的纯配置计划，不创建 HTTP client 或发送请求。
    pub fn preflight_environment(
        environment: &HashMap<String, String>,
        candidate_agent: Option<&AgentRuntimeConfig>,
    ) -> Result<(), LlmError> {
        let _guard = ValidationEnvironmentGuard::install(environment.clone());
        let mut config = Self::from_env()?;
        if let Some(candidate_agent) = candidate_agent {
            config.agent_config = candidate_agent.clone();
        }
        qq_maid_llm::provider::preflight_provider_config(&config.llm_config())
    }

    /// 返回所有可能作为 `ChatRequest.model` 传入 provider 层的模型候选链。
    ///
    /// 这个列表用于启动阶段校验和 Provider 初始化，唯一来源是 `agent.toml`。
    pub fn configured_model_routes(&self) -> Result<Vec<(String, ModelRoute)>, LlmError> {
        Ok(self.agent_config.configured_model_routes())
    }

    /// 提取 LLM crate 所需的 Provider 基础配置。
    ///
    /// 标题、记忆、压缩和翻译模型由 Agent Profile 的辅助路线解析；这里把全部
    /// `ModelRoute` 传给 LLM 层做启动期 Provider 校验。
    pub fn llm_config(&self) -> qq_maid_llm::config::LlmConfig {
        let private_policy = self
            .agent_config
            .resolve(ChatScene::Private)
            .expect("agent config is validated when AppConfig is created");
        qq_maid_llm::config::LlmConfig {
            provider: qq_maid_llm::config::ProviderMode::Auto,
            model_route: private_policy.main_route,
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
            deepseek_model: DEFAULT_DEEPSEEK_MODEL.to_owned(),
            bigmodel_api_key: self.bigmodel_api_key.clone(),
            bigmodel_base_url: self.bigmodel_base_url.clone(),
            bigmodel_model: DEFAULT_BIGMODEL_MODEL.to_owned(),
            gemini_api_key: self.gemini_api_key.clone(),
            gemini_base_url: self.gemini_base_url.clone(),
            gemini_model: DEFAULT_GEMINI_MODEL.to_owned(),
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
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            openai_search_model: private_policy.search_model,
        }
    }
}

/// 安装配置中心合并后的进程级配置视图。
///
/// 统一入口只允许在启动早期安装一次；之后所有旧环境变量 resolver 都从该只读快照取值，
/// 避免把 SQLite 中解密出的 secret 写回真实进程环境。独立 Core 入口未安装时仍完全沿用
/// 原有 `std::env` 行为。
pub fn install_resolved_environment(environment: HashMap<String, String>) -> Result<(), LlmError> {
    RESOLVED_ENVIRONMENT.set(environment).map_err(|_| {
        LlmError::config("resolved configuration environment has already been installed")
    })
}

/// 从外部部署配置中解析数据库 Bootstrap 项。
///
/// 数据库必须先于配置中心打开，因此这里只读取进程环境/dotenv 和安全默认值，受管 TOML
/// 无权改变数据库位置或连接池大小。
pub fn database_bootstrap_from_environment(
    environment: &HashMap<String, String>,
) -> Result<(String, usize), LlmError> {
    let db_file = environment
        .get("APP_DB_FILE")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_APP_DB_FILE)
        .to_owned();
    let pool_size = match environment
        .get("QQ_MAID_DB_POOL_MAX_SIZE")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => raw.parse::<usize>().map_err(|_| {
            LlmError::config(format!(
                "unsupported integer value for QQ_MAID_DB_POOL_MAX_SIZE: {raw}"
            ))
        })?,
        None => DEFAULT_SQLITE_POOL_SIZE,
    };
    if !(MIN_SQLITE_POOL_SIZE..=MAX_SQLITE_POOL_SIZE).contains(&pool_size) {
        return Err(LlmError::config(format!(
            "QQ_MAID_DB_POOL_MAX_SIZE must be between {MIN_SQLITE_POOL_SIZE} and {MAX_SQLITE_POOL_SIZE}"
        )));
    }
    Ok((db_file, pool_size))
}

/// 只解析可让管理恢复入口安全启动的 Bootstrap 项，不要求 Provider、平台凭据或业务参数。
pub fn management_bootstrap_from_environment(
    environment: &HashMap<String, String>,
) -> Result<ManagementBootstrapConfig, LlmError> {
    let _guard = ValidationEnvironmentGuard::install(environment.clone());
    let (app_db_file, sqlite_pool_size) = database_bootstrap_from_environment(environment)?;
    Ok(ManagementBootstrapConfig {
        server_host: env_string("LLM_SERVER_HOST", DEFAULT_SERVER_HOST),
        server_port: env_u16("LLM_SERVER_PORT", DEFAULT_SERVER_PORT)?,
        app_db_file,
        sqlite_pool_size,
        web_console_enabled: env_bool("WEB_CONSOLE_ENABLED", true)?,
        web_console_allowed_origins: env_list("WEB_CONSOLE_ALLOWED_ORIGINS"),
        web_console_trusted_proxy_ips: env_ip_list("WEB_CONSOLE_TRUSTED_PROXY_IPS")?,
        web_console_secure_cookies: env_bool("WEB_CONSOLE_SECURE_COOKIES", false)?,
    })
}

fn effective_environment() -> HashMap<String, String> {
    if let Some(environment) = validation_environment() {
        return environment;
    }
    RESOLVED_ENVIRONMENT
        .get()
        .cloned()
        .unwrap_or_else(|| env::vars().collect())
}

fn configured_value(name: &str) -> Option<String> {
    if let Some(value) = VALIDATION_ENVIRONMENT.with(|environment| {
        environment
            .borrow()
            .as_ref()
            .and_then(|environment| environment.get(name).cloned())
    }) {
        return Some(value);
    }
    if VALIDATION_ENVIRONMENT.with(|environment| environment.borrow().is_some()) {
        return None;
    }
    RESOLVED_ENVIRONMENT
        .get()
        .and_then(|environment| environment.get(name).cloned())
        .or_else(|| {
            if RESOLVED_ENVIRONMENT.get().is_some() {
                None
            } else {
                env::var(name).ok()
            }
        })
}

fn validation_environment() -> Option<HashMap<String, String>> {
    VALIDATION_ENVIRONMENT.with(|environment| environment.borrow().clone())
}

struct ValidationEnvironmentGuard(Option<HashMap<String, String>>);

impl ValidationEnvironmentGuard {
    fn install(environment: HashMap<String, String>) -> Self {
        Self(VALIDATION_ENVIRONMENT.with(|current| current.borrow_mut().replace(environment)))
    }
}

impl Drop for ValidationEnvironmentGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        VALIDATION_ENVIRONMENT.with(|current| {
            *current.borrow_mut() = previous;
        });
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
    configured_value(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn reject_removed_env_vars() -> Result<(), LlmError> {
    const REMOVED_AGENT_ENV_VARS: &[&str] = &[
        "LLM_PROVIDER",
        "OPENAI_MODEL",
        "LLM_MODEL",
        "PRIVATE_LLM_MODEL",
        "GROUP_LLM_MODEL",
        "OPENAI_SEARCH_MODEL",
        "PRIVATE_OPENAI_SEARCH_MODEL",
        "GROUP_OPENAI_SEARCH_MODEL",
        "TITLE_MODEL",
        "MEMORY_MODEL",
        "COMPACT_MODEL",
        "TRANSLATION_MODEL",
        "DEEPSEEK_MODEL",
        "BIGMODEL_MODEL",
        "GEMINI_MODEL",
        "LLM_MAX_OUTPUT_TOKENS",
        "TOOL_CALLING_ENABLED",
        "TOOL_CALLING_GROUP_ENABLED",
        "TOOL_CALLING_MAX_ROUNDS",
    ];
    for name in REMOVED_AGENT_ENV_VARS {
        if env_optional(name).is_some() {
            return Err(LlmError::config(format!(
                "{name} has been removed; configure Agent policy in AGENT_CONFIG_FILE"
            )));
        }
    }
    if env_optional("TODO_MODEL").is_some() {
        return Err(LlmError::config(
            "TODO_MODEL has been removed in this version; delete it from config/.env. \
             Todo writes use the configured Agent Tool Calling route.",
        ));
    }
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

fn env_ip_list(name: &str) -> Result<Vec<IpAddr>, LlmError> {
    env_list(name)
        .into_iter()
        .map(|value| {
            value
                .parse::<IpAddr>()
                .map_err(|_| LlmError::config(format!("unsupported IP address in {name}: {value}")))
        })
        .collect()
}

fn env_bot_display_name() -> Result<String, LlmError> {
    // 新配置显式存在时，即使清理后为空也按默认主称呼处理，不能被旧显示名覆盖。
    // 只有完全未设置主动关键词时才兼容旧变量，便于已有部署平滑迁移。
    let (value, source) = match configured_value("QQ_MAID_GROUP_ACTIVE_KEYWORDS") {
        Some(raw) => (
            raw.split(',')
                .map(str::trim)
                .find(|value| !value.is_empty())
                .unwrap_or(DEFAULT_BOT_DISPLAY_NAME)
                .to_owned(),
            "QQ_MAID_GROUP_ACTIVE_KEYWORDS",
        ),
        None => (
            env_optional("QQ_MAID_STATUS_DISPLAY_NAME")
                .unwrap_or_else(|| DEFAULT_BOT_DISPLAY_NAME.to_owned()),
            "QQ_MAID_STATUS_DISPLAY_NAME",
        ),
    };
    if value.chars().count() > MAX_BOT_DISPLAY_NAME_CHARS {
        return Err(LlmError::config(format!(
            "{source} primary display name must be at most {MAX_BOT_DISPLAY_NAME_CHARS} characters"
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

/// 从 `OPENAI_BASE_URLS` 读取 OpenAI 基础地址，逗号分隔时取第一个非空值。
fn openai_base_url_from_env() -> Option<String> {
    first_openai_base_url(env_optional("OPENAI_BASE_URLS").as_deref())
}

/// 从逗号分隔的 URL 中取第一个非空值。
fn first_openai_base_url(base_urls: Option<&str>) -> Option<String> {
    base_urls
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
