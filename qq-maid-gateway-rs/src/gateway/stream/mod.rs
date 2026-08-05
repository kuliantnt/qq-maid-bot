//! C2C 流式发送状态机。
//!
//! 该模块只处理 Core 流事件到 QQ Markdown 流式消息的映射。普通 C2C fallback
//! 继续复用 `c2c.rs` 的发送路径，保证 Markdown、文本 fallback、图片开关和运行状态记录一致。

mod delivery;
mod event_stream;
mod progress;
mod send;
mod types;

pub(super) use delivery::stream_respond_c2c;
pub(crate) use event_stream::RespondEventStream;

#[cfg(test)]
pub(super) use delivery::{
    stream_respond_c2c_with_sender, stream_respond_c2c_with_sender_and_ref_index,
    stream_respond_c2c_with_sender_and_typing,
};
#[cfg(test)]
pub(super) use event_stream::{C2cStreamSender, RespondEventFuture, StreamSendFuture};
#[cfg(test)]
pub(super) use progress::STREAM_THROTTLE_MS;
#[cfg(test)]
pub(super) use send::{CumulativeTextAction, reconcile_cumulative_text};
#[cfg(test)]
pub(super) use types::{C2cStreamState, C2cStreamingPhase};

#[cfg(test)]
mod tests;
