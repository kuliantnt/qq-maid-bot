//! rig-core Chat Completions fallback。
//!
//! Responses 主链路之外的 rig-core 依赖全部集中在这里，保证 OpenAI Responses 模块
//! 不再直接接触 `rig-core`，也方便 DeepSeek 复用通用的 completion fallback helper。

use futures::StreamExt;
use rig_core::{
    client::CompletionClient,
    completion::{AssistantContent, CompletionModel, GetTokenUsage, Message, Usage},
    providers::openai,
    streaming::StreamedAssistantContent,
};

use crate::{
    error::LlmError,
    provider::{
        ChatOutcome,
        types::{ChatMessage, ChatRole, TokenUsage},
    },
    util::metrics::MetricsRecorder,
};

use super::fallback::{
    should_retry_non_stream_after_empty_stream, should_retry_non_stream_after_stream_error,
};

/// rig-core Chat Completions fallback 客户端包装。
pub(crate) struct RigChatFallbackClient {
    client: openai::CompletionsClient,
}

impl RigChatFallbackClient {
    /// 构造 rig-core OpenAI Chat Completions 客户端。
    pub(crate) fn new(
        api_key: &str,
        base_url: Option<&str>,
        http_client: reqwest::Client,
    ) -> Result<Self, LlmError> {
        let mut builder = openai::CompletionsClient::builder().api_key(api_key.to_owned());
        if let Some(base_url) = base_url.map(str::trim).filter(|value| !value.is_empty()) {
            builder = builder.base_url(base_url);
        }
        let client = builder
            .http_client(http_client)
            .build()
            .map_err(|err| LlmError::config(format!("failed to build OpenAI rig client: {err}")))?;
        Ok(Self { client })
    }
}

/// 通过 rig-core 的 Chat Completions 链路执行 OpenAI 聊天请求。
pub(crate) async fn openai_rig_chat_with_stream_fallback(
    stream: bool,
    client: &RigChatFallbackClient,
    provider: &str,
    model: &str,
    max_output_tokens: u64,
    messages: &[ChatMessage],
) -> Result<ChatOutcome, LlmError> {
    completion_with_stream_fallback(stream, provider, model, || {
        let model_client = client.client.completion_model(model.to_owned());
        let (prompt, history) = to_rig_messages(messages)?;
        Ok(model_client
            .completion_request(prompt)
            .messages(history)
            .max_tokens(max_output_tokens))
    })
    .await
}

/// 执行可选的流式补全，并在流式正文为空时自动补一次非流式请求。
///
/// 某些 OpenAI 兼容网关会返回 rig-core 暂不兼容的 SSE 片段，导致流式链路表面成功、
/// 但最终正文为空字符串。这里保留流式优先策略，同时仅在该异常形态下补一次非流式。
pub(crate) async fn completion_with_stream_fallback<M, F>(
    stream: bool,
    provider: &str,
    model: &str,
    build_request: F,
) -> Result<ChatOutcome, LlmError>
where
    M: CompletionModel + Send + Sync + 'static,
    <M as CompletionModel>::StreamingResponse: Clone + Unpin + GetTokenUsage,
    F: Fn() -> Result<rig_core::completion::CompletionRequestBuilder<M>, LlmError>,
{
    if stream {
        match stream_completion(build_request()?, provider, model).await {
            Ok(outcome) => {
                if !should_retry_non_stream_after_empty_stream(&outcome) {
                    return Ok(outcome);
                }
                tracing::warn!(
                    provider,
                    model = %model,
                    "streaming completion returned empty reply; retrying once with non-stream request"
                );
            }
            Err(err) => {
                // 某些兼容网关只在 SSE 链路上抖动，直接流式失败时先补一次同 provider
                // 的非流式请求，能避免过早切到 fallback provider。
                if !should_retry_non_stream_after_stream_error(&err) {
                    return Err(err);
                }
                tracing::warn!(
                    provider,
                    model = %model,
                    error_code = err.code.as_str(),
                    error_stage = err.stage.as_str(),
                    "streaming completion failed; retrying once with non-stream request"
                );
            }
        }
    }

    non_stream_completion(build_request()?, provider, model).await
}

/// 将内部 [`ChatMessage`] 列表转换为 rig-core 的消息格式。
///
/// 最后一条消息作为 `prompt`，其余作为 `history`。过滤掉内容为空的消息后，
/// 至少仍要保留一条非空消息，否则直接按请求错误返回。
pub(crate) fn to_rig_messages(
    messages: &[ChatMessage],
) -> Result<(Message, Vec<Message>), LlmError> {
    if messages.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "messages must not be empty",
            "request",
        ));
    }

    let mut converted = messages
        .iter()
        .filter(|message| !message.content.trim().is_empty())
        .map(to_rig_message)
        .collect::<Vec<_>>();
    let prompt = converted.pop().ok_or_else(|| {
        LlmError::new(
            "bad_request",
            "messages must contain non-empty content",
            "request",
        )
    })?;
    Ok((prompt, converted))
}

fn to_rig_message(message: &ChatMessage) -> Message {
    match message.role {
        ChatRole::System => Message::system(message.content.clone()),
        ChatRole::User => Message::user(message.content.clone()),
        ChatRole::Assistant => Message::assistant(message.content.clone()),
    }
}

/// 从 rig-core 的补全结果中提取纯文本内容，拼接多个文本段。
fn text_from_choice(choice: &rig_core::OneOrMany<AssistantContent>) -> String {
    choice
        .iter()
        .filter_map(|item| match item {
            AssistantContent::Text(text) => Some(text.text().to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// 将 rig-core 的用量数据转换为内部 [`TokenUsage`]。
fn token_usage(usage: Usage) -> Option<TokenUsage> {
    if usage.input_tokens == 0 && usage.output_tokens == 0 && usage.total_tokens == 0 {
        return None;
    }
    Some(TokenUsage {
        input_tokens: Some(usage.input_tokens),
        output_tokens: Some(usage.output_tokens),
        total_tokens: Some(usage.total_tokens),
    })
}

/// 非流式补全：一次请求等待完整回复，然后返回。
pub(crate) async fn non_stream_completion<M>(
    builder: rig_core::completion::CompletionRequestBuilder<M>,
    provider: &str,
    model: &str,
) -> Result<ChatOutcome, LlmError>
where
    M: CompletionModel + Send + Sync + 'static,
{
    let recorder = MetricsRecorder::start();
    let response = builder
        .send()
        .await
        .map_err(|err| LlmError::provider(err.to_string(), "provider"))?;
    let reply = text_from_choice(&response.choice);
    let usage = token_usage(response.usage);
    let metrics = recorder.finish(provider, model, false);

    Ok(ChatOutcome {
        reply,
        metrics,
        usage,
        fallback_used: false,
    })
}

/// 流式补全：通过 SSE 逐 token 接收回复，聚合并返回。
pub(crate) async fn stream_completion<M>(
    builder: rig_core::completion::CompletionRequestBuilder<M>,
    provider: &str,
    model: &str,
) -> Result<ChatOutcome, LlmError>
where
    M: CompletionModel + Send + Sync + 'static,
    <M as CompletionModel>::StreamingResponse: Clone + Unpin + GetTokenUsage,
{
    let mut recorder = MetricsRecorder::start();
    let mut stream = builder
        .stream()
        .await
        .map_err(|err| LlmError::provider(err.to_string(), "provider"))?;
    let mut buffer = String::new();
    let mut usage: Option<TokenUsage> = None;

    while let Some(item) = stream.next().await {
        recorder.mark_event();
        match item.map_err(|err| LlmError::provider(err.to_string(), "stream"))? {
            StreamedAssistantContent::Text(text) => {
                let delta = text.text();
                if !delta.is_empty() {
                    recorder.mark_token();
                    buffer.push_str(delta);
                }
            }
            StreamedAssistantContent::Final(response) => {
                if let Some(raw_usage) = response.token_usage() {
                    usage = token_usage(raw_usage);
                }
            }
            _ => {}
        }
    }

    if buffer.trim().is_empty() {
        buffer = text_from_choice(&stream.choice);
    }
    if usage.is_none() {
        usage = stream
            .response
            .as_ref()
            .and_then(|response| response.token_usage())
            .and_then(token_usage);
    }
    let metrics = recorder.finish(provider, model, true);

    Ok(ChatOutcome {
        reply: buffer,
        metrics,
        usage,
        fallback_used: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_rig_messages_uses_last_message_as_prompt() {
        let messages = vec![
            ChatMessage {
                role: ChatRole::System,
                content: "system".to_owned(),
            },
            ChatMessage {
                role: ChatRole::User,
                content: "hi".to_owned(),
            },
        ];
        let (_prompt, history) = to_rig_messages(&messages).unwrap();
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn empty_messages_are_rejected() {
        let err = to_rig_messages(&[]).unwrap_err();
        assert_eq!(err.code, "bad_request");
    }
}
