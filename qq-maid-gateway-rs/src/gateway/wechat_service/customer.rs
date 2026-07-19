use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use qq_maid_common::redaction::redact_sensitive_text;

use crate::{
    config::WechatServiceConfig,
    logging::{mask_url, reqwest_error_summary},
};

const WECHAT_TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60);

pub(super) fn build_customer_messenger(
    config: &WechatServiceConfig,
) -> Option<Arc<dyn WechatCustomerMessenger>> {
    let (Some(app_id), Some(app_secret)) = (config.app_id.as_ref(), config.app_secret.as_ref())
    else {
        return None;
    };
    Some(Arc::new(WechatCustomerMessageClient::new(
        qq_maid_common::http_client::client(),
        config.api_base.clone(),
        app_id.clone(),
        app_secret.clone(),
    )))
}

#[async_trait]
pub(super) trait WechatCustomerMessenger: Send + Sync {
    async fn send_text(&self, touser: &str, text: &str) -> Result<(), WechatCustomerMessageError>;
}

#[derive(Debug)]
pub(super) struct WechatCustomerMessageClient {
    client: reqwest::Client,
    api_base: String,
    app_id: String,
    app_secret: String,
    token: Mutex<Option<CachedWechatAccessToken>>,
}

#[derive(Debug, Clone)]
struct CachedWechatAccessToken {
    token: String,
    expires_at: Instant,
}

#[derive(Debug, Error)]
pub(super) enum WechatCustomerMessageError {
    #[error("WeChat API request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("WeChat API returned {status}")]
    Status {
        status: StatusCode,
        body_summary: String,
    },
    #[error("WeChat API returned errcode={errcode}: {errmsg}")]
    Api { errcode: i64, errmsg: String },
    #[error("WeChat token response missing access_token")]
    MissingAccessToken,
    #[error("invalid WeChat API url: {0}")]
    InvalidUrl(String),
}

impl WechatCustomerMessageError {
    pub(super) fn log_summary(&self) -> String {
        match self {
            Self::Http(error) => reqwest_error_summary(error),
            Self::Status {
                status,
                body_summary,
            } => {
                if body_summary.is_empty() {
                    format!("http status {status}")
                } else {
                    format!("http status {status}: {body_summary}")
                }
            }
            Self::Api { errcode, errmsg } => format!("errcode={errcode}: {errmsg}"),
            Self::MissingAccessToken => "missing access_token".to_owned(),
            Self::InvalidUrl(_) => "invalid api url".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WechatAccessTokenResponse {
    access_token: Option<String>,
    expires_in: Option<u64>,
    errcode: Option<i64>,
    errmsg: Option<String>,
}

#[derive(Debug, Serialize)]
struct WechatCustomerTextPayload<'a> {
    touser: &'a str,
    msgtype: &'static str,
    text: WechatCustomerTextContent<'a>,
}

#[derive(Debug, Serialize)]
struct WechatCustomerTextContent<'a> {
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct WechatApiStatusResponse {
    errcode: Option<i64>,
    #[serde(default)]
    errmsg: String,
}

impl WechatCustomerMessageClient {
    pub(super) fn new(
        client: reqwest::Client,
        api_base: String,
        app_id: String,
        app_secret: String,
    ) -> Self {
        Self {
            client,
            api_base: api_base.trim_end_matches('/').to_owned(),
            app_id,
            app_secret,
            token: Mutex::new(None),
        }
    }

    async fn access_token(&self) -> Result<String, WechatCustomerMessageError> {
        if let Some(token) = self.cached_access_token() {
            return Ok(token);
        }

        let mut url = self.url("/cgi-bin/token")?;
        url.query_pairs_mut()
            .append_pair("grant_type", "client_credential")
            .append_pair("appid", &self.app_id)
            .append_pair("secret", &self.app_secret);
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(WechatCustomerMessageError::Http)?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(WechatCustomerMessageError::Http)?;
        if !status.is_success() {
            return Err(WechatCustomerMessageError::Status {
                status,
                body_summary: wechat_api_body_summary(&body),
            });
        }
        let token_response: WechatAccessTokenResponse =
            serde_json::from_str(&body).map_err(|err| WechatCustomerMessageError::Api {
                errcode: -1,
                errmsg: format!("invalid token json: {err}"),
            })?;
        if let Some(errcode) = token_response.errcode
            && errcode != 0
        {
            return Err(WechatCustomerMessageError::Api {
                errcode,
                errmsg: token_response.errmsg.unwrap_or_default(),
            });
        }
        let access_token = token_response
            .access_token
            .filter(|token| !token.trim().is_empty())
            .ok_or(WechatCustomerMessageError::MissingAccessToken)?;
        let expires_in = token_response.expires_in.unwrap_or(7200);
        let expires_at = Instant::now()
            + Duration::from_secs(expires_in).saturating_sub(WECHAT_TOKEN_REFRESH_MARGIN);
        *self
            .token
            .lock()
            .expect("wechat token lock should not be poisoned") = Some(CachedWechatAccessToken {
            token: access_token.clone(),
            expires_at,
        });
        Ok(access_token)
    }

    fn cached_access_token(&self) -> Option<String> {
        let token = self
            .token
            .lock()
            .expect("wechat token lock should not be poisoned")
            .clone()?;
        (Instant::now() < token.expires_at).then_some(token.token)
    }

    fn clear_cached_access_token(&self, stale_token: &str) {
        let mut token = self
            .token
            .lock()
            .expect("wechat token lock should not be poisoned");
        if token
            .as_ref()
            .is_some_and(|cached| cached.token == stale_token)
        {
            *token = None;
        }
    }

    fn url(&self, path: &str) -> Result<reqwest::Url, WechatCustomerMessageError> {
        reqwest::Url::parse(&format!("{}{}", self.api_base, path))
            .map_err(|err| WechatCustomerMessageError::InvalidUrl(err.to_string()))
    }

    async fn send_text_with_access_token(
        &self,
        access_token: &str,
        touser: &str,
        text: &str,
    ) -> Result<(), WechatCustomerMessageError> {
        let mut url = self.url("/cgi-bin/message/custom/send")?;
        url.query_pairs_mut()
            .append_pair("access_token", access_token);
        let payload = WechatCustomerTextPayload {
            touser,
            msgtype: "text",
            text: WechatCustomerTextContent { content: text },
        };
        let response = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .map_err(WechatCustomerMessageError::Http)?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(WechatCustomerMessageError::Http)?;
        if !status.is_success() {
            return Err(WechatCustomerMessageError::Status {
                status,
                body_summary: wechat_api_body_summary(&body),
            });
        }
        parse_wechat_api_status(&body)
    }
}

#[async_trait]
impl WechatCustomerMessenger for WechatCustomerMessageClient {
    async fn send_text(&self, touser: &str, text: &str) -> Result<(), WechatCustomerMessageError> {
        let access_token = self.access_token().await?;
        let result = self
            .send_text_with_access_token(&access_token, touser, text)
            .await;
        if let Err(WechatCustomerMessageError::Api { errcode, .. }) = &result
            && is_wechat_access_token_invalid_errcode(*errcode)
        {
            self.clear_cached_access_token(&access_token);
            let refreshed_access_token = self.access_token().await?;
            // 微信可能提前作废 access_token；刷新后只重试一次，避免真实业务错误被无限放大。
            return self
                .send_text_with_access_token(&refreshed_access_token, touser, text)
                .await;
        }
        result
    }
}

pub(super) fn is_wechat_access_token_invalid_errcode(errcode: i64) -> bool {
    matches!(errcode, 40001 | 40014 | 42001)
}

pub(super) fn parse_wechat_api_status(body: &str) -> Result<(), WechatCustomerMessageError> {
    let status_response: WechatApiStatusResponse =
        serde_json::from_str(body).map_err(|err| WechatCustomerMessageError::Api {
            errcode: -1,
            errmsg: format!("invalid status json: {err}"),
        })?;
    // 客服消息发送成功必须以微信明确返回 errcode=0 为准，避免代理空响应被误判成功。
    let errcode = status_response
        .errcode
        .ok_or_else(|| WechatCustomerMessageError::Api {
            errcode: -1,
            errmsg: "missing errcode in status response".to_owned(),
        })?;
    if errcode != 0 {
        return Err(WechatCustomerMessageError::Api {
            errcode,
            errmsg: status_response.errmsg,
        });
    }
    Ok(())
}

pub(super) fn wechat_api_body_summary(body: &str) -> String {
    const MAX_CHARS: usize = 200;
    let redacted = redact_urls_in_text(&redact_sensitive_text(body));
    let mut summary = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    if summary.chars().count() > MAX_CHARS {
        summary = summary.chars().take(MAX_CHARS).collect::<String>();
        summary.push('…');
    }
    summary
}

fn redact_urls_in_text(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            if token.contains("://") {
                mask_url(token)
            } else {
                token.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
