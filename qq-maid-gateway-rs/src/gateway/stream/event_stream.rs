use std::{future::Future, pin::Pin};

use super::super::{
    outbound::{RuntimeRecordingSender, record_qq_send_result},
    typing::TypingStopReason,
};
use crate::{
    api::{C2cStreamState, OutboundSender, StreamSendResult},
    markdown::MarkdownPayload,
    respond::RespondEvent,
};
use qq_maid_core::service::{CoreFailureKind, CoreOutputPolicy, CoreRespondFailure};

pub(crate) type RespondEventFuture<'a> =
    Pin<Box<dyn Future<Output = Option<RespondEvent>> + Send + 'a>>;
pub(crate) type StreamSendFuture<'a> = Pin<Box<dyn Future<Output = StreamSendResult> + Send + 'a>>;

/// Core 流事件来源抽象，仅用于把 C2C 流式状态机与真实 Core channel 解耦，便于覆盖异常分支。
pub(crate) trait RespondEventStream: Send {
    fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a>;

    fn output_policy(&self) -> CoreOutputPolicy {
        CoreOutputPolicy::DirectStream
    }
}

impl RespondEventStream for qq_maid_core::service::CoreResponseStream {
    fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a> {
        Box::pin(async move { self.recv().await })
    }

    fn output_policy(&self) -> CoreOutputPolicy {
        self.output_policy()
    }
}

/// C2C 流式发送抽象；普通消息能力复用 `OutboundSender`，确保 Pending fallback 走同一发送链路。
pub(crate) trait C2cStreamSender: OutboundSender {
    fn send_stream_markdown<'a>(
        &'a self,
        user_openid: &'a str,
        msg_id: Option<&'a str>,
        markdown: &'a MarkdownPayload,
        stream_state: &'a mut C2cStreamState,
        stream_state_value: u8,
        reset: Option<bool>,
    ) -> StreamSendFuture<'a>;
}

impl C2cStreamSender for RuntimeRecordingSender<'_> {
    fn send_stream_markdown<'a>(
        &'a self,
        user_openid: &'a str,
        msg_id: Option<&'a str>,
        markdown: &'a MarkdownPayload,
        stream_state: &'a mut C2cStreamState,
        stream_state_value: u8,
        reset: Option<bool>,
    ) -> StreamSendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_markdown_stream(
                    user_openid,
                    msg_id,
                    markdown,
                    stream_state,
                    stream_state_value,
                    reset,
                )
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }
}

pub(crate) fn failure_stop_reason(failure: &CoreRespondFailure) -> TypingStopReason {
    match failure.kind {
        CoreFailureKind::SearchTimeout | CoreFailureKind::LlmTimeout => TypingStopReason::Timeout,
        _ => TypingStopReason::RequestFailed,
    }
}
