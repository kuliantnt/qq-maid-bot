//! OneBot 11 反向 WebSocket 单账号连接底座。
//!
//! 当前模块只管理协议、鉴权、连接和 API request/response 关联，不把业务事件送入 Core，
//! 也不实现具体消息发送 action。后续 adapter 与 sender 必须复用这里的连接上下文。

mod connection;
pub mod protocol;
mod server;

pub use connection::{OneBotCallError, OneBotConnectionContext};
pub use server::{OneBotServerHandle, spawn_reverse_websocket_server};

#[cfg(test)]
mod tests;
