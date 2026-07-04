//! 微信服务号 HTTP 回调入口。
//!
//! 当前实现 text-only 同步回复和长任务客服消息补发；模板消息、素材和媒体消息均留给后续任务。

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Context;
use async_trait::async_trait;
use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use qq_maid_common::redaction::redact_sensitive_text;

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
    logging::{mask_openid, mask_url, reqwest_error_summary},
    render::render_respond_response_for_profile,
    respond::{RespondClient, RespondError, RespondTransport, respond_error_to_qq_text},
};

const FALLBACK_ERROR_TEXT: &str = "服务暂时不可用，请稍后再试。";
const SLOW_SYNC_FALLBACK_TEXT: &str = "这次处理需要更久一点，已收到请求，请稍后查看回复。";
const WECHAT_SUCCESS_BODY: &str = "success";
const WECHAT_TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct WechatServiceState {
    config: WechatServiceConfig,
    respond: RespondClient,
    dedupe: Arc<MessageDedupe>,
    customer_messenger: Option<Arc<dyn WechatCustomerMessenger>>,
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
        customer_messenger: build_customer_messenger(&config),
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

    let reply_timeout = state.config.reply_timeout;
    let (reply_tx, reply_rx) = oneshot::channel();
    tokio::spawn(run_response_job(
        state.clone(),
        message.clone(),
        inbound,
        reservation,
        reply_tx,
    ));

    let reply = match tokio::time::timeout(reply_timeout, reply_rx).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(_)) => FALLBACK_ERROR_TEXT.to_owned(),
        Err(_) => {
            warn!(
                message_id = %message.msg_id,
                timeout_ms = reply_timeout.as_millis(),
                customer_message_enabled = state.customer_messenger.is_some(),
                "wechat service respond exceeded sync budget"
            );
            if state.customer_messenger.is_some() {
                return plain(StatusCode::OK, WECHAT_SUCCESS_BODY);
            }
            SLOW_SYNC_FALLBACK_TEXT.to_owned()
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

async fn run_response_job(
    state: WechatServiceState,
    message: WechatTextMessage,
    inbound: platform::InboundMessage,
    reservation: Option<crate::gateway::dedupe::MessageReservation>,
    reply_tx: oneshot::Sender<String>,
) {
    let reply = build_reply_text(
        &state.respond,
        &message,
        &inbound,
        state.config.reply_timeout,
    )
    .await;
    let needs_async_follow_up = reply_tx.send(reply.clone()).is_err();
    if needs_async_follow_up {
        handle_slow_job_completion(&state, &message, &reply).await;
    }
    if let Some(reservation) = reservation {
        reservation.commit();
    }
}

async fn handle_slow_job_completion(
    state: &WechatServiceState,
    message: &WechatTextMessage,
    reply: &str,
) {
    let Some(messenger) = state.customer_messenger.as_ref() else {
        info!(
            message_id = %message.msg_id,
            user = %mask_openid(&message.from_user_name),
            reply_len = reply.chars().count(),
            "wechat service slow response completed without customer-message capability"
        );
        return;
    };
    if reply.trim().is_empty() {
        info!(
            message_id = %message.msg_id,
            user = %mask_openid(&message.from_user_name),
            "wechat service slow response completed with empty reply; customer message skipped"
        );
        return;
    }
    match messenger.send_text(&message.from_user_name, reply).await {
        Ok(()) => {
            info!(
                message_id = %message.msg_id,
                user = %mask_openid(&message.from_user_name),
                reply_len = reply.chars().count(),
                "wechat customer text message sent"
            );
        }
        Err(error) => {
            warn!(
                message_id = %message.msg_id,
                user = %mask_openid(&message.from_user_name),
                reply_len = reply.chars().count(),
                error = %error.log_summary(),
                "wechat customer text message failed"
            );
        }
    }
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

async fn build_reply_text(
    respond: &RespondClient,
    message: &WechatTextMessage,
    inbound: &platform::InboundMessage,
    reply_timeout: Duration,
) -> String {
    let content = platform::render_text_for_core(inbound);
    let capability = ReplyCapability::wechat_service_text_sync(reply_timeout);
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

fn build_customer_messenger(
    config: &WechatServiceConfig,
) -> Option<Arc<dyn WechatCustomerMessenger>> {
    let (Some(app_id), Some(app_secret)) = (config.app_id.as_ref(), config.app_secret.as_ref())
    else {
        return None;
    };
    Some(Arc::new(WechatCustomerMessageClient::new(
        reqwest::Client::new(),
        config.api_base.clone(),
        app_id.clone(),
        app_secret.clone(),
    )))
}

#[async_trait]
trait WechatCustomerMessenger: Send + Sync {
    async fn send_text(&self, touser: &str, text: &str) -> Result<(), WechatCustomerMessageError>;
}

#[derive(Debug)]
struct WechatCustomerMessageClient {
    client: reqwest::Client,
    api_base: String,
    app_id: String,
    app_secret: String,
    token: Mutex<Option<CachedWechatAccessToken>>,
}

#[derive(Debug, Clone)]
struct CachedWechatAccessToken {
    token: String,
    expires_at: Instant,
}

#[derive(Debug, Error)]
enum WechatCustomerMessageError {
    #[error("WeChat API request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("WeChat API returned {status}")]
    Status {
        status: StatusCode,
        body_summary: String,
    },
    #[error("WeChat API returned errcode={errcode}: {errmsg}")]
    Api { errcode: i64, errmsg: String },
    #[error("WeChat token response missing access_token")]
    MissingAccessToken,
    #[error("invalid WeChat API url: {0}")]
    InvalidUrl(String),
}

impl WechatCustomerMessageError {
    fn log_summary(&self) -> String {
        match self {
            Self::Http(error) => reqwest_error_summary(error),
            Self::Status {
                status,
                body_summary,
            } => {
                if body_summary.is_empty() {
                    format!("http status {status}")
                } else {
                    format!("http status {status}: {body_summary}")
                }
            }
            Self::Api { errcode, errmsg } => format!("errcode={errcode}: {errmsg}"),
            Self::MissingAccessToken => "missing access_token".to_owned(),
            Self::InvalidUrl(_) => "invalid api url".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WechatAccessTokenResponse {
    access_token: Option<String>,
    expires_in: Option<u64>,
    errcode: Option<i64>,
    errmsg: Option<String>,
}

#[derive(Debug, Serialize)]
struct WechatCustomerTextPayload<'a> {
    touser: &'a str,
    msgtype: &'static str,
    text: WechatCustomerTextContent<'a>,
}

#[derive(Debug, Serialize)]
struct WechatCustomerTextContent<'a> {
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct WechatApiStatusResponse {
    #[serde(default)]
    errcode: i64,
    #[serde(default)]
    errmsg: String,
}

impl WechatCustomerMessageClient {
    fn new(client: reqwest::Client, api_base: String, app_id: String, app_secret: String) -> Self {
        Self {
            client,
            api_base: api_base.trim_end_matches('/').to_owned(),
            app_id,
            app_secret,
            token: Mutex::new(None),
        }
    }

    async fn access_token(&self) -> Result<String, WechatCustomerMessageError> {
        if let Some(token) = self.cached_access_token() {
            return Ok(token);
        }

        let mut url = self.url("/cgi-bin/token")?;
        url.query_pairs_mut()
            .append_pair("grant_type", "client_credential")
            .append_pair("appid", &self.app_id)
            .append_pair("secret", &self.app_secret);
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(WechatCustomerMessageError::Http)?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(WechatCustomerMessageError::Http)?;
        if !status.is_success() {
            return Err(WechatCustomerMessageError::Status {
                status,
                body_summary: wechat_api_body_summary(&body),
            });
        }
        let token_response: WechatAccessTokenResponse =
            serde_json::from_str(&body).map_err(|err| WechatCustomerMessageError::Api {
                errcode: -1,
                errmsg: format!("invalid token json: {err}"),
            })?;
        if let Some(errcode) = token_response.errcode
            && errcode != 0
        {
            return Err(WechatCustomerMessageError::Api {
                errcode,
                errmsg: token_response.errmsg.unwrap_or_default(),
            });
        }
        let access_token = token_response
            .access_token
            .filter(|token| !token.trim().is_empty())
            .ok_or(WechatCustomerMessageError::MissingAccessToken)?;
        let expires_in = token_response.expires_in.unwrap_or(7200);
        let expires_at = Instant::now()
            + Duration::from_secs(expires_in).saturating_sub(WECHAT_TOKEN_REFRESH_MARGIN);
        *self
            .token
            .lock()
            .expect("wechat token lock should not be poisoned") = Some(CachedWechatAccessToken {
            token: access_token.clone(),
            expires_at,
        });
        Ok(access_token)
    }

    fn cached_access_token(&self) -> Option<String> {
        let token = self
            .token
            .lock()
            .expect("wechat token lock should not be poisoned")
            .clone()?;
        (Instant::now() < token.expires_at).then_some(token.token)
    }

    fn url(&self, path: &str) -> Result<reqwest::Url, WechatCustomerMessageError> {
        reqwest::Url::parse(&format!("{}{}", self.api_base, path))
            .map_err(|err| WechatCustomerMessageError::InvalidUrl(err.to_string()))
    }
}

#[async_trait]
impl WechatCustomerMessenger for WechatCustomerMessageClient {
    async fn send_text(&self, touser: &str, text: &str) -> Result<(), WechatCustomerMessageError> {
        let access_token = self.access_token().await?;
        let mut url = self.url("/cgi-bin/message/custom/send")?;
        url.query_pairs_mut()
            .append_pair("access_token", &access_token);
        let payload = WechatCustomerTextPayload {
            touser,
            msgtype: "text",
            text: WechatCustomerTextContent { content: text },
        };
        let response = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .map_err(WechatCustomerMessageError::Http)?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(WechatCustomerMessageError::Http)?;
        if !status.is_success() {
            return Err(WechatCustomerMessageError::Status {
                status,
                body_summary: wechat_api_body_summary(&body),
            });
        }
        parse_wechat_api_status(&body)
    }
}

fn parse_wechat_api_status(body: &str) -> Result<(), WechatCustomerMessageError> {
    let status_response: WechatApiStatusResponse =
        serde_json::from_str(body).map_err(|err| WechatCustomerMessageError::Api {
            errcode: -1,
            errmsg: format!("invalid status json: {err}"),
        })?;
    if status_response.errcode != 0 {
        return Err(WechatCustomerMessageError::Api {
            errcode: status_response.errcode,
            errmsg: status_response.errmsg,
        });
    }
    Ok(())
}

fn wechat_api_body_summary(body: &str) -> String {
    const MAX_CHARS: usize = 200;
    let redacted = redact_urls_in_text(&redact_sensitive_text(body));
    let mut summary = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    if summary.chars().count() > MAX_CHARS {
        summary = summary.chars().take(MAX_CHARS).collect::<String>();
        summary.push('…');
    }
    summary
}

fn redact_urls_in_text(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            if token.contains("://") {
                mask_url(token)
            } else {
                token.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

    struct MockCustomerMessenger {
        sent: Mutex<Vec<(String, String)>>,
        failures: Mutex<usize>,
        fail: bool,
        sent_or_failed: Notify,
    }

    impl MockCustomerMessenger {
        fn new(fail: bool) -> Self {
            Self {
                sent: Mutex::new(Vec::new()),
                failures: Mutex::new(0),
                fail,
                sent_or_failed: Notify::new(),
            }
        }

        fn sent_messages(&self) -> Vec<(String, String)> {
            self.sent.lock().unwrap().clone()
        }

        fn failure_count(&self) -> usize {
            *self.failures.lock().unwrap()
        }

        async fn wait_for_attempt_count(&self, expected: usize) {
            loop {
                let notified = self.sent_or_failed.notified();
                let attempts = self.sent.lock().unwrap().len() + self.failure_count();
                if attempts >= expected {
                    return;
                }
                notified.await;
            }
        }
    }

    #[async_trait]
    impl WechatCustomerMessenger for MockCustomerMessenger {
        async fn send_text(
            &self,
            touser: &str,
            text: &str,
        ) -> Result<(), WechatCustomerMessageError> {
            if self.fail {
                *self.failures.lock().unwrap() += 1;
                self.sent_or_failed.notify_waiters();
                return Err(WechatCustomerMessageError::Api {
                    errcode: 45015,
                    errmsg: "response out of time limit".to_owned(),
                });
            }
            self.sent
                .lock()
                .unwrap()
                .push((touser.to_owned(), text.to_owned()));
            self.sent_or_failed.notify_waiters();
            Ok(())
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
        state_with_customer(core, None)
    }

    fn reply_timeout() -> Duration {
        WechatServiceConfig::default().reply_timeout
    }

    fn state_with_customer(
        core: Arc<MockCore>,
        customer_messenger: Option<Arc<dyn WechatCustomerMessenger>>,
    ) -> WechatServiceState {
        WechatServiceState {
            config: WechatServiceConfig {
                enabled: true,
                token: Some("token".to_owned()),
                ..WechatServiceConfig::default()
            },
            respond: RespondClient::new(core),
            dedupe: Arc::new(MessageDedupe::new(Duration::from_secs(10 * 60))),
            customer_messenger,
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
            "platform:wechat_service:account:gh_service:private:user_openid"
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

        tokio::time::advance(reply_timeout() + Duration::from_millis(1)).await;
        let (first_status, first_body) = response_body(first.await.unwrap()).await;
        assert_eq!(first_status, StatusCode::OK);
        assert!(first_body.contains(SLOW_SYNC_FALLBACK_TEXT));
        assert_eq!(core.request_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_text_message_without_customer_returns_clear_sync_hint_and_retry_is_deduped() {
        let core = Arc::new(MockCore::with_delay(Duration::from_secs(30)));
        let state = state(core.clone());
        let xml = text_xml("1234567890123456", "你好");

        let first = tokio::spawn(handle_message(
            State(state.clone()),
            Query(signed_post_query()),
            Bytes::from(xml.clone()),
        ));
        core.wait_for_request_count(1).await;
        tokio::time::advance(reply_timeout() + Duration::from_millis(1)).await;
        let (first_status, first_body) = response_body(first.await.unwrap()).await;

        assert_eq!(first_status, StatusCode::OK);
        assert!(first_body.contains("<MsgType>text</MsgType>"));
        assert!(first_body.contains(SLOW_SYNC_FALLBACK_TEXT));
        assert_eq!(core.request_count(), 1);

        let retry =
            handle_message(State(state), Query(signed_post_query()), Bytes::from(xml)).await;
        let (retry_status, retry_body) = response_body(retry).await;

        assert_eq!(retry_status, StatusCode::OK);
        assert_eq!(retry_body, "");
        assert_eq!(core.request_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_text_message_with_customer_returns_success_and_sends_async_text() {
        let core = Arc::new(MockCore::with_delay(Duration::from_secs(30)));
        let customer = Arc::new(MockCustomerMessenger::new(false));
        let state = state_with_customer(core.clone(), Some(customer.clone()));
        let xml = text_xml("async-1", "你好");

        let first = tokio::spawn(handle_message(
            State(state),
            Query(signed_post_query()),
            Bytes::from(xml),
        ));
        core.wait_for_request_count(1).await;
        tokio::time::advance(reply_timeout() + Duration::from_millis(1)).await;
        let (first_status, first_body) = response_body(first.await.unwrap()).await;

        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(first_body, WECHAT_SUCCESS_BODY);
        assert!(customer.sent_messages().is_empty());

        tokio::time::advance(Duration::from_secs(30)).await;
        customer.wait_for_attempt_count(1).await;
        assert_eq!(
            customer.sent_messages(),
            vec![("user_openid".to_owned(), "hello <wx> & user".to_owned())]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_retry_during_async_customer_follow_up_does_not_create_second_task() {
        let core = Arc::new(MockCore::with_delay(Duration::from_secs(30)));
        let customer = Arc::new(MockCustomerMessenger::new(false));
        let state = state_with_customer(core.clone(), Some(customer.clone()));
        let xml = text_xml("async-dup-1", "你好");

        let first = tokio::spawn(handle_message(
            State(state.clone()),
            Query(signed_post_query()),
            Bytes::from(xml.clone()),
        ));
        core.wait_for_request_count(1).await;
        tokio::time::advance(reply_timeout() + Duration::from_millis(1)).await;
        let (first_status, first_body) = response_body(first.await.unwrap()).await;
        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(first_body, WECHAT_SUCCESS_BODY);

        let retry =
            handle_message(State(state), Query(signed_post_query()), Bytes::from(xml)).await;
        let (retry_status, retry_body) = response_body(retry).await;
        assert_eq!(retry_status, StatusCode::OK);
        assert_eq!(retry_body, "");
        assert_eq!(core.request_count(), 1);

        tokio::time::advance(Duration::from_secs(30)).await;
        customer.wait_for_attempt_count(1).await;
        assert_eq!(customer.sent_messages().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn customer_message_error_is_not_recorded_as_success() {
        let core = Arc::new(MockCore::with_delay(Duration::from_secs(30)));
        let customer = Arc::new(MockCustomerMessenger::new(true));
        let state = state_with_customer(core.clone(), Some(customer.clone()));
        let xml = text_xml("async-fail-1", "你好");

        let first = tokio::spawn(handle_message(
            State(state),
            Query(signed_post_query()),
            Bytes::from(xml),
        ));
        core.wait_for_request_count(1).await;
        tokio::time::advance(reply_timeout() + Duration::from_millis(1)).await;
        let (first_status, first_body) = response_body(first.await.unwrap()).await;
        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(first_body, WECHAT_SUCCESS_BODY);

        tokio::time::advance(Duration::from_secs(30)).await;
        customer.wait_for_attempt_count(1).await;
        assert!(customer.sent_messages().is_empty());
        assert_eq!(customer.failure_count(), 1);
    }

    #[test]
    fn customer_message_api_errcode_is_reported_as_failure() {
        let err = parse_wechat_api_status(r#"{"errcode":40003,"errmsg":"invalid openid"}"#)
            .expect_err("non-zero errcode should fail");

        assert!(matches!(
            err,
            WechatCustomerMessageError::Api { errcode: 40003, .. }
        ));
        assert!(err.log_summary().contains("errcode=40003"));
    }

    #[test]
    fn wechat_api_body_summary_redacts_token_and_secret() {
        let summary = wechat_api_body_summary(
            r#"{"errcode":1,"access_token":"token-value","nested":{"app_secret":"secret-value"},"url":"https://api.weixin.qq.com/cgi-bin/message/custom/send?access_token=url-token&debug=1"}"#,
        );

        assert!(!summary.contains("token-value"));
        assert!(!summary.contains("secret-value"));
        assert!(!summary.contains("url-token"));
        assert!(summary.contains(r#""access_token":"<redacted>""#));
        assert!(summary.contains(r#""app_secret":"<redacted>""#));
        assert!(summary.contains("access_token=***"));
    }

    #[test]
    fn wechat_api_body_summary_redacts_query_like_plain_text() {
        let summary = wechat_api_body_summary(
            "proxy echoed https://api.weixin.qq.com/cgi-bin/token?grant_type=client_credential&secret=secret-value access_token=token-value",
        );

        assert!(!summary.contains("secret-value"));
        assert!(!summary.contains("token-value"));
        assert!(summary.contains("secret=***"));
        assert!(summary.contains("access_token=<redacted>"));
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
