//! 管理 API 共用的 HTTP 基础设施。

mod auth;
mod error;
mod pagination;
mod response;

pub(crate) use auth::{
    ApiRequestContext, authenticate_admin_request, csrf_token, origin_allowed, session_cookie,
};
pub(crate) use error::{ApiError, auth_error_response, error_response};
pub(crate) use pagination::{PagedResponse, PaginationRequest, ValidatedPagination};
pub(crate) use response::{json_payload, respond, respond_error, respond_raw};
