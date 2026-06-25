//! gateway 出站发送封装。
//!
//! 这里集中维护“真实 QQ 发送 -> runtime 状态记录”的约束，
//! 避免不同调用点各自实现后出现重复记录或遗漏记录。

use crate::{
    api::{
        C2cReplyTarget, GroupOutboundSender, GroupReplyTarget, OutboundSender, QqApiClient,
        SendFuture, SendResult,
    },
    markdown::MarkdownPayload,
    media::ImagePayload,
};

use super::ping::GatewayRuntimeStatus;

pub(crate) async fn send_c2c_text_with_status(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    user_openid: &str,
    msg_id: Option<&str>,
    text: &str,
) -> SendResult {
    let result = api.send_c2c_text(user_openid, msg_id, text).await;
    record_qq_send_result(runtime, &result);
    result
}

pub(crate) async fn send_group_text_with_status(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    group_openid: &str,
    msg_id: Option<&str>,
    text: &str,
) -> SendResult {
    let result = api.send_group_text(group_openid, msg_id, text).await;
    record_qq_send_result(runtime, &result);
    result
}

pub(crate) fn record_qq_send_result(runtime: &GatewayRuntimeStatus, result: &SendResult) {
    match result {
        Ok(_) => runtime.record_qq_send_success(),
        Err(err) => runtime.record_qq_send_failure(err.log_summary()),
    }
}

pub(crate) struct RuntimeRecordingSender<'a> {
    pub(crate) inner: &'a QqApiClient,
    pub(crate) runtime: &'a GatewayRuntimeStatus,
}

pub(crate) struct RuntimeRecordingGroupSender<'a> {
    pub(crate) inner: &'a QqApiClient,
    pub(crate) runtime: &'a GatewayRuntimeStatus,
}

impl OutboundSender for RuntimeRecordingSender<'_> {
    fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_text(&target.user_openid, target.msg_id.as_deref(), text)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }

    fn send_markdown<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_markdown(&target.user_openid, target.msg_id.as_deref(), markdown)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }

    fn send_image<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        image: &'a ImagePayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_image(&target.user_openid, target.msg_id.as_deref(), image)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }
}

impl GroupOutboundSender for RuntimeRecordingGroupSender<'_> {
    fn send_text<'a>(&'a self, target: &'a GroupReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_group_text(&target.group_openid, target.msg_id.as_deref(), text)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }

    fn send_markdown<'a>(
        &'a self,
        target: &'a GroupReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_group_markdown(&target.group_openid, target.msg_id.as_deref(), markdown)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiError;

    #[test]
    fn record_qq_send_result_updates_runtime_status() {
        let runtime = GatewayRuntimeStatus::new();
        let success: SendResult = Ok(None);

        record_qq_send_result(&runtime, &success);
        let snapshot = runtime.snapshot();
        assert!(snapshot.last_qq_send_success_at.is_some());
        assert_eq!(snapshot.last_qq_send_failure_at, None);

        let failure: SendResult = Err(ApiError::Unsupported("text"));
        record_qq_send_result(&runtime, &failure);
        let snapshot = runtime.snapshot();

        assert!(snapshot.last_qq_send_failure_at.is_some());
        assert_eq!(
            snapshot.last_qq_send_failure_summary.as_deref(),
            Some("text sending is unsupported")
        );
    }
}
