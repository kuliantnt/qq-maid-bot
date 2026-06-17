//! qq-maid-gateway-rs 库根模块。公开启动入口、配置与 gateway 运行域模块。

pub mod api;
pub mod app;
pub mod auth;
pub mod config;
pub mod gateway;
pub mod markdown;
pub mod media;
pub mod render;
pub mod respond;

// 兼容旧的扁平路径，避免目录重组时一次性改动所有内部引用。
pub use gateway::{dedupe, event, logging, ping, push};
