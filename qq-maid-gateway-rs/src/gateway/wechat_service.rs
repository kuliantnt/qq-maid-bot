//! 微信服务号 HTTP 回调入口。
//!
//! 当前只实现同步文本回复闭环；客服消息、模板消息、素材和异步 follow-up 均留给后续任务。

use std::{net::SocketAddr, time::Duration};

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

const DEFAULT_REPLY_TIMEOUT: Duration = Duration::from_secs(4);
const FALLBACK_ERROR_TEXT: &str = "服务暂时不可用，请稍后再试。";

#[derive(Clone)]
struct WechatServiceState {
    config: WechatServiceConfig,
    respond: RespondClient,
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
    shutdown_token: CancellationToken,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    let addr: SocketAddr = format!("{}:{}", config.bind_host, config.bind_port)
        .parse()
        .context("parse wechat service callback bind addr")?;
    let listener = TcpListener::bind(addr)
        .await
        .context("bind wechat service callback listener")?;
    let path = config.callback_path.clone();
    let state = WechatServiceState { config, respond };
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

    let reply = match tokio::time::timeout(
        DEFAULT_REPLY_TIMEOUT,
        build_sync_reply(&state.respond, &message),
    )
    .await
    {
        Ok(reply) => reply,
        Err(_) => {
            warn!(
                message_id = %message.msg_id,
                "wechat service respond timed out; returning local sync fallback"
            );
            FALLBACK_ERROR_TEXT.to_owned()
        }
    };
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

async fn build_sync_reply(respond: &RespondClient, message: &WechatTextMessage) -> String {
    let inbound = inbound_from_text_message(message);
    let content = platform::render_text_for_core(&inbound);
    let capability = ReplyCapability::wechat_service_text_sync(DEFAULT_REPLY_TIMEOUT);
    let response = match respond.respond_inbound(&inbound, content).await {
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
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::body::to_bytes;
    use qq_maid_core::service::{
        CoreError, CoreHealthSnapshot, CoreInboundClassification, CoreInboundKind, CoreRequest,
        CoreRespondOutput, CoreResponse, CoreService, UpstreamStatusSnapshot,
    };

    use super::*;

    #[derive(Default)]
    struct MockCore {
        last_request: Mutex<Option<CoreRequest>>,
    }

    #[async_trait]
    impl CoreService for MockCore {
        async fn respond(&self, request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
            *self.last_request.lock().unwrap() = Some(request);
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
        let xml = r#"<xml>
<ToUserName><![CDATA[gh_service]]></ToUserName>
<FromUserName><![CDATA[user_openid]]></FromUserName>
<CreateTime>1460537339</CreateTime>
<MsgType><![CDATA[text]]></MsgType>
<Content><![CDATA[你好]]></Content>
<MsgId>1234567890123456</MsgId>
</xml>"#;
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
        let request = core.last_request.lock().unwrap().clone().unwrap();
        assert_eq!(request.platform.as_str(), "wechat_service");
        assert_eq!(
            request.scope_key(),
            "service_account:gh_service:user_openid"
        );
        assert_eq!(request.text, "你好");
    }
}
