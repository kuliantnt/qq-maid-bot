//! Google Gemini 提供商实现。
//!
//! Gemini 普通聊天复用官方 OpenAI-compatible Chat Completions 端点；本模块只维护
//! Gemini 的内置配置项、模型前缀规则和 provider 名称，避免复制通用协议实现。

use async_trait::async_trait;

use crate::{
    agent_loop::{AgentSessionRequest, AgentStepSession},
    config::{HttpAuthConfig, LlmConfig, OpenAiCompatibleProviderConfig},
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, ToolCallingProtocol,
        openai_compatible::OpenAiCompatibleProvider,
        types::{ChatRequest, ModelId, ModelProvider},
    },
};

/// Gemini 提供商实现。
pub struct GeminiProvider {
    inner: OpenAiCompatibleProvider,
}

impl GeminiProvider {
    /// 从 LLM 配置创建 Gemini 提供商实例。
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let default_model = gemini_config_model(&config.gemini_model)?;
        let provider_config = OpenAiCompatibleProviderConfig {
            id: ModelProvider::Gemini,
            base_url: config.gemini_base_url.clone(),
            api_key_env: "GEMINI_API_KEY".to_owned(),
            api_key: config.gemini_api_key.clone(),
            auth: HttpAuthConfig::default(),
            request_timeout_seconds: None,
        };
        Ok(Self {
            inner: OpenAiCompatibleProvider::new(
                &provider_config,
                default_model,
                config.stream,
                config.request_timeout_seconds,
                config.media_max_bytes,
                config.max_output_tokens,
            )?,
        })
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        self.inner.chat(req).await
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        self.inner.stream_chat(req).await
    }

    async fn begin_agent_session(
        &self,
        req: AgentSessionRequest<'_>,
    ) -> Result<Option<Box<dyn AgentStepSession + Send>>, LlmError> {
        self.inner.begin_agent_session(req).await
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.inner.tool_calling_protocol(model)
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn stream_enabled(&self) -> bool {
        self.inner.stream_enabled()
    }
}

/// 验证并解析 Gemini 的配置模型名。
pub(crate) fn gemini_config_model(value: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, "GEMINI_MODEL")?;
    match model.provider {
        Some(ModelProvider::Gemini) | None => Ok(model.name),
        Some(ModelProvider::OpenAi)
        | Some(ModelProvider::DeepSeek)
        | Some(ModelProvider::BigModel)
        | Some(ModelProvider::Custom(_)) => Err(LlmError::config(
            "GEMINI_MODEL must use gemini: prefix or no prefix",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_config_model_accepts_gemini_prefix_and_bare_model() {
        assert_eq!(
            gemini_config_model("gemini:gemini-2.5-flash").unwrap(),
            "gemini-2.5-flash"
        );
        assert_eq!(
            gemini_config_model("gemini-2.5-pro").unwrap(),
            "gemini-2.5-pro"
        );
    }

    #[test]
    fn gemini_config_model_rejects_other_provider_prefix() {
        let err = gemini_config_model("openai:gpt-5.5").unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("GEMINI_MODEL"));
    }
}
