use std::{future::Future, pin::Pin};

use super::super::{outbound::RuntimeRecordingSender, typing::TypingStopReason};
use super::types::C2cStreamState;
use crate::api::{OutboundSender, StreamSendResult};
use qq_maid_core::service::{
    CoreFailureKind, CoreOutputPolicy, CoreRespondFailure, CoreResponseEvent,
};

pub(crate) type RespondEventFuture<'a> =
    Pin<Box<dyn Future<Output = Option<CoreResponseEvent>> + Send + 'a>>;
pub(crate) type StreamSendFuture<'a> = Pin<Box<dyn Future<Output = StreamSendResult> + Send + 'a>>;

/// Core 流事件来源抽象，用于把 QQ 流事件消费者与真实 Core channel 解耦，便于覆盖异常分支。
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
        content_raw: &'a str,
        stream_state: &'a mut C2cStreamState,
        input_state: u8,
    ) -> StreamSendFuture<'a>;
}

impl C2cStreamSender for RuntimeRecordingSender<'_> {
    fn send_stream_markdown<'a>(
        &'a self,
        user_openid: &'a str,
        msg_id: Option<&'a str>,
        content_raw: &'a str,
        stream_state: &'a mut C2cStreamState,
        input_state: u8,
    ) -> StreamSendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_stream_message(
                    user_openid,
                    msg_id,
                    content_raw,
                    &mut stream_state.transport,
                    input_state,
                )
                .await;
            match &result {
                Ok(_) => self.runtime.record_qq_send_success(),
                Err(err) => self.runtime.record_qq_send_failure(err.log_summary()),
            }
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
