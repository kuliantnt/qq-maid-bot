//! DeepSeek 提供商：请求前选择旧模型 Chat 兼容路径或公共 Responses adapter。

use async_trait::async_trait;

use crate::{
    agent_loop::{AgentSessionRequest, AgentStepSession},
    config::{LlmConfig, OpenAiCompatibleProviderConfig, OpenAiResponsesProviderConfig},
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, ToolCallingProtocol,
        openai::ConfiguredResponsesProvider,
        openai_compatible::OpenAiCompatibleProvider,
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

/// 仅精确识别历史 Chat 模型；其他用户显式模型保持 Responses 协议，原样传给上游。
/// 这份协议兼容规则不用于推断视觉能力，也不依赖 HTTP 错误触发回退。
pub fn is_legacy_chat_model(model: &str) -> bool {
    matches!(model, "deepseek-chat" | "deepseek-reasoner")
}

/// DeepSeek 只装配供应商身份与协议选择，不复制协议实现。
pub struct DeepSeekProvider {
    inner: ConfiguredResponsesProvider,
    legacy_chat: OpenAiCompatibleProvider,
}

impl DeepSeekProvider {
    /// 从 LLM 配置创建 DeepSeek 提供商实例。
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let connection = responses_config(config);
        let model = deepseek_config_model(&config.deepseek_model)?;
        Ok(Self {
            legacy_chat: OpenAiCompatibleProvider::new(
                &OpenAiCompatibleProviderConfig {
                    id: connection.id.clone(),
                    base_url: connection.base_url.clone(),
                    api_key_env: connection.api_key_env.clone(),
                    api_key: connection.api_key.clone(),
                    auth: connection.auth.clone(),
                    request_timeout_seconds: connection.request_timeout_seconds,
                },
                model.clone(),
                config.stream,
                config.request_timeout_seconds,
                config.media_max_bytes,
                config.max_output_tokens,
            )?,
            inner: ConfiguredResponsesProvider::new(
                &connection,
                model,
                config.stream,
                config.request_timeout_seconds,
                config.media_max_bytes,
                config.max_output_tokens,
            )?,
        })
    }

    fn adapter(&self, model: Option<&str>) -> &dyn LlmProvider {
        // 解析后的裸名称只用于选协议；前缀合法性仍由公共 adapter 校验。
        let legacy = ModelId::parse(model.unwrap_or(self.model()), "request")
            .is_ok_and(|model| is_legacy_chat_model(&model.name));
        if legacy {
            &self.legacy_chat
        } else {
            &self.inner
        }
    }
}

#[async_trait]
impl LlmProvider for DeepSeekProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        self.adapter(req.model.as_deref()).chat(req).await
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        self.adapter(req.model.as_deref()).stream_chat(req).await
    }

    async fn begin_agent_session(
        &self,
        req: AgentSessionRequest<'_>,
    ) -> Result<Option<Box<dyn AgentStepSession + Send>>, LlmError> {
        self.adapter(req.chat.model.as_deref())
            .begin_agent_session(req)
            .await
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.adapter(model).tool_calling_protocol(model)
    }

    fn supports_vision(&self, model: Option<&str>) -> bool {
        // 仅声明 adapter 能编码图片；实际视觉能力由模型元数据/上游决定，
        // 不根据临时模型名称猜测，也不在供应商层丢弃图片。
        self.adapter(model).supports_vision(model)
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
