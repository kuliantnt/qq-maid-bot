//! OneBot 11 反向 WebSocket 单账号 text-only 聊天入口。
//!
//! 当前模块管理协议、鉴权、连接、API request/response 关联、Core 最小闭环和一期文本
//! sender。引用、媒体和平台流式输出仍由后续任务补齐。

mod connection;
mod dispatch;
pub mod protocol;
mod scope_dispatcher;
mod sender;
mod server;

pub use connection::{OneBotCallError, OneBotConnectionContext};
pub use sender::{OneBotSendError, OneBotSendResult, OneBotSender};
pub use server::{OneBotServerHandle, spawn_reverse_websocket_server};

#[cfg(test)]
mod tests;
