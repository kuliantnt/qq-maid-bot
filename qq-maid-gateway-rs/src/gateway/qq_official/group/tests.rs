use super::*;

#[test]
fn local_group_hints_use_configured_bot_display_name() {
    assert_eq!(
        empty_group_reply_fallback_text("小助手"),
        "唔，小助手刚刚没整理出可用回复。可以再说一次。"
    );
    assert_eq!(
        group_cooldown_hint_text("小助手"),
        "哦哦，刚刚在处理上一条消息，稍后再说一声小助手就能继续了呢。"
    );
}

#[tokio::test]
async fn group_stream_timeout_sends_core_safe_failure_text() {
    let stream = FakeGroupEventStream::new([CoreResponseEvent::Failed(CoreRespondFailure {
        kind: CoreFailureKind::LlmTimeout,
        message: "LLM 服务处理超时，请稍后再试。".to_owned(),
        retryable: true,
        agent: None,
    })]);
    let failure = match consume_respond_stream(stream).await {
        GroupStreamOutcome::Failed(failure) => failure,
        other => panic!("expected failed group stream, got {other:?}"),
    };
    let sender = RecordingGroupFailureSender::default();
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let message = group_message("联网对比", GroupEventType::GroupAtMessage);

    send_group_stream_failure(&sender, &cache, &message, &failure)
        .await
        .unwrap();

    assert_eq!(
        sender.calls.lock().unwrap().as_slice(),
        [(
            GroupReplyTarget {
                group_openid: "group-1".to_owned(),
                msg_id: Some("group-msg-1".to_owned()),
            },
            "LLM 服务处理超时，请稍后再试。".to_owned(),
        )]
    );
    assert!(cache.lock().unwrap().contains("failure-message-id"));
    assert!(
        cache
            .lock()
            .unwrap()
            .contains_ref_index_id("REFIDX_failure")
    );
}
use crate::{
    api::{ApiError, GroupReplyTarget, QqApiClient, SendFuture},
    auth::AccessTokenManager,
    config::GroupMessageMode,
    gateway::test_support::qq_official_test_config,
    markdown::MarkdownPayload,
};
use axum::{Router, body::Bytes, routing::get};
use qq_maid_common::input_part::{MessageInputPart, MessageMedia};
use qq_maid_core::service::{
    CoreError, CoreFailureKind, CoreHealthSnapshot, CoreInboundClassification, CoreOutputPolicy,
    CoreRequest, CoreRespondOutput, CoreService, UpstreamStatusSnapshot,
};
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::net::TcpListener;

#[derive(Debug)]
struct FakeGroupEventStream {
    events: VecDeque<CoreResponseEvent>,
}

impl FakeGroupEventStream {
    fn new(events: impl IntoIterator<Item = CoreResponseEvent>) -> Self {
        Self {
            events: events.into_iter().collect(),
        }
    }
}

impl RespondEventStream for FakeGroupEventStream {
    fn recv_event<'a>(&'a mut self) -> crate::gateway::stream::RespondEventFuture<'a> {
        Box::pin(async move { self.events.pop_front() })
    }

    fn output_policy(&self) -> CoreOutputPolicy {
        CoreOutputPolicy::ProgressThenStream
    }
}

#[derive(Debug, Default)]
struct RecordingGroupFailureSender {
    calls: Mutex<Vec<(GroupReplyTarget, String)>>,
}

impl GroupOutboundSender for RecordingGroupFailureSender {
    fn send_text<'a>(&'a self, target: &'a GroupReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((target.clone(), text.to_owned()));
            Ok(SendMessageIds {
                message_id: Some("failure-message-id".to_owned()),
                ref_index_id: Some("REFIDX_failure".to_owned()),
            })
        })
    }

    fn send_markdown<'a>(
        &'a self,
        _target: &'a GroupReplyTarget,
        _markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async { Err(ApiError::Unsupported("markdown")) })
    }
}

fn group_message(content: &str, event_type: GroupEventType) -> GroupMessage {
    GroupMessage {
        message_id: "group-msg-1".to_owned(),
        current_msg_idx: None,
        group_openid: "group-1".to_owned(),
        member_openid: Some("member-1".to_owned()),
        member_role: None,
        content: content.to_owned(),
        mentions: Vec::new(),
        reply: None,
        timestamp: None,
        input_parts: if content.trim().is_empty() {
            Vec::new()
        } else {
            vec![qq_maid_common::input_part::MessageInputPart::text(content)]
        },
        attachments: Vec::new(),
        event_type,
        author_is_bot: false,
        author_is_self: false,
    }
}

fn test_config() -> AppConfig {
    let mut config = qq_official_test_config();
    config.api_base = "http://127.0.0.1:1".to_owned();
    config.enable_group_messages = true;
    config
}

fn qq_group_capability() -> ReplyCapability {
    ReplyCapability::qq_official_group(&test_config())
}

struct MockCore {
    response: CoreResponse,
    respond_calls: Arc<AtomicUsize>,
    classify_calls: Arc<AtomicUsize>,
    immediate_inputs: Vec<String>,
}

#[async_trait::async_trait]
impl CoreService for MockCore {
    async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
        self.respond_calls.fetch_add(1, Ordering::SeqCst);
        Ok(CoreRespondOutput::Complete(Box::new(self.response.clone())))
    }

    async fn classify_inbound(
        &self,
        request: CoreRequest,
    ) -> Result<CoreInboundClassification, CoreError> {
        self.classify_calls.fetch_add(1, Ordering::SeqCst);
        Ok(CoreInboundClassification {
            kind: if self
                .immediate_inputs
                .iter()
                .any(|input| input == &request.text)
            {
                CoreInboundKind::Immediate
            } else {
                CoreInboundKind::NormalChat
            },
        })
    }

    async fn upstream_check(&self) -> Result<(), CoreError> {
        Ok(())
    }

    fn health_snapshot(&self) -> CoreHealthSnapshot {
        CoreHealthSnapshot {
            ok: true,
            provider: "mock".to_owned(),
            model: "mock".to_owned(),
            stream: false,
            upstream: UpstreamStatusSnapshot::default(),
        }
    }
}

fn respond_client() -> RespondClient {
    respond_client_with_counter(Arc::new(AtomicUsize::new(0)))
}

fn respond_client_with_counter(respond_calls: Arc<AtomicUsize>) -> RespondClient {
    respond_client_with_classification(respond_calls, Arc::new(AtomicUsize::new(0)), Vec::new())
}

fn respond_client_with_classification(
    respond_calls: Arc<AtomicUsize>,
    classify_calls: Arc<AtomicUsize>,
    immediate_inputs: Vec<&str>,
) -> RespondClient {
    respond_client_with_response(
        respond_calls,
        classify_calls,
        immediate_inputs,
        CoreResponse {
            output: None,
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        },
    )
}

fn respond_client_with_response(
    respond_calls: Arc<AtomicUsize>,
    classify_calls: Arc<AtomicUsize>,
    immediate_inputs: Vec<&str>,
    response: CoreResponse,
) -> RespondClient {
    RespondClient::new(Arc::new(MockCore {
        response,
        respond_calls,
        classify_calls,
        immediate_inputs: immediate_inputs.into_iter().map(str::to_owned).collect(),
    }))
}

fn api_client() -> QqApiClient {
    QqApiClient::new(
        qq_maid_common::http_client::client(),
        "http://127.0.0.1:1",
        AccessTokenManager::new(
            qq_maid_common::http_client::client(),
            "app",
            "secret",
            Duration::from_secs(60),
        ),
    )
}

fn assert_group_send_error(err: anyhow::Error) {
    assert!(
        matches!(
            err.downcast_ref::<ApiError>(),
            Some(ApiError::Auth(_) | ApiError::Http(_) | ApiError::Status { .. })
        ),
        "expected QQ send/auth error from fake API endpoint, got: {err:#}"
    );
}

fn bot_identity() -> SharedBotIdentity {
    Arc::new(crate::gateway::bot_identity::BotIdentity::new("app", &[]))
}

fn media_message(
    message_id: &str,
    content: &str,
    event_type: GroupEventType,
    url: String,
) -> GroupMessage {
    let attachment = crate::event::Attachment {
        content_type: Some("image/jpeg".to_owned()),
        filename: Some("a.jpg".to_owned()),
        url: Some(url),
        size_bytes: None,
        media_id: None,
        file_id: None,
        attachment_id: None,
        asr_refer_text: None,
        voice_wav_url: None,
    };
    let mut message = group_message(content, event_type);
    message.message_id = message_id.to_owned();
    message.attachments = vec![attachment.clone()];
    message.input_parts = vec![
        MessageInputPart::text(content),
        MessageInputPart::image(MessageMedia {
            mime_type: attachment.content_type.clone(),
            filename: attachment.filename.clone(),
            url: attachment.url.clone(),
            status: qq_maid_common::input_part::MediaStatus::MissingReadableUrl,
            ..Default::default()
        }),
    ];
    message
}

fn unique_media_dir(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "qq-maid-group-{name}-{}-{nanos}",
        std::process::id()
    ))
}

fn media_file_count(root: &std::path::Path) -> usize {
    if !root.exists() {
        return 0;
    }
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                count += 1;
            }
        }
    }
    count
}

async fn spawn_media_server() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_for_route = hits.clone();
    let app = Router::new().route(
        "/a.jpg",
        get(move || {
            let hits = hits_for_route.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    [(reqwest::header::CONTENT_TYPE.as_str(), "image/jpeg")],
                    Bytes::from_static(b"fake-jpeg"),
                )
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/a.jpg"), hits)
}

mod behavior;
