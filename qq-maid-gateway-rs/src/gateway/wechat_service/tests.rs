use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::{Router, body::to_bytes, routing::get};
use qq_maid_core::service::{
    CoreError, CoreHealthSnapshot, CoreInboundClassification, CoreInboundKind, CoreRequest,
    CoreRespondOutput, CoreResponse, CoreService, UpstreamStatusSnapshot,
};
use tokio::{net::TcpListener, sync::Notify};

use super::{
    customer::{
        WechatCustomerMessageClient, WechatCustomerMessageError, WechatCustomerMessenger,
        is_wechat_access_token_invalid_errcode, parse_wechat_api_status, wechat_api_body_summary,
    },
    *,
};

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

struct BlockingCustomerMessenger {
    attempts: Mutex<usize>,
    attempted: Notify,
}

impl Default for BlockingCustomerMessenger {
    fn default() -> Self {
        Self {
            attempts: Mutex::new(0),
            attempted: Notify::new(),
        }
    }
}

impl BlockingCustomerMessenger {
    fn attempt_count(&self) -> usize {
        *self.attempts.lock().unwrap()
    }

    async fn wait_for_attempt_count(&self, expected: usize) {
        loop {
            let notified = self.attempted.notified();
            if self.attempt_count() >= expected {
                return;
            }
            notified.await;
        }
    }
}

#[async_trait]
impl WechatCustomerMessenger for MockCustomerMessenger {
    async fn send_text(&self, touser: &str, text: &str) -> Result<(), WechatCustomerMessageError> {
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

#[async_trait]
impl WechatCustomerMessenger for BlockingCustomerMessenger {
    async fn send_text(
        &self,
        _touser: &str,
        _text: &str,
    ) -> Result<(), WechatCustomerMessageError> {
        *self.attempts.lock().unwrap() += 1;
        self.attempted.notify_waiters();
        std::future::pending::<Result<(), WechatCustomerMessageError>>().await
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
        Ok(CoreRespondOutput::Complete(Box::new(CoreResponse {
            output: Some(qq_maid_core::service::AssistantOutput::markdown(
                "hello <wx> & user",
                "**hello**",
            )),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        })))
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
    state_with_customer_and_dedupe_ttl(core, customer_messenger, Duration::from_secs(10 * 60))
}

fn state_with_customer_and_dedupe_ttl(
    core: Arc<MockCore>,
    customer_messenger: Option<Arc<dyn WechatCustomerMessenger>>,
    dedupe_ttl: Duration,
) -> WechatServiceState {
    WechatServiceState {
        config: WechatServiceConfig {
            enabled: true,
            token: Some("token".to_owned()),
            ..WechatServiceConfig::default()
        },
        respond: RespondClient::new(core),
        dedupe: Arc::new(MessageDedupe::new(dedupe_ttl)),
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
    let second = handle_message(State(state), Query(signed_post_query()), Bytes::from(xml)).await;
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

    let retry = handle_message(State(state), Query(signed_post_query()), Bytes::from(xml)).await;
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

    let retry = handle_message(State(state), Query(signed_post_query()), Bytes::from(xml)).await;
    let (retry_status, retry_body) = response_body(retry).await;
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry_body, "");
    assert_eq!(core.request_count(), 1);

    tokio::time::advance(Duration::from_secs(30)).await;
    customer.wait_for_attempt_count(1).await;
    assert_eq!(customer.sent_messages().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn blocked_customer_follow_up_does_not_hold_dedupe_reservation() {
    let core = Arc::new(MockCore::with_delay(Duration::from_secs(30)));
    let customer = Arc::new(BlockingCustomerMessenger::default());
    let state = state_with_customer_and_dedupe_ttl(
        core.clone(),
        Some(customer.clone()),
        Duration::from_millis(1),
    );
    let xml = text_xml("async-blocked-1", "你好");

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

    tokio::time::advance(Duration::from_secs(30)).await;
    customer.wait_for_attempt_count(1).await;
    assert_eq!(core.request_count(), 1);

    // 去重使用 std::time::Instant；这里等待真实时间，确认已提交的记录可按 TTL 清理。
    std::thread::sleep(Duration::from_millis(5));
    let retry = tokio::spawn(handle_message(
        State(state),
        Query(signed_post_query()),
        Bytes::from(xml),
    ));
    core.wait_for_request_count(2).await;
    tokio::time::advance(reply_timeout() + Duration::from_millis(1)).await;
    let (retry_status, retry_body) = response_body(retry.await.unwrap()).await;

    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry_body, WECHAT_SUCCESS_BODY);
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
fn customer_message_status_missing_errcode_is_failure() {
    let err = parse_wechat_api_status(r#"{}"#).expect_err("missing errcode should fail");

    assert!(matches!(
        err,
        WechatCustomerMessageError::Api { errcode: -1, .. }
    ));
    assert!(err.log_summary().contains("missing errcode"));
}

#[test]
fn customer_message_token_errcodes_are_retryable() {
    for errcode in [40001, 40014, 42001] {
        assert!(is_wechat_access_token_invalid_errcode(errcode));
    }
    assert!(!is_wechat_access_token_invalid_errcode(40003));
    assert!(!is_wechat_access_token_invalid_errcode(45015));
}

#[derive(Clone)]
struct TokenRefreshApiState {
    issued_tokens: Arc<Mutex<Vec<String>>>,
    message_tokens: Arc<Mutex<Vec<String>>>,
}

async fn token_refresh_token_handler(
    State(state): State<TokenRefreshApiState>,
) -> axum::Json<serde_json::Value> {
    let mut issued_tokens = state.issued_tokens.lock().unwrap();
    let token = if issued_tokens.is_empty() {
        "stale-token"
    } else {
        "fresh-token"
    };
    issued_tokens.push(token.to_owned());
    axum::Json(serde_json::json!({
        "access_token": token,
        "expires_in": 7200
    }))
}

async fn token_refresh_message_handler(
    State(state): State<TokenRefreshApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> axum::Json<serde_json::Value> {
    let token = query.get("access_token").cloned().unwrap_or_default();
    state.message_tokens.lock().unwrap().push(token.clone());
    if token == "stale-token" {
        return axum::Json(serde_json::json!({
            "errcode": 40001,
            "errmsg": "invalid credential"
        }));
    }
    axum::Json(serde_json::json!({
        "errcode": 0,
        "errmsg": "ok"
    }))
}

#[tokio::test]
async fn customer_message_refreshes_token_and_retries_once_when_token_invalid() {
    let api_state = TokenRefreshApiState {
        issued_tokens: Arc::new(Mutex::new(Vec::new())),
        message_tokens: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/cgi-bin/token", get(token_refresh_token_handler))
        .route(
            "/cgi-bin/message/custom/send",
            axum::routing::post(token_refresh_message_handler),
        )
        .with_state(api_state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = WechatCustomerMessageClient::new(
        reqwest::Client::new(),
        format!("http://{addr}"),
        "appid".to_owned(),
        "secret".to_owned(),
    );

    client.send_text("openid", "hello").await.unwrap();
    server.abort();

    assert_eq!(
        *api_state.issued_tokens.lock().unwrap(),
        vec!["stale-token".to_owned(), "fresh-token".to_owned()]
    );
    assert_eq!(
        *api_state.message_tokens.lock().unwrap(),
        vec!["stale-token".to_owned(), "fresh-token".to_owned()]
    );
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
