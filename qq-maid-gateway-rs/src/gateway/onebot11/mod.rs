//! OneBot 11 反向 WebSocket 单账号聊天入口。
//!
//! 当前模块管理协议、鉴权、连接、API request/response 关联、Core 闭环、一期文本 sender、
//! reply/ref_index 和安全媒体入站；平台流式输出仍不启用。

mod connection;
mod dispatch;
pub mod protocol;
mod scope_dispatcher;
mod sender;
mod server;

pub use connection::{OneBotCallError, OneBotConnectionContext};
pub use sender::{OneBotSendError, OneBotSendResult, OneBotSender};
pub(crate) use server::spawn_reverse_websocket_server_with_ref_index;
pub use server::{OneBotServerHandle, spawn_reverse_websocket_server};

#[cfg(test)]
mod tests;
