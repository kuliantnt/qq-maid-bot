//! Connection 模型发现只读取上游声明，不验证能力，也不参与模型路由。

use crate::{config::HttpAuthConfig, error::LlmError};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

static DISCOVERIES: Semaphore = Semaphore::const_new(2);
const MAX_BYTES: usize = 1024 * 1024;

/// 显式 Adapter 能力；不根据 URL、品牌或模型 ID 推断协议。
#[derive(Clone, Copy)]
pub enum DiscoveryAdapter {
    OpenAiCompatible,
    OpenAiResponses,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryState {
    Success,
    Unsupported,
    Failed,
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub source: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ModelDiscovery {
    pub state: DiscoveryState,
    /// 仅 success 时为 Some，避免失败被误用为有效空列表。
    pub models: Option<Vec<DiscoveredModel>>,
    pub category: &'static str,
    pub http_status: Option<u16>,
    pub elapsed_ms: u64,
}

impl ModelDiscovery {
    fn outcome(state: DiscoveryState, category: &'static str) -> Self {
        Self {
            state,
            category,
            models: None,
            http_status: None,
            elapsed_ms: 0,
        }
    }
}

pub async fn discover_models(
    adapter: DiscoveryAdapter,
    base_url: &str,
    auth: &HttpAuthConfig,
    api_key: &str,
    request_timeout: Duration,
) -> Result<ModelDiscovery, LlmError> {
    if matches!(adapter, DiscoveryAdapter::Unsupported) {
        return Ok(ModelDiscovery::outcome(
            DiscoveryState::Unsupported,
            "adapter_unsupported",
        ));
    }
    let Ok(_permit) = DISCOVERIES.try_acquire() else {
        return Ok(ModelDiscovery::outcome(DiscoveryState::Unknown, "busy"));
    };
    // 沿用连接超时，但管理请求额外限制为最多 15 秒；禁止重定向携带凭证。
    let timeout = request_timeout.min(Duration::from_secs(15));
    if timeout.is_zero() {
        return Err(LlmError::config("模型发现超时必须大于零"));
    }
    let mut url =
        reqwest::Url::parse(base_url).map_err(|_| LlmError::config("模型发现 Base URL 无效"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(LlmError::config(
            "模型发现 Base URL 必须为无凭证、查询及片段的 HTTP(S) URL",
        ));
    }
    url.set_path(&format!("{}/models", url.path().trim_end_matches('/')));
    let header = reqwest::header::HeaderName::from_bytes(auth.header.as_bytes())
        .map_err(|_| LlmError::config("认证 Header 无效"))?;
    let value = match auth
        .scheme
        .as_deref()
        .filter(|scheme| !scheme.trim().is_empty())
    {
        Some(scheme) => format!("{scheme} {api_key}"),
        None => api_key.to_owned(),
    };
    let mut value = reqwest::header::HeaderValue::from_str(&value)
        .map_err(|_| LlmError::config("认证凭证格式无效"))?;
    value.set_sensitive(true);
    let client = qq_maid_common::http_client::try_builder()
        .map_err(|_| LlmError::config("无法初始化模型发现 TLS"))?
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
        .build()
        .map_err(|_| LlmError::config("无法初始化模型发现客户端"))?;
    let start = Instant::now();
    let mut result = ModelDiscovery::outcome(DiscoveryState::Failed, "unknown");
    if tokio::time::timeout(
        timeout,
        fetch_models(client.get(url).header(header, value), &mut result),
    )
    .await
    .is_err()
    {
        result.category = "timeout";
    }
    result.elapsed_ms = start.elapsed().as_millis() as u64;
    Ok(result)
}

async fn fetch_models(request: reqwest::RequestBuilder, result: &mut ModelDiscovery) {
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            result.category = if error.is_timeout() {
                "timeout"
            } else {
                "transport"
            };
            return;
        }
    };
    let status = response.status();
    result.http_status = Some(status.as_u16());
    if !status.is_success() {
        result.category = match status.as_u16() {
            404 | 405 | 501 => {
                result.state = DiscoveryState::Unsupported;
                "endpoint_unsupported"
            }
            401 | 403 => "authentication",
            429 => "rate_limit",
            500..=599 => "upstream",
            300..=399 => "redirect_rejected",
            _ => "request_rejected",
        };
        return;
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_BYTES as u64)
    {
        result.category = "response_too_large";
        return;
    }
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= MAX_BYTES => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(Some(_)) => {
                result.category = "response_too_large";
                return;
            }
            Ok(None) => break,
            Err(error) => {
                result.category = if error.is_timeout() {
                    "timeout"
                } else {
                    "response_read"
                };
                return;
            }
        }
    }
    match parse_models(&bytes) {
        Some(models) => {
            result.models = Some(models);
            result.state = DiscoveryState::Success;
            result.category = "model_list";
        }
        None => {
            result.state = DiscoveryState::Unknown;
            result.category = "unrecognized_response";
        }
    }
}

fn parse_models(bytes: &[u8]) -> Option<Vec<DiscoveredModel>> {
    #[derive(Deserialize)]
    struct List {
        data: Vec<Entry>,
        #[serde(default)]
        has_more: bool,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
    }
    let list: List = serde_json::from_slice(bytes).ok()?;
    // 未实现分页时不能将截断列表宣称为完整成功；错误条目也不能静默丢弃。
    if list.has_more
        || list.data.len() > 10000
        || list.data.iter().any(|entry| {
            entry.id.trim().is_empty()
                || entry.id.len() > 512
                || entry.id.trim() != entry.id
                || entry.id.chars().any(char::is_control)
        })
    {
        return None;
    }
    Some(
        list.data
            .into_iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|id| DiscoveredModel {
                id,
                source: "connection_discovery",
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests;
