use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use reqwest::StatusCode;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tracing::{info, warn};

use crate::{
    auth::{AccessTokenManager, AuthError},
    logging::{mask_openid, reqwest_error_summary},
    markdown::{MarkdownPayload, build_c2c_markdown_payload, build_group_markdown_payload},
    media::{ImagePayload, build_c2c_image_payload},
    render::OutboundMessage,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct C2cReplyTarget {
    pub user_openid: String,
    pub msg_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupReplyTarget {
    pub group_openid: String,
    pub msg_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QqApiClient {
    client: reqwest::Client,
    api_base: String,
    auth: AccessTokenManager,
    msg_seq: Arc<AtomicU64>,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("QQ OpenAPI request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("QQ OpenAPI returned {status}")]
    Status { status: StatusCode, body: String },
    #[error("{0} sending is not supported by this sender")]
    Unsupported(&'static str),
}

impl ApiError {
    pub fn log_summary(&self) -> String {
        match self {
            Self::Auth(_) => "QQ auth error".to_owned(),
            Self::Http(error) => reqwest_error_summary(error),
            Self::Status { status, .. } => format!("http status {status}"),
            Self::Unsupported(kind) => format!("{kind} sending is unsupported"),
        }
    }
}

#[derive(Debug, Serialize)]
struct C2cTextPayload<'a> {
    content: &'a str,
    msg_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

#[derive(Debug, Serialize)]
struct GroupTextPayload<'a> {
    content: &'a str,
    msg_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ApiError>> + Send + 'a>>;

pub trait OutboundSender: Send + Sync {
    fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a>;
    fn send_markdown<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a>;
    fn send_image<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        image: &'a ImagePayload,
    ) -> SendFuture<'a>;
}

impl QqApiClient {
    pub fn new(
        client: reqwest::Client,
        api_base: impl Into<String>,
        auth: AccessTokenManager,
    ) -> Self {
        Self {
            client,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            auth,
            msg_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn next_msg_seq(&self) -> u32 {
        let value = self.msg_seq.fetch_add(1, Ordering::Relaxed);
        (value % 10_000 + 1) as u32
    }

    pub async fn send_c2c_text(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        text: &str,
    ) -> Result<(), ApiError> {
        let payload = build_c2c_text_payload(text, msg_id, self.next_msg_seq());
        self.post_c2c_message(user_openid, msg_id, "text", &payload)
            .await
    }

    pub async fn send_group_text(
        &self,
        group_openid: &str,
        msg_id: Option<&str>,
        text: &str,
    ) -> Result<(), ApiError> {
        let payload = build_group_text_payload(text, msg_id, self.next_msg_seq());
        self.post_group_message(group_openid, msg_id, "text", &payload)
            .await
    }

    pub async fn send_group_markdown(
        &self,
        group_openid: &str,
        msg_id: Option<&str>,
        markdown: &MarkdownPayload,
    ) -> Result<(), ApiError> {
        let payload = build_group_markdown_payload(markdown, msg_id, self.next_msg_seq());
        self.post_group_message(group_openid, msg_id, "markdown", &payload)
            .await
    }

    pub async fn send_c2c_markdown(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        markdown: &MarkdownPayload,
    ) -> Result<(), ApiError> {
        let payload = build_c2c_markdown_payload(markdown, msg_id, self.next_msg_seq());
        self.post_c2c_message(user_openid, msg_id, "markdown", &payload)
            .await
    }

    pub async fn send_c2c_image(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        image: &ImagePayload,
    ) -> Result<(), ApiError> {
        let payload = build_c2c_image_payload(image, msg_id, self.next_msg_seq());
        self.post_c2c_message(user_openid, msg_id, "image", &payload)
            .await
    }

    async fn post_c2c_message(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        message_type: &'static str,
        payload: &Value,
    ) -> Result<(), ApiError> {
        let url = format!("{}/v2/users/{user_openid}/messages", self.api_base);
        let masked_user = mask_openid(user_openid);
        let response = self
            .client
            .post(url)
            .header("Authorization", self.auth.authorization_header().await?)
            .json(payload)
            .send()
            .await
            .map_err(|error| {
                warn!(
                    user = %masked_user,
                    source_message_id = msg_id.unwrap_or(""),
                    message_type = message_type,
                    error = %reqwest_error_summary(&error),
                    "QQ send request failed"
                );
                ApiError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            warn!(
                user = %masked_user,
                source_message_id = msg_id.unwrap_or(""),
                message_type = message_type,
                status = %status,
                "QQ send returned non-success status"
            );
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Status { status, body });
        }

        info!(
            user = %masked_user,
            source_message_id = msg_id.unwrap_or(""),
            message_type = message_type,
            "qq send success"
        );
        Ok(())
    }

    async fn post_group_message(
        &self,
        group_openid: &str,
        msg_id: Option<&str>,
        message_type: &'static str,
        payload: &Value,
    ) -> Result<(), ApiError> {
        let url = format!("{}/v2/groups/{group_openid}/messages", self.api_base);
        let masked_group = mask_openid(group_openid);
        let response = self
            .client
            .post(url)
            .header("Authorization", self.auth.authorization_header().await?)
            .json(payload)
            .send()
            .await
            .map_err(|error| {
                warn!(
                    group = %masked_group,
                    source_message_id = msg_id.unwrap_or(""),
                    message_type = message_type,
                    error = %reqwest_error_summary(&error),
                    "QQ group send request failed"
                );
                ApiError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            warn!(
                group = %masked_group,
                source_message_id = msg_id.unwrap_or(""),
                message_type = message_type,
                status = %status,
                "QQ group send returned non-success status"
            );
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Status { status, body });
        }

        info!(
            group = %masked_group,
            source_message_id = msg_id.unwrap_or(""),
            message_type = message_type,
            "qq group send success"
        );
        Ok(())
    }
}

impl OutboundSender for QqApiClient {
    fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            self.send_c2c_text(&target.user_openid, target.msg_id.as_deref(), text)
                .await
        })
    }

    fn send_markdown<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            self.send_c2c_markdown(&target.user_openid, target.msg_id.as_deref(), markdown)
                .await
        })
    }

    fn send_image<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        image: &'a ImagePayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            self.send_c2c_image(&target.user_openid, target.msg_id.as_deref(), image)
                .await
        })
    }
}

pub fn build_c2c_text_payload(text: &str, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(C2cTextPayload {
        content: text,
        msg_type: 0,
        msg_id,
        msg_seq,
    })
    .expect("C2C text payload should serialize")
}

pub fn build_group_text_payload(text: &str, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(GroupTextPayload {
        content: text,
        msg_type: 0,
        msg_id,
        msg_seq,
    })
    .expect("group text payload should serialize")
}

pub async fn send_outbound_with_fallback<S: OutboundSender + ?Sized>(
    sender: &S,
    target: &C2cReplyTarget,
    outbound: &OutboundMessage,
) -> Result<(), ApiError> {
    match outbound {
        OutboundMessage::Text { text } => sender.send_text(target, text).await,
        OutboundMessage::Markdown {
            markdown,
            fallback_text,
        } => match sender.send_markdown(target, markdown).await {
            Ok(()) => Ok(()),
            Err(err) if !fallback_text.trim().is_empty() => {
                warn!(
                    user = %mask_openid(&target.user_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    error = %err.log_summary(),
                    "markdown send failed; falling back to text"
                );
                match sender.send_text(target, fallback_text).await {
                    Ok(()) => Ok(()),
                    Err(fallback_err) => {
                        warn!(
                            user = %mask_openid(&target.user_openid),
                            source_message_id = target.msg_id.as_deref().unwrap_or(""),
                            error = %fallback_err.log_summary(),
                            "markdown fallback text send failed"
                        );
                        Err(fallback_err)
                    }
                }
            }
            Err(err) => Err(err),
        },
        OutboundMessage::Image {
            image,
            fallback_text,
        } => match sender.send_image(target, image).await {
            Ok(()) => Ok(()),
            Err(err) if !fallback_text.trim().is_empty() => {
                warn!(
                    user = %mask_openid(&target.user_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    error = %err.log_summary(),
                    "image send failed; falling back to text"
                );
                match sender.send_text(target, fallback_text).await {
                    Ok(()) => Ok(()),
                    Err(fallback_err) => {
                        warn!(
                            user = %mask_openid(&target.user_openid),
                            source_message_id = target.msg_id.as_deref().unwrap_or(""),
                            error = %fallback_err.log_summary(),
                            "image fallback text send failed"
                        );
                        Err(fallback_err)
                    }
                }
            }
            Err(err) => Err(err),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::{markdown::MarkdownPayload, media::ImagePayload, render::OutboundMessage};

    #[test]
    fn c2c_text_payload_matches_qq_shape() {
        let payload = build_c2c_text_payload("hello", Some("msg-1"), 7);

        assert_eq!(payload["content"], "hello");
        assert_eq!(payload["msg_type"], 0);
        assert_eq!(payload["msg_id"], "msg-1");
        assert_eq!(payload["msg_seq"], 7);
    }

    #[derive(Debug, Default)]
    struct MockSender {
        calls: Mutex<Vec<String>>,
    }

    impl MockSender {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl OutboundSender for MockSender {
        fn send_text<'a>(&'a self, _target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("text:{text}"));
                Ok(())
            })
        }

        fn send_markdown<'a>(
            &'a self,
            _target: &'a C2cReplyTarget,
            _markdown: &'a MarkdownPayload,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("markdown".to_owned());
                Err(ApiError::Unsupported("markdown"))
            })
        }

        fn send_image<'a>(
            &'a self,
            _target: &'a C2cReplyTarget,
            _image: &'a ImagePayload,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("image".to_owned());
                Err(ApiError::Unsupported("image"))
            })
        }
    }

    fn target() -> C2cReplyTarget {
        C2cReplyTarget {
            user_openid: "user-1".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }
    }

    /// 合并 2 个 send 回退测试为表驱动测试。
    #[tokio::test]
    async fn send_failure_falls_back_to_text() {
        struct Case {
            name: &'static str,
            outbound: OutboundMessage,
            expected_calls: &'static [&'static str],
        }

        let cases = [
            Case {
                name: "markdown_send_failure_falls_back_to_text",
                outbound: OutboundMessage::Markdown {
                    markdown: MarkdownPayload::new("# hello"),
                    fallback_text: "hello".to_owned(),
                },
                expected_calls: &["markdown", "text:hello"],
            },
            Case {
                name: "image_send_failure_falls_back_to_text",
                outbound: OutboundMessage::Image {
                    image: ImagePayload::new("file-info"),
                    fallback_text: "image fallback".to_owned(),
                },
                expected_calls: &["image", "text:image fallback"],
            },
        ];

        for case in &cases {
            let sender = MockSender::default();
            send_outbound_with_fallback(&sender, &target(), &case.outbound)
                .await
                .unwrap_or_else(|e| panic!("case '{}' failed: {:?}", case.name, e));
            assert_eq!(
                sender.calls(),
                case.expected_calls,
                "case '{}' failed: calls mismatch",
                case.name
            );
        }
    }
}
