use std::{sync::Arc, time::Duration};

use reqwest::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use tokio::{sync::Mutex, time::Instant};

pub const ACCESS_TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

#[derive(Debug, Clone)]
pub struct AccessTokenManager {
    inner: Arc<AccessTokenManagerInner>,
}

#[derive(Debug)]
struct AccessTokenManagerInner {
    client: reqwest::Client,
    app_id: String,
    app_secret: String,
    refresh_margin: Duration,
    cached: Mutex<Option<CachedAccessToken>>,
}

#[derive(Debug, Clone)]
pub struct CachedAccessToken {
    pub token: String,
    pub expires_at: Instant,
    pub refresh_margin: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessTokenSnapshot {
    pub state: AccessTokenSnapshotState,
    pub expires_in_seconds: Option<u64>,
    pub refresh_margin_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessTokenSnapshotState {
    Empty,
    Cached,
    RefreshDue,
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("QQ token request failed: {0}")]
    Request(reqwest::Error),
    #[error("QQ token endpoint returned {status}")]
    Status { status: StatusCode },
    #[error("QQ token response missing access_token or expires_in")]
    InvalidResponse,
}

impl AuthError {
    /// Token 刷新只重试远端瞬时故障；鉴权 4xx 与成功但无效的认证响应必须尽快暴露。
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Request(error) => !error.is_builder() && !error.is_redirect(),
            Self::Status { status } => is_retryable_status(*status),
            Self::InvalidResponse => false,
        }
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429) || status.is_server_error()
}

#[derive(Debug, Serialize)]
struct TokenRequest<'a> {
    #[serde(rename = "appId")]
    app_id: &'a str,
    #[serde(rename = "clientSecret")]
    client_secret: &'a str,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    expires_in: Option<u64>,
}

impl AccessTokenManager {
    pub fn new(
        client: reqwest::Client,
        app_id: impl Into<String>,
        app_secret: impl Into<String>,
        refresh_margin: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(AccessTokenManagerInner {
                client,
                app_id: app_id.into(),
                app_secret: app_secret.into(),
                refresh_margin,
                cached: Mutex::new(None),
            }),
        }
    }

    pub async fn token(&self) -> Result<String, AuthError> {
        let now = Instant::now();
        let mut cached = self.inner.cached.lock().await;
        if let Some(token) = cached.as_ref()
            && !token.needs_refresh(now)
        {
            return Ok(token.token.clone());
        }

        let token = self.fetch_token(now).await?;
        let value = token.token.clone();
        *cached = Some(token);
        Ok(value)
    }

    pub async fn authorization_header(&self) -> Result<String, AuthError> {
        Ok(format!("QQBot {}", self.token().await?))
    }

    pub async fn snapshot(&self) -> AccessTokenSnapshot {
        let now = Instant::now();
        let cached = self.inner.cached.lock().await;
        let refresh_margin_seconds = self.inner.refresh_margin.as_secs();
        let Some(token) = cached.as_ref() else {
            return AccessTokenSnapshot {
                state: AccessTokenSnapshotState::Empty,
                expires_in_seconds: None,
                refresh_margin_seconds,
            };
        };

        let expires_in_seconds = Some(
            token
                .expires_at
                .checked_duration_since(now)
                .unwrap_or(Duration::ZERO)
                .as_secs(),
        );
        let state = if token.needs_refresh(now) {
            AccessTokenSnapshotState::RefreshDue
        } else {
            AccessTokenSnapshotState::Cached
        };
        AccessTokenSnapshot {
            state,
            expires_in_seconds,
            refresh_margin_seconds,
        }
    }

    async fn fetch_token(&self, now: Instant) -> Result<CachedAccessToken, AuthError> {
        let response = self
            .inner
            .client
            .post(ACCESS_TOKEN_URL)
            .json(&TokenRequest {
                app_id: &self.inner.app_id,
                client_secret: &self.inner.app_secret,
            })
            .send()
            .await
            .map_err(AuthError::Request)?;

        let status = response.status();
        if !status.is_success() {
            // Token 错误正文可能包含平台诊断或认证相关内容，分类只需要状态码，不保留正文。
            return Err(AuthError::Status { status });
        }

        let token = response.json::<TokenResponse>().await.map_err(|error| {
            if error.is_decode() {
                AuthError::InvalidResponse
            } else {
                // 已收到 2xx 响应头后，响应体仍可能因连接重置或超时读取失败。
                AuthError::Request(error)
            }
        })?;
        let access_token = token.access_token.filter(|value| !value.trim().is_empty());
        let expires_in = token.expires_in.filter(|value| *value > 0);
        let (Some(access_token), Some(expires_in)) = (access_token, expires_in) else {
            return Err(AuthError::InvalidResponse);
        };

        Ok(CachedAccessToken {
            token: access_token,
            expires_at: now + Duration::from_secs(expires_in),
            refresh_margin: self.inner.refresh_margin,
        })
    }
}

impl CachedAccessToken {
    pub fn needs_refresh(&self, now: Instant) -> bool {
        match now.checked_add(self.refresh_margin) {
            Some(refresh_deadline) => refresh_deadline >= self.expires_at,
            None => true,
        }
    }
}

fn deserialize_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };

    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(number) => Ok(number.as_u64()),
        serde_json::Value::String(raw) => raw
            .parse::<u64>()
            .map(Some)
            .map_err(serde::de::Error::custom),
        _ => Err(serde::de::Error::custom(
            "expected integer or string integer",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合并 2 个 needs_refresh 边界测试为表驱动测试。
    #[test]
    fn cached_token_needs_refresh_boundary() {
        struct Case {
            name: &'static str,
            expires_in_seconds: u64,
            expected_needs_refresh: bool,
        }

        let cases = [
            Case {
                name: "cached_token_refreshes_inside_margin",
                expires_in_seconds: 30,
                expected_needs_refresh: true,
            },
            Case {
                name: "cached_token_is_reused_before_margin",
                expires_in_seconds: 120,
                expected_needs_refresh: false,
            },
        ];

        for case in &cases {
            let now = Instant::now();
            let token = CachedAccessToken {
                token: "token".to_owned(),
                expires_at: now + Duration::from_secs(case.expires_in_seconds),
                refresh_margin: Duration::from_secs(60),
            };
            assert_eq!(
                token.needs_refresh(now),
                case.expected_needs_refresh,
                "case '{}' failed",
                case.name
            );
        }
    }

    #[test]
    fn token_response_accepts_string_expires_in() {
        let response = serde_json::from_str::<TokenResponse>(
            r#"{"access_token":"token","expires_in":"7200"}"#,
        )
        .unwrap();

        assert_eq!(response.expires_in, Some(7200));
    }

    /// 公共测试 helper：创建 AccessTokenManager 实例。
    fn test_manager() -> AccessTokenManager {
        AccessTokenManager::new(
            reqwest::Client::new(),
            "appid",
            "app-secret",
            Duration::from_secs(60),
        )
    }

    #[tokio::test]
    async fn token_snapshot_reports_empty_cache_without_secret() {
        let manager = test_manager();

        let snapshot = manager.snapshot().await;
        let rendered = format!("{snapshot:?}");

        assert_eq!(snapshot.state, AccessTokenSnapshotState::Empty);
        assert_eq!(snapshot.expires_in_seconds, None);
        assert_eq!(snapshot.refresh_margin_seconds, 60);
        assert!(!rendered.contains("app-secret"));
        assert!(!rendered.contains("QQBot"));
    }

    #[tokio::test]
    async fn token_snapshot_reports_cached_token_without_value() {
        let manager = test_manager();
        *manager.inner.cached.lock().await = Some(CachedAccessToken {
            token: "real-access-token".to_owned(),
            expires_at: Instant::now() + Duration::from_secs(120),
            refresh_margin: Duration::from_secs(60),
        });

        let snapshot = manager.snapshot().await;
        let rendered = format!("{snapshot:?}");

        assert_eq!(snapshot.state, AccessTokenSnapshotState::Cached);
        assert!(snapshot.expires_in_seconds.unwrap_or_default() <= 120);
        assert!(!rendered.contains("real-access-token"));
        assert!(!rendered.contains("app-secret"));
    }

    #[tokio::test]
    async fn token_snapshot_reports_refresh_due() {
        let manager = test_manager();
        *manager.inner.cached.lock().await = Some(CachedAccessToken {
            token: "real-access-token".to_owned(),
            expires_at: Instant::now() + Duration::from_secs(30),
            refresh_margin: Duration::from_secs(60),
        });

        let snapshot = manager.snapshot().await;

        assert_eq!(snapshot.state, AccessTokenSnapshotState::RefreshDue);
        assert!(snapshot.expires_in_seconds.unwrap_or_default() <= 30);
    }

    #[test]
    fn token_status_retry_classification_is_explicit() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_EARLY,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
        ] {
            assert!(AuthError::Status { status }.is_retryable(), "{status}");
        }

        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            assert!(!AuthError::Status { status }.is_retryable(), "{status}");
        }
        assert!(!AuthError::InvalidResponse.is_retryable());
    }

    #[tokio::test]
    async fn token_network_error_is_retryable_and_safe_to_format() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let request_error = reqwest::Client::new()
            .get(format!("http://{address}/token"))
            .send()
            .await
            .unwrap_err();
        let error = AuthError::Request(request_error);
        let rendered = error.to_string();

        assert!(error.is_retryable());
        assert!(!rendered.contains("app-secret"));
        assert!(!rendered.contains("QQBot"));
    }
}
