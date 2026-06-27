//! DeepSeek 提供商实现。
//!
//! DeepSeek 使用 OpenAI 兼容 Chat Completions 协议；本模块只维护
//! DeepSeek 的 base URL、认证和模型前缀规则差异。

use std::time::Duration;

use async_trait::async_trait;

use crate::{
    config::LlmConfig,
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider,
        openai::{ChatCompletionsClient, chat_completions_with_stream_fallback},
        types::{ChatRequest, ModelId, ModelProvider},
    },
};

/// DeepSeek 提供商实现。
pub struct DeepSeekProvider {
    /// OpenAI 兼容 Chat Completions 客户端。
    client: ChatCompletionsClient,
    /// 默认模型名称（如 `"deepseek-chat"`）。
    model: String,
    /// 是否启用流式传输。
    stream: bool,
    /// 最大输出令牌数。
    max_output_tokens: u64,
}

impl DeepSeekProvider {
    /// 从 LLM 配置创建 DeepSeek 提供商实例。
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let api_key = config
            .deepseek_api_key
            .clone()
            .ok_or_else(|| LlmError::config("DEEPSEEK_API_KEY is required"))?;
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build DeepSeek HTTP client: {err}"))
            })?;
        let client = ChatCompletionsClient::new(
            api_key,
            Some(config.deepseek_base_url.as_str()),
            http_client,
        );

        Ok(Self {
            client,
            model: deepseek_config_model(&config.deepseek_model)?,
            stream: config.stream,
            max_output_tokens: config.max_output_tokens,
        })
    }
}

#[async_trait]
impl LlmProvider for DeepSeekProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let effective_model = effective_deepseek_model(req.model.as_deref(), &self.model)?;
        chat_completions_with_stream_fallback(
            self.stream,
            &self.client,
            self.name(),
            &effective_model,
            self.max_output_tokens,
            &req.messages,
        )
        .await
    }

    fn name(&self) -> &'static str {
        "deepseek"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream_enabled(&self) -> bool {
        self.stream
    }
}

/// 验证并解析 DeepSeek 的配置模型名。
pub(crate) fn deepseek_config_model(value: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, "DEEPSEEK_MODEL")?;
    match model.provider {
        Some(ModelProvider::DeepSeek) | None => Ok(model.name),
        Some(ModelProvider::OpenAi) | Some(ModelProvider::BigModel) => Err(LlmError::config(
            "DEEPSEEK_MODEL must use deepseek: prefix or no prefix",
        )),
    }
}

/// 决定本次请求实际使用的 DeepSeek 模型名称。
fn effective_deepseek_model(
    override_model: Option<&str>,
    default_model: &str,
) -> Result<String, LlmError> {
    let Some(value) = override_model else {
        return Ok(default_model.to_owned());
    };
    let model = ModelId::parse(value, "request")?;
    match model.provider {
        Some(ModelProvider::DeepSeek) | None => Ok(model.name),
        Some(ModelProvider::OpenAi) | Some(ModelProvider::BigModel) => Err(LlmError::new(
            "bad_request",
            "non-deepseek-prefixed model cannot be used by DeepSeek provider",
            "request",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_deepseek_model_strips_deepseek_prefix() {
        assert_eq!(
            effective_deepseek_model(Some("deepseek:deepseek-chat"), "default").unwrap(),
            "deepseek-chat"
        );
        assert_eq!(
            effective_deepseek_model(Some("deepseek-chat"), "default").unwrap(),
            "deepseek-chat"
        );
        assert_eq!(
            effective_deepseek_model(None, "default").unwrap(),
            "default"
        );
    }

    #[test]
    fn effective_deepseek_model_rejects_openai_prefix() {
        let err = effective_deepseek_model(Some("openai:gpt-5-mini"), "default").unwrap_err();
        assert_eq!(err.code, "bad_request");
    }
}
