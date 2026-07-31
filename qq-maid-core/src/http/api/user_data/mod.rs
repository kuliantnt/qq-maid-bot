//! 控制台当前用户偏好与通用文件 API。

use axum::{Router, extract::DefaultBodyLimit, routing::post};

use crate::{http::routes::OpsHttpState, management::MAX_CONSOLE_FILE_BYTES};

mod dto;
mod handlers;

#[cfg(test)]
mod tests;

const MULTIPART_OVERHEAD_BYTES: usize = 1024 * 1024;
const JSON_BODY_LIMIT_BYTES: usize = 64 * 1024;

pub(crate) fn router() -> Router<OpsHttpState> {
    let json_routes = Router::new()
        .route(
            "/api/v1/console/user-preferences/get",
            post(handlers::get_preferences),
        )
        .route(
            "/api/v1/console/user-preferences/update",
            post(handlers::update_preferences),
        )
        .route("/api/v1/console/files/list", post(handlers::list_files))
        .route("/api/v1/console/files/delete", post(handlers::delete_file))
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT_BYTES));
    let upload_route = Router::new()
        .route("/api/v1/console/files/upload", post(handlers::upload_file))
        .layer(DefaultBodyLimit::max(
            MAX_CONSOLE_FILE_BYTES + MULTIPART_OVERHEAD_BYTES,
        ));

    Router::new().merge(json_routes).merge(upload_route).route(
        "/api/v1/console/files/get/{file_id}",
        post(handlers::get_file),
    )
}
