//! 已认证管理 API 调用者上下文。

use axum::http::{HeaderMap, header};
use uuid::Uuid;

use crate::{
    http::routes::OpsHttpState,
    management::{SECURE_SESSION_COOKIE_NAME, SESSION_COOKIE_NAME},
};

use super::ApiError;

const CSRF_HEADER: &str = "x-csrf-token";
const REQUEST_ID_HEADER: &str = "x-request-id";

/// 服务端认证后得到的通用 API 身份；领域层仍需自行判断资源权限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthenticatedApiActor {
    admin_id: i64,
    subject: String,
}

impl AuthenticatedApiActor {
    /// 不暴露原始 cookie；领域只消费认证系统签发的稳定 subject。
    pub(crate) fn subject(&self) -> &str {
        &self.subject
    }

    pub(crate) fn admin_id(&self) -> i64 {
        self.admin_id
    }
}

/// 每个管理 API Handler 复用的身份与诊断上下文。
#[derive(Debug, Clone)]
pub(crate) struct ApiRequestContext {
    pub(crate) actor: AuthenticatedApiActor,
    pub(crate) request_id: ApiRequestId,
}

impl ApiRequestContext {
    /// 所有业务管理接口均为 POST，因此统一要求管理员 Session、同源与 CSRF。
    pub(crate) fn authenticate(
        state: &OpsHttpState,
        headers: &HeaderMap,
    ) -> Result<Self, ApiError> {
        let request_id = ApiRequestId::from_headers(headers);
        authenticate_admin_request(state, headers, true)
            .map(|authenticated| Self {
                actor: authenticated.actor,
                request_id: request_id.clone(),
            })
            .map_err(|error| error.with_request_id(request_id))
    }
}

/// 配置管理与资源管理 API 共用的完整管理员认证结果。
pub(crate) struct AuthenticatedAdminRequest {
    pub(crate) auth: crate::management::AdminAuth,
    pub(crate) cookie: String,
    pub(crate) csrf: Option<String>,
    pub(crate) actor_id: i64,
    actor: AuthenticatedApiActor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiRequestId(String);

impl ApiRequestId {
    fn from_headers(headers: &HeaderMap) -> Self {
        let supplied = headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                    })
            });
        Self(
            supplied
                .map(str::to_owned)
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
        )
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn authenticate_admin_request(
    state: &OpsHttpState,
    headers: &HeaderMap,
    require_csrf: bool,
) -> Result<AuthenticatedAdminRequest, ApiError> {
    if !origin_allowed(headers) {
        return Err(ApiError::forbidden(
            "origin_denied",
            "request origin is not allowed",
        ));
    }
    let auth = state.admin_auth.clone().ok_or_else(|| {
        ApiError::unavailable(
            "auth_unavailable",
            "administrator authentication is unavailable",
        )
    })?;
    let cookie = session_cookie(headers, state.config.web_console_secure_cookies)
        .ok_or_else(|| ApiError::unauthenticated("administrator session is missing"))?;
    let csrf = csrf_token(headers);
    if require_csrf && csrf.is_none() {
        return Err(ApiError::forbidden("csrf_failed", "CSRF token is missing"));
    }
    let (admin_id, _) = auth
        .authorize_admin(
            &cookie,
            require_csrf.then_some(csrf.as_deref().unwrap_or_default()),
        )
        .map_err(ApiError::from_admin_auth)?;
    if require_csrf {
        auth.check_management_rate_limit(admin_id)
            .map_err(ApiError::from_admin_auth)?;
    }
    Ok(AuthenticatedAdminRequest {
        auth,
        cookie,
        csrf,
        actor_id: admin_id,
        actor: AuthenticatedApiActor {
            admin_id,
            subject: format!("console_admin:{admin_id}"),
        },
    })
}

pub(crate) fn session_cookie(headers: &HeaderMap, secure: bool) -> Option<String> {
    let expected = if secure {
        SECURE_SESSION_COOKIE_NAME
    } else {
        SESSION_COOKIE_NAME
    };
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .find_map(|(name, value)| (name == expected).then(|| value.to_owned()))
}

pub(crate) fn csrf_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(crate) fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    url::Url::parse(origin)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|value| (value.to_owned(), url.port_or_known_default()))
        })
        .is_some_and(|(origin_host, origin_port)| {
            let mut parts = host.rsplitn(2, ':');
            let port_or_host = parts.next().unwrap_or_default();
            let maybe_host = parts.next();
            let (host_name, host_port) = match maybe_host {
                Some(name) if port_or_host.parse::<u16>().is_ok() => {
                    (name, port_or_host.parse::<u16>().ok())
                }
                _ => (host, None),
            };
            origin_host.eq_ignore_ascii_case(host_name)
                && (host_port.is_none() || host_port == origin_port)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_accepts_safe_value_and_replaces_unsafe_value() {
        let mut headers = HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, "req-123".parse().unwrap());
        assert_eq!(ApiRequestId::from_headers(&headers).as_str(), "req-123");

        headers.insert(REQUEST_ID_HEADER, "unsafe value".parse().unwrap());
        let generated = ApiRequestId::from_headers(&headers);
        assert_ne!(generated.as_str(), "unsafe value");
        assert!(Uuid::parse_str(generated.as_str()).is_ok());
    }
}
