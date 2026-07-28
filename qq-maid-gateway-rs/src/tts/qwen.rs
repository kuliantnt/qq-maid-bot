use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use super::{TtsError, TtsFuture, TtsProvider};

#[derive(Clone)]
pub struct QwenTtsConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub voice: String,
    pub request_timeout: Duration,
    pub max_text_chars: usize,
}

impl std::fmt::Debug for QwenTtsConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QwenTtsConfig")
            .field("api_key_configured", &!self.api_key.trim().is_empty())
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("request_timeout", &self.request_timeout)
            .field("max_text_chars", &self.max_text_chars)
            .finish()
    }
}

#[derive(Clone)]
pub struct QwenTtsProvider {
    client: reqwest::Client,
    pub(super) config: QwenTtsConfig,
}

#[derive(Serialize)]
struct QwenRequest<'a> {
    model: &'a str,
    input: QwenInput<'a>,
}

#[derive(Serialize)]
struct QwenInput<'a> {
    text: &'a str,
    voice: &'a str,
}

#[derive(Deserialize)]
struct QwenResponse {
    #[serde(default)]
    status_code: Option<u16>,
    #[serde(default)]
    output: Option<QwenOutput>,
}

#[derive(Deserialize)]
struct QwenOutput {
    #[serde(default)]
    audio: Option<QwenAudio>,
}

#[derive(Deserialize)]
struct QwenAudio {
    #[serde(default)]
    url: Option<String>,
}

impl QwenTtsProvider {
    pub fn new(client: reqwest::Client, config: QwenTtsConfig) -> Self {
        Self { client, config }
    }

    async fn synthesize_inner(&self, text: &str) -> Result<String, TtsError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(TtsError::EmptyText);
        }
        if text.chars().count() > self.config.max_text_chars {
            return Err(TtsError::TextTooLong {
                max_chars: self.config.max_text_chars,
            });
        }

        // 同一个超时覆盖发送、响应头、完整 JSON body、字段提取和 URL 校验，避免
        // Provider 先返回响应头再悬挂 body 时长期占住 QQ 最终回复。
        let request = async {
            let response = self
                .client
                .post(&self.config.base_url)
                .bearer_auth(&self.config.api_key)
                .json(&QwenRequest {
                    model: &self.config.model,
                    input: QwenInput {
                        text,
                        voice: &self.config.voice,
                    },
                })
                .send()
                .await
                .map_err(TtsError::Http)?;
            let status = response.status();
            if !status.is_success() {
                // 错误体可能回显请求信息；不读取正文即可完成阶段化分类。
                return Err(TtsError::Status { status });
            }
            let response = response
                .json::<QwenResponse>()
                .await
                .map_err(|_| TtsError::InvalidResponse)?;
            if response.status_code.is_some_and(|status| status != 200) {
                return Err(TtsError::ProviderStatus);
            }
            let audio_url = response
                .output
                .and_then(|output| output.audio)
                .and_then(|audio| audio.url)
                .map(|url| url.trim().to_owned())
                .filter(|url| !url.is_empty())
                .ok_or(TtsError::InvalidResponse)?;
            validate_audio_url(&audio_url)?;
            Ok(audio_url)
        };
        timeout(self.config.request_timeout, request)
            .await
            .map_err(|_| TtsError::Timeout {
                timeout_seconds: self.config.request_timeout.as_secs(),
            })?
    }
}

impl TtsProvider for QwenTtsProvider {
    fn synthesize<'a>(&'a self, text: &'a str) -> TtsFuture<'a> {
        Box::pin(async move { self.synthesize_inner(text).await })
    }
}

fn validate_audio_url(value: &str) -> Result<(), TtsError> {
    let url = reqwest::Url::parse(value).map_err(|_| TtsError::InvalidAudioUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(TtsError::InvalidAudioUrl);
    }
    Ok(())
}
