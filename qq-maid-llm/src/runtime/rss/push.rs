//! RSS 主动推送客户端。
//!
//! LLM 服务不直接调用 QQ OpenAPI；这里调用 gateway 提供的本地内部接口，
//! 继续由 gateway 负责平台鉴权、目标类型和 QQ payload。

use std::time::Duration;

use reqwest::StatusCode;
use serde::Serialize;
use thiserror::Error;

use crate::storage::rss::{RssTarget, RssTargetType};

#[derive(Debug, Clone)]
pub struct RssPushClient {
    client: reqwest::Client,
    endpoint: String,
    token: Option<String>,
}

#[derive(Debug, Error)]
pub enum RssPushError {
    #[error("push endpoint is not configured")]
    MissingEndpoint,
    #[error("push request failed: {0}")]
    Request(String),
    #[error("push endpoint returned {status}")]
    Status { status: StatusCode, body: String },
}

#[derive(Debug, Serialize)]
struct PushPayload<'a> {
    target_type: &'a str,
    target_id: &'a str,
    message_type: &'a str,
    text: &'a str,
    fallback_text: Option<&'a str>,
}

impl RssPushClient {
    pub fn new(
        endpoint: impl Into<String>,
        token: Option<String>,
        timeout_seconds: u64,
    ) -> Result<Self, RssPushError> {
        let endpoint = endpoint.into().trim().to_owned();
        if endpoint.is_empty() {
            return Err(RssPushError::MissingEndpoint);
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_seconds.max(1)))
            .build()
            .map_err(|err| RssPushError::Request(err.to_string()))?;
        Ok(Self {
            client,
            endpoint,
            token: token.and_then(|value| {
                let value = value.trim().to_owned();
                (!value.is_empty()).then_some(value)
            }),
        })
    }

    pub async fn send(
        &self,
        target: &RssTarget,
        message_type: &str,
        text: &str,
        fallback_text: Option<&str>,
    ) -> Result<(), RssPushError> {
        let target_type = match target.target_type {
            RssTargetType::Private => "private",
            RssTargetType::Group => "group",
        };
        let mut request = self.client.post(&self.endpoint).json(&PushPayload {
            target_type,
            target_id: &target.target_id,
            message_type,
            text,
            fallback_text,
        });
        if let Some(token) = &self.token {
            request = request.header("X-QQ-Maid-Push-Token", token);
        }
        let response = request
            .send()
            .await
            .map_err(|err| RssPushError::Request(reqwest_error_summary(&err)))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RssPushError::Status { status, body });
        }
        Ok(())
    }
}

fn reqwest_error_summary(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timeout".to_owned()
    } else if error.is_connect() {
        "connect failed".to_owned()
    } else {
        "request failed".to_owned()
    }
}
