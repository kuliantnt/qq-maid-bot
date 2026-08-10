//! 受控 Memory 管理 API 路由。

mod dto;
mod handlers;

use axum::{Router, extract::DefaultBodyLimit, routing::post};

use crate::http::routes::OpsHttpState;

pub(crate) fn router() -> Router<OpsHttpState> {
    Router::new()
        .route("/api/v1/console/memories/targets", post(handlers::targets))
        .route("/api/v1/console/memories/list", post(handlers::list))
        .route("/api/v1/console/memories/get", post(handlers::get))
        .route("/api/v1/console/memories/create", post(handlers::create))
        .route("/api/v1/console/memories/update", post(handlers::update))
        .route("/api/v1/console/memories/archive", post(handlers::archive))
        .route("/api/v1/console/memories/restore", post(handlers::restore))
        .route("/api/v1/console/memories/delete", post(handlers::delete))
        .route(
            "/api/v1/console/memories/operations/prepare",
            post(handlers::prepare_operation),
        )
        .route(
            "/api/v1/console/memories/operations/commit",
            post(handlers::commit_operation),
        )
        .layer(DefaultBodyLimit::max(128 * 1024))
}

#[cfg(test)]
mod tests;
