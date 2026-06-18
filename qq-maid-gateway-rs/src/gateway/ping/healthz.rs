use std::time::Duration;

use crate::gateway::logging::mask_url;
use serde::Deserialize;

const LLM_HEALTHZ_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LlmHealthSnapshot {
    pub(super) healthz_url: String,
    pub(super) status: String,
    pub(super) upstream: LlmUpstreamSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LlmUpstreamSnapshot {
    Unavailable,
    Unverified,
    Available {
        last_success_at: Option<String>,
        provider: Option<String>,
        model: Option<String>,
        fallback_used: bool,
    },
    Error {
        last_checked_at: Option<String>,
        error_summary: String,
    },
}

#[derive(Debug, Deserialize)]
struct HealthzBody {
    #[serde(default)]
    upstream: Option<HealthzUpstream>,
}

#[derive(Debug, Deserialize)]
struct HealthzUpstream {
    state: String,
    #[serde(default)]
    last_checked_at: Option<String>,
    #[serde(default)]
    last_success_at: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    fallback_used: bool,
    #[serde(default)]
    error_summary: Option<String>,
}

pub(super) async fn probe_llm_healthz(respond_url: &str) -> LlmHealthSnapshot {
    let Ok(healthz_url) = healthz_url_from_respond_url(respond_url) else {
        return LlmHealthSnapshot {
            healthz_url: "invalid url".to_owned(),
            status: "invalid url".to_owned(),
            upstream: LlmUpstreamSnapshot::Unavailable,
        };
    };
    let healthz_url_text = mask_url(healthz_url.as_str());

    let client = match reqwest::Client::builder()
        .timeout(LLM_HEALTHZ_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            return LlmHealthSnapshot {
                healthz_url: healthz_url_text,
                status: "client build failed".to_owned(),
                upstream: LlmUpstreamSnapshot::Unavailable,
            };
        }
    };

    match client.get(healthz_url.clone()).send().await {
        Ok(response) => {
            let status = response.status();
            if !status.is_success() {
                return LlmHealthSnapshot {
                    healthz_url: healthz_url_text,
                    status: format!("http status {}", status.as_u16()),
                    upstream: LlmUpstreamSnapshot::Unavailable,
                };
            }
            let upstream = response
                .json::<HealthzBody>()
                .await
                .ok()
                .and_then(|body| body.upstream)
                .map(parse_upstream)
                .unwrap_or(LlmUpstreamSnapshot::Unverified);
            LlmHealthSnapshot {
                healthz_url: healthz_url_text,
                status: format!("ok(status={})", status.as_u16()),
                upstream,
            }
        }
        Err(error) => LlmHealthSnapshot {
            healthz_url: healthz_url_text,
            status: healthz_error_summary(&error),
            upstream: LlmUpstreamSnapshot::Unavailable,
        },
    }
}

fn parse_upstream(upstream: HealthzUpstream) -> LlmUpstreamSnapshot {
    match upstream.state.as_str() {
        "available" => LlmUpstreamSnapshot::Available {
            last_success_at: upstream.last_success_at,
            provider: upstream.provider,
            model: upstream.model,
            fallback_used: upstream.fallback_used,
        },
        "error" => LlmUpstreamSnapshot::Error {
            last_checked_at: upstream.last_checked_at,
            error_summary: safe_upstream_error_summary(upstream.error_summary.as_deref()),
        },
        _ => LlmUpstreamSnapshot::Unverified,
    }
}

/// Gateway 对 healthz 文本再做一次防御性过滤，兼容旧版本或异常本地服务。
pub(super) fn safe_upstream_error_summary(value: Option<&str>) -> String {
    let value = value.unwrap_or("上游调用失败").replace(['\r', '\n'], " ");
    let lower = value.to_ascii_lowercase();
    if [
        "authorization",
        "bearer",
        "api_key",
        "api key",
        "token",
        "secret",
        "sk-",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "上游调用失败（错误详情已隐藏）".to_owned();
    }
    let mut summary = value.chars().take(80).collect::<String>();
    if value.chars().count() > 80 {
        summary.push_str("...");
    }
    summary
}

pub(super) fn llm_health_ok(llm_health: &LlmHealthSnapshot) -> bool {
    llm_health.status.starts_with("ok(status=")
}

pub(super) fn healthz_status_detail(llm_health: &LlmHealthSnapshot) -> String {
    llm_health
        .status
        .strip_prefix("ok(status=")
        .and_then(|rest| rest.strip_suffix(')'))
        .map(|status| format!("healthz {status}"))
        .unwrap_or_else(|| format!("healthz {}", llm_health.status))
}

fn healthz_url_from_respond_url(respond_url: &str) -> Result<reqwest::Url, ()> {
    let mut url = reqwest::Url::parse(respond_url.trim()).map_err(|_| ())?;
    url.set_path("/healthz");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn healthz_error_summary(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timeout".to_owned()
    } else if error.is_connect() {
        "connect failed".to_owned()
    } else if error.is_request() {
        "request failed".to_owned()
    } else {
        "healthz failed".to_owned()
    }
}
