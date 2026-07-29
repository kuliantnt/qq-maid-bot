//! Todo 管理 API 路由。

mod dto;
mod handlers;

use axum::{Router, extract::DefaultBodyLimit, routing::post};

use crate::http::routes::OpsHttpState;

pub(crate) fn router() -> Router<OpsHttpState> {
    Router::new()
        .route("/api/v1/console/todo/create", post(handlers::create))
        .route("/api/v1/console/todo/list", post(handlers::list))
        .route("/api/v1/console/todo/get", post(handlers::get))
        .route("/api/v1/console/todo/update", post(handlers::update))
        .route("/api/v1/console/todo/delete", post(handlers::delete))
        .layer(DefaultBodyLimit::max(64 * 1024))
}

#[cfg(test)]
mod tests;
