//! HTTP 服务层。
//!
//! 提供基于 Axum 的 HTTP 路由和处理器，供外部服务调用（如进程级 healthz 和控制台）。

pub mod api;
pub mod console;
pub mod routes;
