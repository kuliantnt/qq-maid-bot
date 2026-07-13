use std::{net::SocketAddr, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, StatusCode},
    },
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::OneBot11Config,
    gateway::{onebot11::protocol::ActionRequest, ping::GatewayRuntimeStatus},
};

use super::{OneBotCallError, OneBotServerHandle, spawn_reverse_websocket_server};

const TOKEN: &str = "test-onebot-access-token";
const PATH: &str = "/onebot/v11/ws";
type ClientSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn test_config() -> OneBot11Config {
    OneBot11Config {
        enabled: true,
        bind_host: "127.0.0.1".to_owned(),
        bind_port: 0,
        websocket_path: PATH.to_owned(),
        access_token: Some(TOKEN.to_owned()),
        request_timeout: Duration::from_millis(500),
        max_message_bytes: 1024,
    }
}

async fn spawn_server() -> (OneBotServerHandle, GatewayRuntimeStatus, CancellationToken) {
    let runtime = GatewayRuntimeStatus::new();
    let shutdown = CancellationToken::new();
    let handle = spawn_reverse_websocket_server(test_config(), runtime.clone(), shutdown.clone())
        .await
        .unwrap();
    (handle, runtime, shutdown)
}

async fn connect(
    addr: SocketAddr,
    path: &str,
    token: &str,
    self_id: Option<&str>,
) -> Result<ClientSocket, tokio_tungstenite::tungstenite::Error> {
    let mut request = format!("ws://{addr}{path}").into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    if let Some(self_id) = self_id {
        request
            .headers_mut()
            .insert("x-self-id", HeaderValue::from_str(self_id).unwrap());
    }
    connect_async(request).await.map(|(socket, _)| socket)
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn next_close(socket: &mut ClientSocket) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(message) = socket.next().await {
            match message {
                Ok(Message::Close(frame)) => {
                    return frame.map(|frame| frame.reason.to_string());
                }
                Err(_) => return None,
                _ => {}
            }
        }
        None
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn rejects_wrong_token_and_wrong_path_without_stopping_listener() {
    let (handle, runtime, shutdown) = spawn_server().await;

    let auth_error = connect(handle.local_addr, PATH, "wrong-token", Some("10001"))
        .await
        .unwrap_err();
    assert!(matches!(
        auth_error,
        tokio_tungstenite::tungstenite::Error::Http(response)
            if response.status() == StatusCode::UNAUTHORIZED
    ));
    let path_error = connect(handle.local_addr, "/wrong", TOKEN, Some("10001"))
        .await
        .unwrap_err();
    assert!(matches!(
        path_error,
        tokio_tungstenite::tungstenite::Error::Http(response)
            if response.status() == StatusCode::NOT_FOUND
    ));

    assert!(runtime.snapshot().onebot_listening);
    let _valid = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().onebot_connected).await;

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
    assert!(!runtime.snapshot().onebot_listening);
    assert!(!runtime.snapshot().onebot_connected);
}

#[tokio::test]
async fn lifecycle_heartbeat_and_api_response_share_one_connection() {
    let (handle, runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, None).await.unwrap();
    client
        .send(Message::Text(
            json!({
                "time": 1,
                "self_id": 123456789012345678_u64,
                "post_type": "meta_event",
                "meta_event_type": "lifecycle",
                "sub_type": "connect"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    client
        .send(Message::Text(
            json!({
                "time": 2,
                "self_id": "123456789012345678",
                "post_type": "meta_event",
                "meta_event_type": "heartbeat",
                "interval": 1000,
                "status": {"online": true}
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().last_onebot_heartbeat_at.is_some()).await;
    let snapshot = runtime.snapshot();
    assert!(snapshot.onebot_connected);
    assert_eq!(
        snapshot.onebot_self_id_summary.as_deref(),
        Some("******345678")
    );

    let connection = handle.connection.clone();
    let call = tokio::spawn(async move { connection.call("get_status", json!({})).await });
    let request = match client.next().await.unwrap().unwrap() {
        Message::Text(payload) => serde_json::from_str::<ActionRequest>(&payload).unwrap(),
        other => panic!("expected text action request, got {other:?}"),
    };
    client
        .send(Message::Text(
            json!({
                "status": "ok",
                "retcode": 0,
                "data": {"online": true},
                "echo": request.echo
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let response = call.await.unwrap().unwrap();
    assert_eq!(response.data, json!({"online": true}));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn same_account_replaces_old_connection_and_different_account_is_rejected() {
    let (handle, runtime, shutdown) = spawn_server().await;
    let mut first = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().onebot_connected).await;

    let connection = handle.connection.clone();
    let pending_call = tokio::spawn(async move { connection.call("get_status", json!({})).await });
    assert!(matches!(
        first.next().await.unwrap().unwrap(),
        Message::Text(_)
    ));
    let _second = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    assert!(matches!(
        pending_call.await.unwrap(),
        Err(OneBotCallError::ConnectionClosed)
    ));
    assert_eq!(
        next_close(&mut first).await.as_deref(),
        Some("replaced by newer connection")
    );
    wait_until(|| runtime.snapshot().last_onebot_replaced_at.is_some()).await;
    assert!(runtime.snapshot().onebot_connected);

    let mut mismatch = connect(handle.local_addr, PATH, TOKEN, Some("20002"))
        .await
        .unwrap();
    assert_eq!(
        next_close(&mut mismatch).await.as_deref(),
        Some("different self_id is not allowed")
    );
    assert!(runtime.snapshot().onebot_connected);
    assert_eq!(
        handle.connection.connected_self_id().unwrap().as_str(),
        "10001"
    );

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn invalid_json_is_isolated_and_client_can_reconnect() {
    let (handle, runtime, shutdown) = spawn_server().await;
    let mut invalid = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    invalid
        .send(Message::Text("{not-json".into()))
        .await
        .unwrap();
    assert_eq!(
        next_close(&mut invalid).await.as_deref(),
        Some("invalid JSON")
    );
    wait_until(|| !runtime.snapshot().onebot_connected).await;
    assert_eq!(
        runtime.snapshot().last_onebot_disconnect_summary.as_deref(),
        Some("invalid JSON")
    );
    assert!(runtime.snapshot().onebot_listening);

    let _reconnected = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().onebot_connected).await;

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn heartbeat_timeout_closes_only_the_client() {
    let mut config = test_config();
    config.request_timeout = Duration::from_millis(100);
    let runtime = GatewayRuntimeStatus::new();
    let shutdown = CancellationToken::new();
    let handle = spawn_reverse_websocket_server(config, runtime.clone(), shutdown.clone())
        .await
        .unwrap();
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    client
        .send(Message::Text(
            json!({
                "self_id": "10001",
                "post_type": "meta_event",
                "meta_event_type": "heartbeat",
                "interval": 10,
                "status": {"online": true}
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        next_close(&mut client).await.as_deref(),
        Some("heartbeat timed out")
    );
    assert!(runtime.snapshot().onebot_listening);

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn api_request_timeout_does_not_drop_healthy_connection() {
    let mut config = test_config();
    config.request_timeout = Duration::from_millis(100);
    let runtime = GatewayRuntimeStatus::new();
    let shutdown = CancellationToken::new();
    let handle = spawn_reverse_websocket_server(config, runtime.clone(), shutdown.clone())
        .await
        .unwrap();
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().onebot_connected).await;

    let connection = handle.connection.clone();
    let call = tokio::spawn(async move { connection.call("get_status", json!({})).await });
    assert!(matches!(
        client.next().await.unwrap().unwrap(),
        Message::Text(_)
    ));
    assert!(matches!(call.await.unwrap(), Err(OneBotCallError::Timeout)));
    assert!(runtime.snapshot().onebot_connected);

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn oversized_message_is_rejected_without_stopping_listener() {
    let (handle, runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    client
        .send(Message::Text("x".repeat(2048).into()))
        .await
        .unwrap();
    let _ = next_close(&mut client).await;
    wait_until(|| !runtime.snapshot().onebot_connected).await;
    assert_eq!(
        runtime.snapshot().last_onebot_disconnect_summary.as_deref(),
        Some("message too large")
    );
    assert!(runtime.snapshot().onebot_listening);

    let _reconnected = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().onebot_connected).await;

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[test]
fn test_payload_helpers_do_not_require_float_id_round_trip() {
    let value: Value = serde_json::from_str("123456789012345678").unwrap();
    assert!(value.is_u64());
}
