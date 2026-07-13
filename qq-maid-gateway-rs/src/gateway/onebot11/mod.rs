//! OneBot 11 反向 WebSocket 单账号连接底座。
//!
//! 当前模块管理协议、鉴权、连接、API request/response 关联和一期文本 sender，
//! 暂不把业务事件送入 Core。后续 adapter 必须复用这里的连接与 sender 边界。

mod connection;
pub mod protocol;
mod sender;
mod server;

pub use connection::{OneBotCallError, OneBotConnectionContext};
pub use sender::{OneBotSendError, OneBotSendResult, OneBotSender};
pub use server::{OneBotServerHandle, spawn_reverse_websocket_server};

#[cfg(test)]
mod tests;
