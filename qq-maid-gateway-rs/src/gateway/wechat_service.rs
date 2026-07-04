//! 微信服务号 HTTP 回调入口。
//!
//! 当前只实现同步文本回复闭环；客服消息、模板消息、素材和异步 follow-up 均留给后续任务。

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::WechatServiceConfig,
    gateway::{
        dedupe::MessageDedupe,
        outbound::ReplyCapability,
        platform::{
            self,
            wechat_service::{
                WechatInboundMessage, WechatTextMessage, inbound_from_text_message,
                parse_message_xml, render_text_reply_xml, verify_signature,
            },
        },
    },
    render::render_respond_response_for_profile,
    respond::{RespondClient, RespondError, RespondTransport, respond_error_to_qq_text},
};

// 微信同步回复窗口约 5 秒；这里必须提前释放 HTTP 回包，避免平台按同一 MsgId 重试。
const DEFAULT_REPLY_TIMEOUT: Duration = Duration::from_secs(4);
const FALLBACK_ERROR_TEXT: &str = "服务暂时不可用，请稍后再试。";

#[derive(Clone)]
struct WechatServiceState {
    config: WechatServiceConfig,
    respond: RespondClient,
    dedupe: Arc<MessageDedupe>,
}

#[derive(Debug, Deserialize)]
struct VerifyQuery {
    signature: Option<String>,
    timestamp: Option<String>,
    nonce: Option<String>,
    echostr: Option<String>,
}

pub(super) async fn spawn_callback_server(
    config: WechatServiceConfig,
    respond: RespondClient,
    dedupe: Arc<MessageDedupe>,
    shutdown_token: CancellationToken,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    let addr: SocketAddr = format!("{}:{}", config.bind_host, config.bind_port)
        .parse()
        .context("parse wechat service callback bind addr")?;
    let listener = TcpListener::bind(addr)
        .await
        .context("bind wechat service callback listener")?;
    let path = config.callback_path.clone();
    let state = WechatServiceState {
        config,
        respond,
        dedupe,
    };
    let app = Router::new()
        .route(&path, get(verify_url).post(handle_message))
        .with_state(state);

    info!(%addr, path = %path, "wechat service callback listening");
    Ok(tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_token.cancelled().await;
            })
            .await
            .context("serve wechat service callback")
    }))
}

async fn verify_url(
    State(state): State<WechatServiceState>,
    Query(query): Query<VerifyQuery>,
) -> Response {
    let Some(echostr) = query.echostr.as_deref() else {
        return plain(StatusCode::BAD_REQUEST, "missing echostr");
    };
    if !verify_query_signature(&state, &query) {
        return plain(StatusCode::FORBIDDEN, "invalid signature");
    }
    plain(StatusCode::OK, echostr)
}

async fn handle_message(
    State(state): State<WechatServiceState>,
    Query(query): Query<VerifyQuery>,
    body: Bytes,
) -> Response {
    if !verify_query_signature(&state, &query) {
        return plain(StatusCode::FORBIDDEN, "invalid signature");
    }
    let body = match std::str::from_utf8(&body) {
        Ok(body) => body,
        Err(_) => return plain(StatusCode::BAD_REQUEST, "invalid utf-8 xml"),
    };
    let message = match parse_message_xml(body) {
        Ok(WechatInboundMessage::Text(message)) => message,
        Ok(WechatInboundMessage::Unsupported { msg_type }) => {
            debug!(msg_type = %msg_type, "wechat service message type is not supported");
            return plain(StatusCode::OK, "");
        }
        Err(error) => return plain(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    if message.content.trim().is_empty() {
        // 微信服务号允许同步返回空串表示本轮不回复；空 Content 不进入 Core，避免制造无意义会话。
        return plain(StatusCode::OK, "");
    }
    let inbound = inbound_from_text_message(&message);
    let reservation = match reserve_wechat_message(&state, &inbound, &message.msg_id) {
        Ok(reservation) => reservation,
        Err(()) => return plain(StatusCode::OK, ""),
    };

    let reply = match tokio::time::timeout(
        DEFAULT_REPLY_TIMEOUT,
        build_sync_reply(&state.respond, &message, &inbound),
    )
    .await
    {
        Ok(reply) => reply,
        Err(_) => {
            warn!(
                message_id = %message.msg_id,
                timeout_ms = DEFAULT_REPLY_TIMEOUT.as_millis(),
                "wechat service respond timed out; returning empty sync response"
            );
            String::new()
        }
    };
    if let Some(reservation) = reservation {
        reservation.commit();
    }
    if reply.trim().is_empty() {
        return plain(StatusCode::OK, "");
    }

    let xml = render_text_reply_xml(
        &message,
        &crate::render::OutboundMessage::Text { text: reply },
        now_unix_seconds(),
    );
    xml_response(xml)
}

fn reserve_wechat_message(
    state: &WechatServiceState,
    inbound: &platform::InboundMessage,
    message_id: &str,
) -> Result<Option<crate::gateway::dedupe::MessageReservation>, ()> {
    let Some(key) = inbound.dedupe_message_key() else {
        return Ok(None);
    };
    match state.dedupe.reserve_many([key], Instant::now()) {
        Ok(reservation) => Ok(Some(reservation)),
        Err(_) => {
            info!(
                message_id = %message_id,
                "wechat service duplicate message retry ignored"
            );
            Err(())
        }
    }
}

async fn build_sync_reply(
    respond: &RespondClient,
    message: &WechatTextMessage,
    inbound: &platform::InboundMessage,
) -> String {
    let content = platform::render_text_for_core(inbound);
    let capability = ReplyCapability::wechat_service_text_sync(DEFAULT_REPLY_TIMEOUT);
    let response = match respond.respond_inbound(inbound, content).await {
        Ok(RespondTransport::Complete(response)) => Some(response),
        Ok(RespondTransport::Stream(mut stream)) => {
            // 微信服务号同步回复不支持流式；这里只消费到 Completed，超时由外层统一处理。
            let mut completed = None;
            while let Some(event) = stream.recv().await {
                match event {
                    qq_maid_core::service::CoreResponseEvent::Completed(response) => {
                        completed = Some(response);
                        break;
                    }
                    qq_maid_core::service::CoreResponseEvent::Failed(failure) => {
                        warn!(
                            message_id = %message.msg_id,
                            kind = ?failure.kind,
                            "wechat service core stream failed"
                        );
                        return FALLBACK_ERROR_TEXT.to_owned();
                    }
                    _ => {}
                }
            }
            completed
        }
        Err(error) => {
            warn!(
                message_id = %message.msg_id,
                error = %error.log_summary(),
                "wechat service respond failed"
            );
            return respond_error_to_wechat_text(&error);
        }
    };

    let Some(response) = response else {
        return FALLBACK_ERROR_TEXT.to_owned();
    };
    render_respond_response_for_profile(&response, &capability.render)
        .map(|outbound| outbound.fallback_text().to_owned())
        .unwrap_or_default()
}

fn verify_query_signature(state: &WechatServiceState, query: &VerifyQuery) -> bool {
    let Some(token) = state.config.token.as_deref() else {
        return false;
    };
    let (Some(signature), Some(timestamp), Some(nonce)) = (
        query.signature.as_deref(),
        query.timestamp.as_deref(),
        query.nonce.as_deref(),
    ) else {
        return false;
    };
    verify_signature(token, timestamp, nonce, signature)
}

fn respond_error_to_wechat_text(error: &RespondError) -> String {
    let text = respond_error_to_qq_text(error);
    if text.trim().is_empty() {
        FALLBACK_ERROR_TEXT.to_owned()
    } else {
        text
    }
}

fn plain(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body.to_owned(),
    )
        .into_response()
}

fn xml_response(body: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

fn now_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use async_trait::async_trait;
    use axum::body::to_bytes;
    use qq_maid_core::service::{
        CoreError, CoreHealthSnapshot, CoreInboundClassification, CoreInboundKind, CoreRequest,
        CoreRespondOutput, CoreResponse, CoreService, UpstreamStatusSnapshot,
    };
    use tokio::sync::Notify;

    use super::*;

    struct MockCore {
        requests: Mutex<Vec<CoreRequest>>,
        response_delay: Mutex<Option<Duration>>,
        started: Notify,
    }

    impl Default for MockCore {
        fn default() -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                response_delay: Mutex::new(None),
                started: Notify::new(),
            }
        }
    }

    impl MockCore {
        fn with_delay(response_delay: Duration) -> Self {
            Self {
                response_delay: Mutex::new(Some(response_delay)),
                ..Self::default()
            }
        }

        fn request_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }

        fn last_request(&self) -> Option<CoreRequest> {
            self.requests.lock().unwrap().last().cloned()
        }

        async fn wait_for_request_count(&self, expected: usize) {
            loop {
                let notified = self.started.notified();
                if self.request_count() >= expected {
                    return;
                }
                notified.await;
            }
        }
    }

    #[async_trait]
    impl CoreService for MockCore {
        async fn respond(&self, request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
            self.requests.lock().unwrap().push(request);
            self.started.notify_waiters();
            let delay = *self.response_delay.lock().unwrap();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            Ok(CoreRespondOutput::Complete(CoreResponse {
                text: Some("hello <wx> & user".to_owned()),
                markdown: Some("**hello**".to_owned()),
                handled: Some(true),
                session_id: None,
                command: None,
                diagnostics: None,
            }))
        }

        async fn classify_inbound(
            &self,
            _request: CoreRequest,
        ) -> Result<CoreInboundClassification, CoreError> {
            Ok(CoreInboundClassification {
                kind: CoreInboundKind::NormalChat,
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

    fn state(core: Arc<MockCore>) -> WechatServiceState {
        WechatServiceState {
            config: WechatServiceConfig {
                enabled: true,
                token: Some("token".to_owned()),
                ..WechatServiceConfig::default()
            },
            respond: RespondClient::new(core),
            dedupe: Arc::new(MessageDedupe::new(Duration::from_secs(10 * 60))),
        }
    }

    fn signed_get_query() -> VerifyQuery {
        VerifyQuery {
            signature: Some("6db4861c77e0633e0105672fcd41c9fc2766e26e".to_owned()),
            timestamp: Some("timestamp".to_owned()),
            nonce: Some("nonce".to_owned()),
            echostr: Some("echo-ok".to_owned()),
        }
    }

    fn signed_post_query() -> VerifyQuery {
        VerifyQuery {
            signature: Some("6db4861c77e0633e0105672fcd41c9fc2766e26e".to_owned()),
            timestamp: Some("timestamp".to_owned()),
            nonce: Some("nonce".to_owned()),
            echostr: None,
        }
    }

    async fn response_body(response: Response) -> (StatusCode, String) {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    fn text_xml(message_id: &str, content: &str) -> String {
        format!(
            r#"<xml>
<ToUserName><![CDATA[gh_service]]></ToUserName>
<FromUserName><![CDATA[user_openid]]></FromUserName>
<CreateTime>1460537339</CreateTime>
<MsgType><![CDATA[text]]></MsgType>
<Content><![CDATA[{content}]]></Content>
<MsgId>{message_id}</MsgId>
</xml>"#
        )
    }

    #[tokio::test]
    async fn get_verification_returns_echostr_for_valid_signature() {
        let response = verify_url(
            State(state(Arc::new(MockCore::default()))),
            Query(signed_get_query()),
        )
        .await;
        let (status, body) = response_body(response).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "echo-ok");
    }

    #[tokio::test]
    async fn get_verification_requires_echostr() {
        let response = verify_url(
            State(state(Arc::new(MockCore::default()))),
            Query(signed_post_query()),
        )
        .await;
        let (status, body) = response_body(response).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "missing echostr");
    }

    #[tokio::test]
    async fn get_verification_rejects_bad_signature() {
        let mut query = signed_get_query();
        query.signature = Some("bad".to_owned());
        let response = verify_url(State(state(Arc::new(MockCore::default()))), Query(query)).await;
        let (status, body) = response_body(response).await;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body, "invalid signature");
    }

    #[tokio::test]
    async fn post_text_message_invokes_core_and_returns_sync_xml() {
        let core = Arc::new(MockCore::default());
        let xml = text_xml("1234567890123456", "你好");
        let response = handle_message(
            State(state(core.clone())),
            Query(signed_post_query()),
            Bytes::from(xml),
        )
        .await;
        let (status, body) = response_body(response).await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<ToUserName>user_openid</ToUserName>"));
        assert!(body.contains("<FromUserName>gh_service</FromUserName>"));
        assert!(body.contains("<MsgType>text</MsgType>"));
        assert!(body.contains("<Content>hello &lt;wx&gt; &amp; user</Content>"));
        let request = core.last_request().unwrap();
        assert_eq!(request.platform.as_str(), "wechat_service");
        assert_eq!(
            request.scope_key(),
            "service_account:gh_service:user_openid"
        );
        assert_eq!(request.text, "你好");
    }

    #[tokio::test]
    async fn duplicate_text_message_after_completion_does_not_enter_core_again() {
        let core = Arc::new(MockCore::default());
        let state = state(core.clone());
        let xml = text_xml("1234567890123456", "你好");

        let first = handle_message(
            State(state.clone()),
            Query(signed_post_query()),
            Bytes::from(xml.clone()),
        )
        .await;
        let second =
            handle_message(State(state), Query(signed_post_query()), Bytes::from(xml)).await;
        let (first_status, first_body) = response_body(first).await;
        let (second_status, second_body) = response_body(second).await;

        assert_eq!(first_status, StatusCode::OK);
        assert!(first_body.contains("<MsgType>text</MsgType>"));
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(second_body, "");
        assert_eq!(core.request_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_text_message_while_first_in_flight_does_not_enter_core_again() {
        let core = Arc::new(MockCore::with_delay(Duration::from_secs(30)));
        let state = state(core.clone());
        let xml = text_xml("1234567890123456", "你好");

        let first = tokio::spawn(handle_message(
            State(state.clone()),
            Query(signed_post_query()),
            Bytes::from(xml.clone()),
        ));
        core.wait_for_request_count(1).await;

        let duplicate =
            handle_message(State(state), Query(signed_post_query()), Bytes::from(xml)).await;
        let (duplicate_status, duplicate_body) = response_body(duplicate).await;

        assert_eq!(duplicate_status, StatusCode::OK);
        assert_eq!(duplicate_body, "");
        assert_eq!(core.request_count(), 1);

        tokio::time::advance(DEFAULT_REPLY_TIMEOUT + Duration::from_millis(1)).await;
        let (first_status, first_body) = response_body(first.await.unwrap()).await;
        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(first_body, "");
        assert_eq!(core.request_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_text_message_returns_empty_ok_and_later_retry_is_deduped() {
        let core = Arc::new(MockCore::with_delay(Duration::from_secs(30)));
        let state = state(core.clone());
        let xml = text_xml("1234567890123456", "你好");

        let first = tokio::spawn(handle_message(
            State(state.clone()),
            Query(signed_post_query()),
            Bytes::from(xml.clone()),
        ));
        core.wait_for_request_count(1).await;
        tokio::time::advance(DEFAULT_REPLY_TIMEOUT + Duration::from_millis(1)).await;
        let (first_status, first_body) = response_body(first.await.unwrap()).await;

        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(first_body, "");
        assert_eq!(core.request_count(), 1);

        let retry =
            handle_message(State(state), Query(signed_post_query()), Bytes::from(xml)).await;
        let (retry_status, retry_body) = response_body(retry).await;

        assert_eq!(retry_status, StatusCode::OK);
        assert_eq!(retry_body, "");
        assert_eq!(core.request_count(), 1);
    }

    #[tokio::test]
    async fn unsupported_message_type_returns_empty_ok_without_core() {
        let core = Arc::new(MockCore::default());
        let xml = r#"<xml>
<ToUserName><![CDATA[gh_service]]></ToUserName>
<FromUserName><![CDATA[user_openid]]></FromUserName>
<CreateTime>1460537339</CreateTime>
<MsgType><![CDATA[image]]></MsgType>
<MsgId>image-1</MsgId>
</xml>"#;
        let response = handle_message(
            State(state(core.clone())),
            Query(signed_post_query()),
            Bytes::from(xml),
        )
        .await;
        let (status, body) = response_body(response).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "");
        assert_eq!(core.request_count(), 0);
    }

    #[tokio::test]
    async fn empty_text_message_returns_empty_ok_without_core() {
        let core = Arc::new(MockCore::default());
        let xml = text_xml("empty-1", "   ");
        let response = handle_message(
            State(state(core.clone())),
            Query(signed_post_query()),
            Bytes::from(xml),
        )
        .await;
        let (status, body) = response_body(response).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "");
        assert_eq!(core.request_count(), 0);
    }
}
