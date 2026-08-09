//! 最终回复语音投递的共享功能配置。
//!
//! Core 用预检状态决定 `/语音 开启` 是否允许写入，Gateway 使用同一快照构造具体
//! TTS Provider。无效 TTS 配置只关闭语音能力，避免破坏既有文本机器人启动。

use std::{collections::HashMap, time::Duration};

pub const DEFAULT_QWEN_TTS_BASE_URL: &str =
    "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation";
pub const DEFAULT_QWEN_TTS_MODEL: &str = "qwen3-tts-flash";
pub const DEFAULT_QWEN_TTS_VOICE: &str = "Cherry";
pub const DEFAULT_TTS_REQUEST_TIMEOUT_SECONDS: u64 = 30;
pub const DEFAULT_TTS_MAX_TEXT_CHARS: usize = 600;
const MAX_TTS_REQUEST_TIMEOUT_SECONDS: u64 = 120;
const MAX_TTS_TEXT_CHARS: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsProviderMode {
    Disabled,
    Qwen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceFeatureStatus {
    Disabled,
    Available,
    Invalid(VoicePreflightError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoicePreflightError {
    UnsupportedProvider,
    MissingApiKey,
    InvalidBaseUrl,
    InvalidRequestTimeout,
    InvalidTextLimit,
}

#[derive(Clone, PartialEq, Eq)]
pub struct VoiceFeatureConfig {
    pub provider: TtsProviderMode,
    pub status: VoiceFeatureStatus,
    pub qwen_api_key: Option<String>,
    pub qwen_base_url: String,
    pub qwen_model: String,
    pub qwen_voice: String,
    pub request_timeout: Duration,
    pub max_text_chars: usize,
}

impl std::fmt::Debug for VoiceFeatureConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VoiceFeatureConfig")
            .field("provider", &self.provider)
            .field("status", &self.status)
            .field("qwen_api_key_configured", &self.qwen_api_key.is_some())
            .field("qwen_base_url", &self.qwen_base_url)
            .field("qwen_model", &self.qwen_model)
            .field("qwen_voice", &self.qwen_voice)
            .field("request_timeout", &self.request_timeout)
            .field("max_text_chars", &self.max_text_chars)
            .finish()
    }
}

impl Default for VoiceFeatureConfig {
    fn default() -> Self {
        Self {
            provider: TtsProviderMode::Disabled,
            status: VoiceFeatureStatus::Disabled,
            qwen_api_key: None,
            qwen_base_url: DEFAULT_QWEN_TTS_BASE_URL.to_owned(),
            qwen_model: DEFAULT_QWEN_TTS_MODEL.to_owned(),
            qwen_voice: DEFAULT_QWEN_TTS_VOICE.to_owned(),
            request_timeout: Duration::from_secs(DEFAULT_TTS_REQUEST_TIMEOUT_SECONDS),
            max_text_chars: DEFAULT_TTS_MAX_TEXT_CHARS,
        }
    }
}

impl VoiceFeatureConfig {
    pub fn from_environment(environment: &HashMap<String, String>) -> Self {
        let mut config = Self::default();
        let provider = optional(environment, "TTS_PROVIDER")
            .unwrap_or_else(|| "disabled".to_owned())
            .to_ascii_lowercase();
        config.provider = match provider.as_str() {
            "disabled" => return config,
            "qwen" => TtsProviderMode::Qwen,
            _ => {
                config.status =
                    VoiceFeatureStatus::Invalid(VoicePreflightError::UnsupportedProvider);
                return config;
            }
        };
        config.qwen_api_key = optional(environment, "QWEN_TTS_API_KEY");
        config.qwen_base_url = optional(environment, "QWEN_TTS_BASE_URL")
            .unwrap_or_else(|| DEFAULT_QWEN_TTS_BASE_URL.to_owned());
        config.qwen_model = optional(environment, "QWEN_TTS_MODEL")
            .unwrap_or_else(|| DEFAULT_QWEN_TTS_MODEL.to_owned());
        config.qwen_voice = optional(environment, "QWEN_TTS_VOICE")
            .unwrap_or_else(|| DEFAULT_QWEN_TTS_VOICE.to_owned());

        let Some(timeout_seconds) = parse_bounded_u64(
            environment,
            "TTS_REQUEST_TIMEOUT_SECONDS",
            DEFAULT_TTS_REQUEST_TIMEOUT_SECONDS,
            1,
            MAX_TTS_REQUEST_TIMEOUT_SECONDS,
        ) else {
            config.status = VoiceFeatureStatus::Invalid(VoicePreflightError::InvalidRequestTimeout);
            return config;
        };
        config.request_timeout = Duration::from_secs(timeout_seconds);
        let Some(max_text_chars) = parse_bounded_usize(
            environment,
            "TTS_MAX_TEXT_CHARS",
            DEFAULT_TTS_MAX_TEXT_CHARS,
            1,
            MAX_TTS_TEXT_CHARS,
        ) else {
            config.status = VoiceFeatureStatus::Invalid(VoicePreflightError::InvalidTextLimit);
            return config;
        };
        config.max_text_chars = max_text_chars;

        if config.qwen_api_key.is_none() {
            config.status = VoiceFeatureStatus::Invalid(VoicePreflightError::MissingApiKey);
            return config;
        }
        let base_url_valid = reqwest::Url::parse(&config.qwen_base_url).is_ok_and(|url| {
            url.scheme() == "https" && url.host_str().is_some() && url.username().is_empty()
        });
        if !base_url_valid {
            config.status = VoiceFeatureStatus::Invalid(VoicePreflightError::InvalidBaseUrl);
            return config;
        }
        config.status = VoiceFeatureStatus::Available;
        config
    }

    pub fn is_available(&self) -> bool {
        self.status == VoiceFeatureStatus::Available
    }

    pub fn enable_rejection_text(&self) -> Option<&'static str> {
        match self.status {
            VoiceFeatureStatus::Available => None,
            VoiceFeatureStatus::Disabled => Some("语音功能当前未启用，请先配置 TTS_PROVIDER=qwen"),
            VoiceFeatureStatus::Invalid(VoicePreflightError::MissingApiKey) => {
                Some("语音功能不可用：缺少 QWEN_TTS_API_KEY")
            }
            VoiceFeatureStatus::Invalid(_) => {
                Some("语音功能配置预检失败，请联系管理员检查 TTS 配置")
            }
        }
    }
}

fn optional(environment: &HashMap<String, String>, name: &str) -> Option<String> {
    environment
        .get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn parse_bounded_u64(
    environment: &HashMap<String, String>,
    name: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Option<u64> {
    let value = match optional(environment, name) {
        Some(raw) => raw.parse().ok()?,
        None => default,
    };
    (min..=max).contains(&value).then_some(value)
}

fn parse_bounded_usize(
    environment: &HashMap<String, String>,
    name: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Option<usize> {
    let value = match optional(environment, name) {
        Some(raw) => raw.parse().ok()?,
        None => default,
    };
    (min..=max).contains(&value).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_and_invalid_qwen_configs_are_unavailable_without_exposing_secret() {
        let disabled = VoiceFeatureConfig::from_environment(&HashMap::new());
        assert_eq!(disabled.status, VoiceFeatureStatus::Disabled);

        let missing_key = VoiceFeatureConfig::from_environment(&HashMap::from([(
            "TTS_PROVIDER".to_owned(),
            "qwen".to_owned(),
        )]));
        assert_eq!(
            missing_key.status,
            VoiceFeatureStatus::Invalid(VoicePreflightError::MissingApiKey)
        );

        let invalid_url = VoiceFeatureConfig::from_environment(&HashMap::from([
            ("TTS_PROVIDER".to_owned(), "qwen".to_owned()),
            ("QWEN_TTS_API_KEY".to_owned(), "secret-key".to_owned()),
            (
                "QWEN_TTS_BASE_URL".to_owned(),
                "http://localhost/tts".to_owned(),
            ),
        ]));
        assert_eq!(
            invalid_url.status,
            VoiceFeatureStatus::Invalid(VoicePreflightError::InvalidBaseUrl)
        );
        assert!(!format!("{invalid_url:?}").contains("secret-key"));
    }

    #[test]
    fn valid_qwen_config_is_available_with_expected_defaults() {
        let config = VoiceFeatureConfig::from_environment(&HashMap::from([
            ("TTS_PROVIDER".to_owned(), "qwen".to_owned()),
            ("QWEN_TTS_API_KEY".to_owned(), "secret-key".to_owned()),
        ]));

        assert!(config.is_available());
        assert_eq!(config.qwen_model, DEFAULT_QWEN_TTS_MODEL);
        assert_eq!(config.qwen_voice, DEFAULT_QWEN_TTS_VOICE);
        assert_eq!(config.max_text_chars, DEFAULT_TTS_MAX_TEXT_CHARS);
    }
}
