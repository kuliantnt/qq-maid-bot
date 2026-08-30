use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::{
    Router,
    extract::{Query, State},
    routing::get,
};
use tokio::net::TcpListener;

use super::super::{
    customer::{
        WECHAT_CUSTOMER_TEXT_MAX_BYTES, WechatCustomerMessageClient, WechatCustomerMessageError,
        WechatCustomerMessenger, is_wechat_access_token_invalid_errcode, parse_wechat_api_status,
        wechat_api_body_summary,
    },
    send_customer_text_reply,
};

struct RecordingCustomerMessenger {
    attempts: Mutex<Vec<String>>,
    fail_on_call: Option<usize>,
}

impl RecordingCustomerMessenger {
    fn new(fail_on_call: Option<usize>) -> Self {
        Self {
            attempts: Mutex::new(Vec::new()),
            fail_on_call,
        }
    }

    fn attempts(&self) -> Vec<String> {
        self.attempts.lock().unwrap().clone()
    }
}

#[async_trait]
impl WechatCustomerMessenger for RecordingCustomerMessenger {
    async fn send_text(&self, _touser: &str, text: &str) -> Result<(), WechatCustomerMessageError> {
        let call_index = {
            let mut attempts = self.attempts.lock().unwrap();
            attempts.push(text.to_owned());
            attempts.len()
        };

        if self.fail_on_call == Some(call_index) {
            return Err(WechatCustomerMessageError::Api {
                errcode: 45002,
                errmsg: "content size out of limit".to_owned(),
            });
        }
        Ok(())
    }
}

fn assert_customer_chunks_reconstruct(attempts: &[String], original: &str) {
    assert!(attempts.iter().all(|chunk| {
        chunk.len() <= WECHAT_CUSTOMER_TEXT_MAX_BYTES
            && std::str::from_utf8(chunk.as_bytes()).is_ok()
    }));
    let reconstructed = attempts.iter().map(String::as_str).collect::<String>();
    assert_eq!(reconstructed, original);
}

#[tokio::test]
async fn customer_short_text_is_sent_once_without_chunking() {
    let messenger = RecordingCustomerMessenger::new(None);
    let reply = "短回复 😊";

    let chunk_count = send_customer_text_reply(&messenger, "message-short", "openid", reply)
        .await
        .unwrap();

    assert_eq!(chunk_count, 1);
    assert_eq!(messenger.attempts(), vec![reply.to_owned()]);
}

#[tokio::test]
async fn customer_text_exactly_at_byte_limit_is_sent_once() {
    let messenger = RecordingCustomerMessenger::new(None);
    let reply = "a".repeat(WECHAT_CUSTOMER_TEXT_MAX_BYTES);

    let chunk_count = send_customer_text_reply(&messenger, "message-exact", "openid", &reply)
        .await
        .unwrap();

    assert_eq!(chunk_count, 1);
    assert_eq!(messenger.attempts(), vec![reply]);
}

#[tokio::test]
async fn customer_text_one_byte_over_limit_is_split() {
    let messenger = RecordingCustomerMessenger::new(None);
    let reply = "a".repeat(WECHAT_CUSTOMER_TEXT_MAX_BYTES + 1);

    let chunk_count = send_customer_text_reply(&messenger, "message-over", "openid", &reply)
        .await
        .unwrap();
    let attempts = messenger.attempts();

    assert_eq!(chunk_count, 2);
    assert_eq!(attempts[0].len(), WECHAT_CUSTOMER_TEXT_MAX_BYTES);
    assert_eq!(attempts[1], "a");
    assert_customer_chunks_reconstruct(&attempts, &reply);
}

#[tokio::test]
async fn customer_long_ascii_chunks_preserve_order_and_byte_limit() {
    let messenger = RecordingCustomerMessenger::new(None);
    let reply = format!(
        "{}{}{}",
        "a".repeat(WECHAT_CUSTOMER_TEXT_MAX_BYTES),
        "b".repeat(WECHAT_CUSTOMER_TEXT_MAX_BYTES),
        "c".repeat(17)
    );

    let chunk_count = send_customer_text_reply(&messenger, "message-ascii", "openid", &reply)
        .await
        .unwrap();
    let attempts = messenger.attempts();

    assert_eq!(chunk_count, 3);
    assert_eq!(attempts[0], "a".repeat(WECHAT_CUSTOMER_TEXT_MAX_BYTES));
    assert_eq!(attempts[1], "b".repeat(WECHAT_CUSTOMER_TEXT_MAX_BYTES));
    assert_eq!(attempts[2], "c".repeat(17));
    assert_customer_chunks_reconstruct(&attempts, &reply);
}

#[tokio::test]
async fn customer_long_chinese_text_splits_by_utf8_bytes() {
    let messenger = RecordingCustomerMessenger::new(None);
    // 683 个汉字占 2049 个 UTF-8 字节，字符数却只有 683，正好覆盖单位差异。
    let reply = "你".repeat(683);

    let chunk_count = send_customer_text_reply(&messenger, "message-chinese", "openid", &reply)
        .await
        .unwrap();
    let attempts = messenger.attempts();

    assert_eq!(chunk_count, 2);
    assert_eq!(attempts[0].chars().count(), 682);
    assert_eq!(attempts[0].len(), 2046);
    assert_eq!(attempts[1], "你");
    assert_customer_chunks_reconstruct(&attempts, &reply);
}

#[tokio::test]
async fn customer_emoji_at_chunk_boundary_stays_intact() {
    let messenger = RecordingCustomerMessenger::new(None);
    let reply = format!("{}😊z", "a".repeat(2044));

    let chunk_count = send_customer_text_reply(&messenger, "message-emoji", "openid", &reply)
        .await
        .unwrap();
    let attempts = messenger.attempts();

    assert_eq!(chunk_count, 2);
    assert_eq!(attempts[0].len(), WECHAT_CUSTOMER_TEXT_MAX_BYTES);
    assert!(attempts[0].ends_with('😊'));
    assert_eq!(attempts[1], "z");
    assert_customer_chunks_reconstruct(&attempts, &reply);
}

#[tokio::test]
async fn customer_chunk_failure_stops_follow_up_sends_and_returns_error() {
    let messenger = RecordingCustomerMessenger::new(Some(2));
    let reply = "x".repeat(WECHAT_CUSTOMER_TEXT_MAX_BYTES * 2 + 1);

    let error = send_customer_text_reply(&messenger, "message-fail", "openid", &reply)
        .await
        .expect_err("failed customer message chunk must be reported");
    let attempts = messenger.attempts();

    assert!(matches!(
        error,
        WechatCustomerMessageError::Api { errcode: 45002, .. }
    ));
    assert_eq!(attempts.len(), 2, "the third chunk must not be sent");
    assert_eq!(attempts[0].len(), WECHAT_CUSTOMER_TEXT_MAX_BYTES);
    assert_eq!(attempts[1].len(), WECHAT_CUSTOMER_TEXT_MAX_BYTES);
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
        qq_maid_common::http_client::client(),
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
