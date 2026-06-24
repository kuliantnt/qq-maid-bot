//! DeepSeek (Rig) 提供商实现。
//!
//! 基于 `rig-core` 的 DeepSeek 客户端封装，复用 `provider/openai`
//! 顶层导出的通用补全函数（`completion_with_stream_fallback`、`to_rig_messages`）。

use std::time::Duration;

use async_trait::async_trait;
use rig_core::{client::CompletionClient, completion::CompletionModel, providers::deepseek};

use crate::{
    config::AppConfig,
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider,
        openai::{completion_with_stream_fallback, to_rig_messages},
        types::{ChatRequest, ModelId, ModelProvider},
    },
};

/// 基于 rig-core 的 DeepSeek 提供商实现。
pub struct DeepSeekRigProvider {
    /// rig-core DeepSeek HTTP 客户端。
    client: deepseek::Client,
    /// 默认模型名称（如 `"deepseek-chat"`）。
    model: String,
    /// 是否启用流式传输。
    stream: bool,
    /// 最大输出令牌数。
    max_output_tokens: u64,
}

impl DeepSeekRigProvider {
    /// 从应用配置创建 DeepSeek 提供商实例。
    ///
    /// 需要配置 `deepseek_api_key` 和 `deepseek_base_url`。
    pub fn new(config: &AppConfig) -> Result<Self, LlmError> {
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
        let client = deepseek::Client::builder()
            .api_key(api_key)
            .base_url(&config.deepseek_base_url)
            .http_client(http_client)
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build DeepSeek rig client: {err}"))
            })?;

        Ok(Self {
            client,
            model: deepseek_config_model(&config.deepseek_model)?,
            stream: config.stream,
            max_output_tokens: config.max_output_tokens,
        })
    }
}

#[async_trait]
impl LlmProvider for DeepSeekRigProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let effective_model = effective_deepseek_model(req.model.as_deref(), &self.model)?;
        completion_with_stream_fallback(self.stream, self.name(), &effective_model, || {
            let model = self.client.completion_model(effective_model.clone());
            let (prompt, history) = to_rig_messages(&req.messages)?;
            Ok(model
                .completion_request(prompt)
                .messages(history)
                .max_tokens(self.max_output_tokens))
        })
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
///
/// 只允许 `deepseek:` 前缀或无前缀；若为 `openai:` 前缀则返回配置错误。
pub(crate) fn deepseek_config_model(value: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, "DEEPSEEK_MODEL")?;
    match model.provider {
        Some(ModelProvider::DeepSeek) | None => Ok(model.name),
        Some(ModelProvider::OpenAi) => Err(LlmError::config(
            "DEEPSEEK_MODEL must use deepseek: prefix or no prefix",
        )),
    }
}

/// 决定本次请求实际使用的 DeepSeek 模型名称。
///
/// 如果请求中指定了模型，则去掉 `deepseek:` 前缀后返回；
/// 若指定了 `openai:` 前缀则拒绝；无指定时返回默认模型。
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
        Some(ModelProvider::OpenAi) => Err(LlmError::new(
            "bad_request",
            "openai-prefixed model cannot be used by DeepSeek provider",
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
