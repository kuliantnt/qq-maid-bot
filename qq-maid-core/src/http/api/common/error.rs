//! 管理 API 的统一 HTTP 错误。

use axum::{
    Json,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::management::AdminAuthError;

use super::auth::ApiRequestId;

/// API 层错误只负责 HTTP 状态和公开错误文案，不反向进入领域层。
#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: String,
    message: String,
    request_id: Option<ApiRequestId>,
}

#[derive(Serialize)]
struct ApiErrorEnvelope<'a> {
    ok: bool,
    error: ApiErrorBody<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<&'a str>,
}

#[derive(Serialize)]
struct ApiErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

impl ApiError {
    pub(crate) fn code(&self) -> &str {
        &self.code
    }

    pub(crate) fn new(
        status: StatusCode,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            request_id: None,
        }
    }

    pub(crate) fn invalid_json(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_json", message)
    }

    pub(crate) fn validation(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_error",
            message,
        )
    }

    pub(crate) fn unauthenticated(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthenticated", message)
    }

    pub(crate) fn forbidden(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
    }

    pub(crate) fn unavailable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message)
    }

    pub(crate) fn with_request_id(mut self, request_id: ApiRequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    pub(crate) fn from_admin_auth(error: AdminAuthError) -> Self {
        let status = match error.code() {
            "unauthenticated" | "invalid_credentials" => StatusCode::UNAUTHORIZED,
            "csrf_failed" | "invalid_bootstrap_token" | "already_initialized" => {
                StatusCode::FORBIDDEN
            }
            "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
            "not_initialized" => StatusCode::CONFLICT,
            "session_capacity_reached" => StatusCode::SERVICE_UNAVAILABLE,
            "validation_error" | "invalid_bootstrap_token_format" => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, error.code(), error.message())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = self.request_id.as_ref().map(ApiRequestId::as_str);
        let mut response = (
            self.status,
            Json(ApiErrorEnvelope {
                ok: false,
                error: ApiErrorBody {
                    code: &self.code,
                    message: &self.message,
                },
                request_id,
            }),
        )
            .into_response();
        if let Some(request_id) = request_id
            && let Ok(value) = HeaderValue::from_str(request_id)
        {
            response.headers_mut().insert("x-request-id", value);
        }
        response
    }
}

/// 旧控制台接口复用统一错误结构，但保持原有无 request_id 响应兼容性。
pub(crate) fn error_response(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
) -> Response {
    ApiError::new(status, code, message).into_response()
}

pub(crate) fn auth_error_response(error: AdminAuthError) -> Response {
    ApiError::from_admin_auth(error).into_response()
}
