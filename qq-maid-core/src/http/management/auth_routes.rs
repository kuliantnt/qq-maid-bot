//! 部署管理员认证相关的 HTTP 路由、Header、Cookie、CSRF 与响应转换。

use axum::{
    Json, Router,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::{convert::Infallible, net::SocketAddr};

use crate::management::{
    AdminAuthError, PREAUTH_COOKIE_NAME, SECURE_PREAUTH_COOKIE_NAME, SECURE_SESSION_COOKIE_NAME,
    SESSION_COOKIE_NAME,
};

use super::{admin_context, api_error, respond};
use crate::http::routes::OpsHttpState;

const CSRF_HEADER: &str = "x-csrf-token";
const COOKIE_MAX_AGE_SECONDS: i64 = 12 * 60 * 60;

struct OptionalPeer(Option<SocketAddr>);

impl<S> FromRequestParts<S> for OptionalPeer
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(address)| *address),
        ))
    }
}

pub(super) fn router() -> Router<OpsHttpState> {
    Router::new()
        .route("/api/v1/console/auth/bootstrap", get(auth_bootstrap))
        .route("/api/v1/console/auth/preauth", post(auth_preauth))
        .route("/api/v1/console/auth/initialize", post(auth_initialize))
        .route(
            "/api/v1/console/auth/password-reset/bootstrap",
            post(auth_request_password_reset),
        )
        .route(
            "/api/v1/console/auth/password-reset",
            post(auth_reset_password),
        )
        .route("/api/v1/console/auth/login", post(auth_login))
        .route("/api/v1/console/auth/logout", post(auth_logout))
        .route("/api/v1/console/session", get(console_session))
}

async fn auth_bootstrap(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
) -> Response {
    let Some(auth) = state.admin_auth.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    if !origin_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "request origin is not allowed",
            ),
        );
    }
    let source = client_source(&state, &headers, peer.0);
    if let Err(error) = auth.check_bootstrap_rate_limit(&source) {
        return respond(&state, &headers, auth_error(error));
    }
    let status = match auth.bootstrap_status() {
        Ok(value) => value,
        Err(error) => return respond(&state, &headers, auth_error(error)),
    };
    respond(
        &state,
        &headers,
        Json(json!({"ok": true, "bootstrap": status})).into_response(),
    )
}

async fn auth_preauth(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
) -> Response {
    let Some(auth) = state.admin_auth.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    if !preauth_request_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "pre-authentication requires a same-origin browser request",
            ),
        );
    }
    let source = client_source(&state, &headers, peer.0);
    let issued = match auth.issue_preauth_for(&source) {
        Ok(value) => value,
        Err(error) => return respond(&state, &headers, auth_error(error)),
    };
    let mut response = Json(json!({
        "ok": true,
        "csrf_token": issued.session.csrf_token,
    }))
    .into_response();
    set_preauth_cookie(
        &mut response,
        &issued.cookie_value,
        10 * 60,
        state.config.web_console_secure_cookies,
    );
    respond(&state, &headers, response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeRequest {
    username: String,
    password: String,
    bootstrap_token: String,
}

async fn auth_initialize(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
    Json(payload): Json<InitializeRequest>,
) -> Response {
    if !origin_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "request origin is not allowed",
            ),
        );
    }
    let Some(auth) = state.admin_auth.clone() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    let Some(cookie) = preauth_cookie(&headers, state.config.web_console_secure_cookies) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "pre-authentication session is missing",
            ),
        );
    };
    let Some(csrf) = csrf_token(&headers) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "csrf_failed",
                "CSRF token is missing",
            ),
        );
    };
    let source = client_source(&state, &headers, peer.0);
    let result = tokio::task::spawn_blocking(move || {
        auth.initialize_for(
            &cookie,
            &csrf,
            &payload.bootstrap_token,
            &payload.username,
            &payload.password,
            &source,
        )
    })
    .await;
    let issued = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return respond(&state, &headers, auth_error(error)),
        Err(_) => {
            return respond(
                &state,
                &headers,
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "auth_internal_error",
                    "administrator initialization task failed",
                ),
            );
        }
    };
    let mut response = Json(json!({"ok": true, "session": issued.session})).into_response();
    set_session_cookie(
        &mut response,
        &issued.cookie_value,
        COOKIE_MAX_AGE_SECONDS,
        state.config.web_console_secure_cookies,
    );
    clear_preauth_cookie(&mut response, state.config.web_console_secure_cookies);
    respond(&state, &headers, response)
}

async fn auth_request_password_reset(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "request origin is not allowed",
            ),
        );
    }
    let Some(auth) = state.admin_auth.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    let Some(cookie) = preauth_cookie(&headers, state.config.web_console_secure_cookies) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "pre-authentication session is missing",
            ),
        );
    };
    let Some(csrf) = csrf_token(&headers) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "csrf_failed",
                "CSRF token is missing",
            ),
        );
    };
    let source = client_source(&state, &headers, peer.0);
    match auth.request_password_reset_for(&cookie, &csrf, &source) {
        Ok(status) => respond(
            &state,
            &headers,
            Json(json!({"ok": true, "bootstrap": status})).into_response(),
        ),
        Err(error) => respond(&state, &headers, auth_error(error)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordResetRequest {
    password: String,
    bootstrap_token: String,
}

async fn auth_reset_password(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
    Json(payload): Json<PasswordResetRequest>,
) -> Response {
    if !origin_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "request origin is not allowed",
            ),
        );
    }
    let Some(auth) = state.admin_auth.clone() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    let Some(cookie) = preauth_cookie(&headers, state.config.web_console_secure_cookies) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "pre-authentication session is missing",
            ),
        );
    };
    let Some(csrf) = csrf_token(&headers) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "csrf_failed",
                "CSRF token is missing",
            ),
        );
    };
    let source = client_source(&state, &headers, peer.0);
    let result = tokio::task::spawn_blocking(move || {
        auth.reset_password_for(
            &cookie,
            &csrf,
            &payload.bootstrap_token,
            &payload.password,
            &source,
        )
    })
    .await;
    let issued = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return respond(&state, &headers, auth_error(error)),
        Err(_) => {
            return respond(
                &state,
                &headers,
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "auth_internal_error",
                    "administrator password reset task failed",
                ),
            );
        }
    };
    let mut response = Json(json!({"ok": true, "session": issued.session})).into_response();
    set_session_cookie(
        &mut response,
        &issued.cookie_value,
        COOKIE_MAX_AGE_SECONDS,
        state.config.web_console_secure_cookies,
    );
    clear_preauth_cookie(&mut response, state.config.web_console_secure_cookies);
    respond(&state, &headers, response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn auth_login(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> Response {
    if !origin_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "request origin is not allowed",
            ),
        );
    }
    let Some(auth) = state.admin_auth.clone() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    let Some(cookie) = preauth_cookie(&headers, state.config.web_console_secure_cookies) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "pre-authentication session is missing",
            ),
        );
    };
    let Some(csrf) = csrf_token(&headers) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "csrf_failed",
                "CSRF token is missing",
            ),
        );
    };
    let source = client_source(&state, &headers, peer.0);
    let result = tokio::task::spawn_blocking(move || {
        auth.login_for(
            &cookie,
            &csrf,
            &payload.username,
            &payload.password,
            &source,
        )
    })
    .await;
    let issued = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return respond(&state, &headers, auth_error(error)),
        Err(_) => {
            return respond(
                &state,
                &headers,
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "auth_internal_error",
                    "administrator login task failed",
                ),
            );
        }
    };
    let mut response = Json(json!({"ok": true, "session": issued.session})).into_response();
    set_session_cookie(
        &mut response,
        &issued.cookie_value,
        COOKIE_MAX_AGE_SECONDS,
        state.config.web_console_secure_cookies,
    );
    clear_preauth_cookie(&mut response, state.config.web_console_secure_cookies);
    respond(&state, &headers, response)
}

async fn console_session(State(state): State<OpsHttpState>, headers: HeaderMap) -> Response {
    let Some(auth) = state.admin_auth.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    let Some(cookie) = session_cookie(&headers, state.config.web_console_secure_cookies) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "administrator session is missing",
            ),
        );
    };
    match auth.refresh_admin_session(&cookie) {
        Ok(session) => respond(
            &state,
            &headers,
            Json(json!({"ok": true, "session": session})).into_response(),
        ),
        Err(error) => respond(&state, &headers, auth_error(error)),
    }
}

async fn auth_logout(State(state): State<OpsHttpState>, headers: HeaderMap) -> Response {
    let (auth, cookie, csrf, _) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    if let Err(error) = auth.logout(&cookie, csrf.as_deref().unwrap_or_default()) {
        return respond(&state, &headers, auth_error(error));
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    clear_session_cookie(&mut response, state.config.web_console_secure_cookies);
    respond(&state, &headers, response)
}

pub(super) fn origin_allowed(headers: &HeaderMap) -> bool {
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

fn preauth_request_allowed(headers: &HeaderMap) -> bool {
    if !origin_allowed(headers) {
        return false;
    }
    headers.contains_key(header::ORIGIN)
        || headers
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("same-origin"))
}

fn client_source(state: &OpsHttpState, headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    let peer_ip = peer.map(|address| address.ip());
    if peer_ip.is_some_and(|ip| state.config.web_console_trusted_proxy_ips.contains(&ip))
        && let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.contains(','))
            .and_then(|value| value.parse::<std::net::IpAddr>().ok())
    {
        return forwarded.to_string();
    }
    peer_ip
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .find_map(|(candidate, value)| (candidate == name).then(|| value.to_owned()))
}

pub(super) fn session_cookie(headers: &HeaderMap, secure: bool) -> Option<String> {
    cookie_value(
        headers,
        if secure {
            SECURE_SESSION_COOKIE_NAME
        } else {
            SESSION_COOKIE_NAME
        },
    )
}

fn preauth_cookie(headers: &HeaderMap, secure: bool) -> Option<String> {
    cookie_value(
        headers,
        if secure {
            SECURE_PREAUTH_COOKIE_NAME
        } else {
            PREAUTH_COOKIE_NAME
        },
    )
}

pub(super) fn csrf_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn set_cookie(response: &mut Response, name: &str, value: &str, max_age: i64, secure: bool) {
    let secure_attribute = if secure { "; Secure" } else { "" };
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure_attribute}"
    )) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn set_session_cookie(response: &mut Response, value: &str, max_age: i64, secure: bool) {
    let name = if secure {
        SECURE_SESSION_COOKIE_NAME
    } else {
        SESSION_COOKIE_NAME
    };
    set_cookie(response, name, value, max_age, secure);
}

fn set_preauth_cookie(response: &mut Response, value: &str, max_age: i64, secure: bool) {
    let name = if secure {
        SECURE_PREAUTH_COOKIE_NAME
    } else {
        PREAUTH_COOKIE_NAME
    };
    set_cookie(response, name, value, max_age, secure);
}

fn clear_cookie(response: &mut Response, name: &str, secure: bool) {
    set_cookie(response, name, "", 0, secure);
}

fn clear_session_cookie(response: &mut Response, secure: bool) {
    clear_cookie(
        response,
        if secure {
            SECURE_SESSION_COOKIE_NAME
        } else {
            SESSION_COOKIE_NAME
        },
        secure,
    );
}

fn clear_preauth_cookie(response: &mut Response, secure: bool) {
    clear_cookie(
        response,
        if secure {
            SECURE_PREAUTH_COOKIE_NAME
        } else {
            PREAUTH_COOKIE_NAME
        },
        secure,
    );
}

pub(super) fn auth_error(error: AdminAuthError) -> Response {
    let status = match error.code() {
        "unauthenticated" | "invalid_credentials" => StatusCode::UNAUTHORIZED,
        "csrf_failed" | "invalid_bootstrap_token" | "already_initialized" => StatusCode::FORBIDDEN,
        "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
        "not_initialized" => StatusCode::CONFLICT,
        "session_capacity_reached" => StatusCode::SERVICE_UNAVAILABLE,
        "validation_error" | "invalid_bootstrap_token_format" => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, error.code(), error.message())
}
