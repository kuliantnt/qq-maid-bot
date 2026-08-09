//! 控制台知识库托管文件 API。
//!
//! 路由只在控制台启用时注册，认证、CSRF、Origin 和响应包络复用通用管理 API；
//! 文件状态与处理流程由 knowledge 领域门面负责。

use axum::{Router, extract::DefaultBodyLimit, routing::post};

use crate::http::routes::OpsHttpState;

mod dto;
mod handlers;

#[cfg(test)]
mod tests;

const MULTIPART_OVERHEAD_BYTES: usize = 1024 * 1024;
const JSON_BODY_LIMIT_BYTES: usize = 64 * 1024;

pub(crate) fn router(max_file_bytes: u64) -> Router<OpsHttpState> {
    let max_file_bytes = usize::try_from(max_file_bytes)
        .unwrap_or(usize::MAX.saturating_sub(MULTIPART_OVERHEAD_BYTES));
    let json_routes = Router::new()
        .route(
            "/api/v1/console/knowledge/files/capabilities",
            post(handlers::capabilities),
        )
        .route(
            "/api/v1/console/knowledge/files/list",
            post(handlers::list_files),
        )
        .route(
            "/api/v1/console/knowledge/files/delete",
            post(handlers::delete_file),
        )
        .route(
            "/api/v1/console/knowledge/files/retry",
            post(handlers::retry_file),
        )
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT_BYTES));
    let upload_route = Router::new()
        .route(
            "/api/v1/console/knowledge/files/upload",
            post(handlers::upload_file),
        )
        .layer(DefaultBodyLimit::max(
            max_file_bytes.saturating_add(MULTIPART_OVERHEAD_BYTES),
        ));

    Router::new().merge(json_routes).merge(upload_route).route(
        "/api/v1/console/knowledge/files/get/{file_id}",
        post(handlers::get_file),
    )
}
