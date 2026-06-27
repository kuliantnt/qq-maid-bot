//! 智谱 BigModel 提供商实现。
//!
//! BigModel 官方通用端点使用 HTTP Bearer 鉴权，并提供 OpenAI 兼容的
//! `/chat/completions` 接口；这里复用已有 Chat Completions adapter，
//! 只维护 BigModel 自身的配置项和模型前缀规则。

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

/// 智谱 BigModel 提供商实现。
pub struct BigModelProvider {
    /// OpenAI 兼容 Chat Completions 客户端。
    client: ChatCompletionsClient,
    /// 默认模型名称（如 `"glm-5.2"`）。
    model: String,
    /// 是否启用流式传输。
    stream: bool,
    /// 最大输出令牌数。
    max_output_tokens: u64,
}

impl BigModelProvider {
    /// 从 LLM 配置创建 BigModel 提供商实例。
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let api_key = config
            .bigmodel_api_key
            .clone()
            .ok_or_else(|| LlmError::config("BIGMODEL_API_KEY is required"))?;
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build BigModel HTTP client: {err}"))
            })?;
        let client = ChatCompletionsClient::new(
            api_key,
            Some(config.bigmodel_base_url.as_str()),
            http_client,
        );

        Ok(Self {
            client,
            model: bigmodel_config_model(&config.bigmodel_model)?,
            stream: config.stream,
            max_output_tokens: config.max_output_tokens,
        })
    }
}

#[async_trait]
impl LlmProvider for BigModelProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let effective_model = effective_bigmodel_model(req.model.as_deref(), &self.model)?;
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
        "bigmodel"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream_enabled(&self) -> bool {
        self.stream
    }
}

/// 验证并解析 BigModel 的配置模型名。
pub(crate) fn bigmodel_config_model(value: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, "BIGMODEL_MODEL")?;
    match model.provider {
        Some(ModelProvider::BigModel) | None => Ok(model.name),
        Some(ModelProvider::OpenAi) | Some(ModelProvider::DeepSeek) => Err(LlmError::config(
            "BIGMODEL_MODEL must use bigmodel: prefix or no prefix",
        )),
    }
}

/// 决定本次请求实际使用的 BigModel 模型名称。
fn effective_bigmodel_model(
    override_model: Option<&str>,
    default_model: &str,
) -> Result<String, LlmError> {
    let Some(value) = override_model else {
        return Ok(default_model.to_owned());
    };
    let model = ModelId::parse(value, "request")?;
    match model.provider {
        Some(ModelProvider::BigModel) | None => Ok(model.name),
        Some(ModelProvider::OpenAi) | Some(ModelProvider::DeepSeek) => Err(LlmError::new(
            "bad_request",
            "non-bigmodel-prefixed model cannot be used by BigModel provider",
            "request",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_bigmodel_model_strips_bigmodel_prefix() {
        assert_eq!(
            effective_bigmodel_model(Some("bigmodel:glm-5.2"), "default").unwrap(),
            "glm-5.2"
        );
        assert_eq!(
            effective_bigmodel_model(Some("zhipu:glm-4-flash"), "default").unwrap(),
            "glm-4-flash"
        );
        assert_eq!(
            effective_bigmodel_model(Some("glm-5.2"), "default").unwrap(),
            "glm-5.2"
        );
        assert_eq!(
            effective_bigmodel_model(None, "default").unwrap(),
            "default"
        );
    }

    #[test]
    fn effective_bigmodel_model_rejects_other_provider_prefix() {
        let err = effective_bigmodel_model(Some("openai:gpt-5-mini"), "default").unwrap_err();
        assert_eq!(err.code, "bad_request");

        let err = bigmodel_config_model("deepseek:deepseek-chat").unwrap_err();
        assert_eq!(err.code, "config");
    }
}
