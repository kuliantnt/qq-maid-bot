//! 管理 API 的统一成功响应和 JSON rejection 映射。

use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::http::{console_routes::with_console_cors, routes::OpsHttpState};

use super::{ApiError, ApiRequestContext};

#[derive(Serialize)]
struct ApiSuccessEnvelope<'a, T> {
    ok: bool,
    data: T,
    request_id: &'a str,
}

pub(crate) fn json_payload<T: DeserializeOwned>(
    payload: Result<Json<T>, JsonRejection>,
    context: &ApiRequestContext,
) -> Result<T, ApiError> {
    payload.map(|Json(value)| value).map_err(|error| {
        ApiError::invalid_json(error.body_text()).with_request_id(context.request_id.clone())
    })
}

pub(crate) fn respond<T: Serialize>(
    state: &OpsHttpState,
    headers: &HeaderMap,
    context: &ApiRequestContext,
    result: Result<T, ApiError>,
) -> Response {
    let response = match result {
        Ok(data) => {
            let mut response = Json(ApiSuccessEnvelope {
                ok: true,
                data,
                request_id: context.request_id.as_str(),
            })
            .into_response();
            if let Ok(value) = HeaderValue::from_str(context.request_id.as_str()) {
                response.headers_mut().insert("x-request-id", value);
            }
            response
        }
        Err(error) => error
            .with_request_id(context.request_id.clone())
            .into_response(),
    };
    with_console_cors(response, state, headers)
}

/// 文件内容等非 JSON 成功响应仍复用请求 ID、错误包络、CORS 与安全响应头。
pub(crate) fn respond_raw(
    state: &OpsHttpState,
    headers: &HeaderMap,
    context: &ApiRequestContext,
    result: Result<Response, ApiError>,
) -> Response {
    let mut response = match result {
        Ok(response) => response,
        Err(error) => error
            .with_request_id(context.request_id.clone())
            .into_response(),
    };
    if let Ok(value) = HeaderValue::from_str(context.request_id.as_str()) {
        response.headers_mut().insert("x-request-id", value);
    }
    with_console_cors(response, state, headers)
}

pub(crate) fn respond_error(
    state: &OpsHttpState,
    headers: &HeaderMap,
    error: ApiError,
) -> Response {
    with_console_cors(error.into_response(), state, headers)
}
