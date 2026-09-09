//! DeepSeek 提供商：复用公共 Responses 请求、SSE、图片编码与 Tool Loop。

use async_trait::async_trait;

use crate::{
    agent_loop::{AgentSessionRequest, AgentStepSession},
    config::{LlmConfig, OpenAiResponsesProviderConfig},
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, ToolCallingProtocol,
        openai::ConfiguredResponsesProvider,
        types::{ChatRequest, ModelId, ModelProvider},
    },
};

/// 内置 DeepSeek 的连接元数据；聊天与原生搜索必须使用同一份认证和地址。
pub(crate) fn responses_config(config: &LlmConfig) -> OpenAiResponsesProviderConfig {
    OpenAiResponsesProviderConfig {
        id: ModelProvider::DeepSeek,
        base_url: config.deepseek_base_url.clone(),
        api_key_env: "DEEPSEEK_API_KEY".to_owned(),
        api_key: config.deepseek_api_key.clone(),
        auth: Default::default(),
        request_timeout_seconds: None,
        // 普通聊天和工具循环保持相同协议；失败由已有候选链处理。
        chat_fallback: false,
    }
}

/// DeepSeek 只装配供应商身份，不复制 Responses 协议实现。
pub struct DeepSeekProvider {
    inner: ConfiguredResponsesProvider,
}

impl DeepSeekProvider {
    /// 从 LLM 配置创建 DeepSeek 提供商实例。
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        Ok(Self {
            inner: ConfiguredResponsesProvider::new(
                &responses_config(config),
                deepseek_config_model(&config.deepseek_model)?,
                config.stream,
                config.request_timeout_seconds,
                config.media_max_bytes,
                config.max_output_tokens,
            )?,
        })
    }
}

#[async_trait]
impl LlmProvider for DeepSeekProvider {
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

    fn supports_vision(&self, model: Option<&str>) -> bool {
        // 仅声明 adapter 能编码 input_image；实际视觉能力由模型元数据/上游决定，
        // 不根据临时模型名称猜测，也不在供应商层丢弃图片。
        self.inner.supports_vision(model)
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

/// 验证并解析 DeepSeek 的配置模型名。
pub(crate) fn deepseek_config_model(value: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, "DEEPSEEK_MODEL")?;
    match model.provider {
        Some(ModelProvider::DeepSeek) | None => Ok(model.name),
        Some(ModelProvider::OpenAi)
        | Some(ModelProvider::BigModel)
        | Some(ModelProvider::Gemini)
        | Some(ModelProvider::Custom(_)) => Err(LlmError::config(
            "DEEPSEEK_MODEL must use deepseek: prefix or no prefix",
        )),
    }
}

#[cfg(test)]
mod tests;
