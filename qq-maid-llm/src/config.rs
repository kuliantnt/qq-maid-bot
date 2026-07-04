//! LLM crate 专用配置结构。
//!
//! Core 负责从环境变量解析完整应用配置，本模块只接收已经结构化的 Provider
//! 基础配置，避免 `qq-maid-llm` 反向依赖 core 的业务配置。

use crate::provider::types::{ModelProvider, ModelRoute};

/// LLM 供应商选择模式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderMode {
    /// 使用 OpenAI 兼容 API。
    OpenAi,
    /// 使用 DeepSeek API。
    DeepSeek,
    /// 使用智谱 BigModel API。
    BigModel,
    /// 根据模型 ID 自动选择。
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiApiMode {
    Auto,
    ChatOnly,
}

/// OpenAI-compatible provider 的 HTTP 认证配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpAuthConfig {
    /// 认证头名称，例如 Authorization 或 api-key。
    pub header: String,
    /// 可选认证 scheme，例如 Bearer。为空时直接使用 API key 作为头值。
    pub scheme: Option<String>,
}

impl Default for HttpAuthConfig {
    fn default() -> Self {
        Self {
            header: "Authorization".to_owned(),
            scheme: Some("Bearer".to_owned()),
        }
    }
}

/// 配置文件声明的 OpenAI-compatible provider。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiCompatibleProviderConfig {
    /// provider id，对应模型候选中的 `id:model` 前缀。
    pub id: ModelProvider,
    /// OpenAI-compatible API base URL，不包含 `/chat/completions`。
    pub base_url: String,
    /// API key 环境变量名，用于日志提示和配置诊断。
    pub api_key_env: String,
    /// 从环境变量解析出的 API key。缺失时 auto 模式会跳过该 provider。
    pub api_key: Option<String>,
    /// HTTP 认证头配置。
    pub auth: HttpAuthConfig,
    /// 可选单 provider 请求超时；未配置时继承全局超时。
    pub request_timeout_seconds: Option<u64>,
}

impl ProviderMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::BigModel => "bigmodel",
            Self::Auto => "auto",
        }
    }
}

/// 单个模型调用子系统所需的配置。
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// LLM 供应商（openai / deepseek / bigmodel / auto）。
    pub provider: ProviderMode,
    /// 主模型候选链。
    pub model_route: ModelRoute,
    /// 所有可能通过 `ChatRequest.model` 传入的业务模型候选链。
    pub configured_model_routes: Vec<(String, ModelRoute)>,
    /// OpenAI API 密钥。
    pub openai_api_key: Option<String>,
    /// OpenAI API 基础地址。
    pub openai_base_url: Option<String>,
    /// OpenAI API 模式。
    pub openai_api_mode: OpenAiApiMode,
    /// DeepSeek API 密钥。
    pub deepseek_api_key: Option<String>,
    /// DeepSeek API 基础地址。
    pub deepseek_base_url: String,
    /// DeepSeek 默认模型。
    pub deepseek_model: String,
    /// 智谱 BigModel API 密钥。
    pub bigmodel_api_key: Option<String>,
    /// 智谱 BigModel API 基础地址。
    pub bigmodel_base_url: String,
    /// 智谱 BigModel 默认模型。
    pub bigmodel_model: String,
    /// 配置文件声明的 OpenAI-compatible provider 列表。
    pub openai_compatible_providers: Vec<OpenAiCompatibleProviderConfig>,
    /// 是否启用流式输出。
    pub stream: bool,
    /// 请求超时秒数。
    pub request_timeout_seconds: u64,
    /// 最大输出 token。
    pub max_output_tokens: u64,
    /// OpenAI Web Search 模型。
    pub openai_search_model: String,
}
