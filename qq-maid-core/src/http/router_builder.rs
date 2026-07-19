//! 控制台路由树组装。

use axum::{
    Router,
    routing::{get, post},
};

use super::{
    console_routes::{
        console_asset, console_configuration, console_index, console_status, healthz,
        markdown_render, markdown_render_preflight,
    },
    management::management_router,
    routes::OpsHttpState,
};

/// 构建 Axum 路由树，注册所有 HTTP 端点。
pub fn build_router(state: OpsHttpState) -> Router {
    let console_enabled = state.config.web_console_enabled;
    let router = Router::new().route("/healthz", get(healthz));
    let router = if console_enabled {
        router
            .route("/console/", get(console_index))
            .route("/console/{*asset}", get(console_asset))
            .route("/api/v1/console/status", get(console_status))
            .route("/api/v1/console/configuration", get(console_configuration))
            .route(
                "/api/v1/markdown/render",
                post(markdown_render).options(markdown_render_preflight),
            )
            .merge(management_router())
    } else {
        router
    };
    router.with_state(state)
}
