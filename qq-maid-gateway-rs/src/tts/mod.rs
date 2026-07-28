//! Gateway 出站 TTS Provider 边界。
//!
//! Core 只通过结构化 delivery hint 表达“最终正文需要语音投递”；具体 Provider
//! 协议与临时音频 URL 留在 Gateway，避免 Core 理解千问响应或签名地址。

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use qq_maid_core::config::{TtsProviderMode, VoiceFeatureConfig, VoiceFeatureStatus};
use thiserror::Error;

mod qwen;

pub use qwen::{QwenTtsConfig, QwenTtsProvider};

pub type TtsFuture<'a> = Pin<Box<dyn Future<Output = Result<String, TtsError>> + Send + 'a>>;

pub trait TtsProvider: Send + Sync {
    fn synthesize<'a>(&'a self, text: &'a str) -> TtsFuture<'a>;
}

pub type DynTtsProvider = Arc<dyn TtsProvider>;

/// 从 Core/Gateway 共用且已预检的配置构造 Provider。
///
/// 配置不可用时不尝试猜测或降级 Provider，调用方应直接沿用原始文字出站。
pub fn provider_from_config(config: &VoiceFeatureConfig) -> Option<DynTtsProvider> {
    if config.status != VoiceFeatureStatus::Available || config.provider != TtsProviderMode::Qwen {
        return None;
    }
    let api_key = config.qwen_api_key.clone()?;
    Some(Arc::new(QwenTtsProvider::new(
        qq_maid_common::http_client::client(),
        QwenTtsConfig {
            api_key,
            base_url: config.qwen_base_url.clone(),
            model: config.qwen_model.clone(),
            voice: config.qwen_voice.clone(),
            request_timeout: config.request_timeout,
            max_text_chars: config.max_text_chars,
        },
    )))
}

#[derive(Debug, Error)]
pub enum TtsError {
    #[error("TTS input is empty")]
    EmptyText,
    #[error("TTS input exceeds the configured {max_chars}-character limit")]
    TextTooLong { max_chars: usize },
    #[error("TTS request timed out after {timeout_seconds} seconds")]
    Timeout { timeout_seconds: u64 },
    #[error("TTS request failed")]
    Http(#[source] reqwest::Error),
    #[error("TTS provider returned HTTP {status}")]
    Status { status: reqwest::StatusCode },
    #[error("TTS provider returned an unsuccessful response")]
    ProviderStatus,
    #[error("TTS provider returned an invalid response")]
    InvalidResponse,
    #[error("TTS provider returned a non-HTTP(S) audio URL")]
    InvalidAudioUrl,
}

impl TtsError {
    /// 日志只需要阶段化分类；不得拼接 Provider 响应体或完整签名 URL。
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyText => "empty_text",
            Self::TextTooLong { .. } => "text_too_long",
            Self::Timeout { .. } => "timeout",
            Self::Http(_) => "http_error",
            Self::Status { .. } => "http_status",
            Self::ProviderStatus => "provider_status",
            Self::InvalidResponse => "invalid_response",
            Self::InvalidAudioUrl => "invalid_audio_url",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtsRuntimeConfig {
    pub request_timeout: Duration,
    pub max_text_chars: usize,
}

#[cfg(test)]
mod tests;
