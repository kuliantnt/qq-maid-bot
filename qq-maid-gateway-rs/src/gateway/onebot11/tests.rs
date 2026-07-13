use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use qq_maid_core::service::{
    CoreError, CoreHealthSnapshot, CoreInboundClassification, CoreInboundKind, CoreRequest,
    CoreRespondOutput, CoreResponse, CoreService, UpstreamStatusSnapshot,
};
use serde_json::{Value, json};
use tokio::{
    net::TcpStream,
    sync::{Barrier, Notify, mpsc},
    time::Instant,
};
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
    config::AppConfig,
    gateway::{
        dedupe::MessageDedupe,
        onebot11::protocol::{ActionRequest, ActionResponse, OneBotId},
        ping::GatewayRuntimeStatus,
    },
    respond::RespondClient,
};

use super::{
    OneBotCallError, OneBotConnectionContext, OneBotSendError, OneBotSender, OneBotServerHandle,
    spawn_reverse_websocket_server,
};

const TOKEN: &str = "test-onebot-access-token";
const PATH: &str = "/onebot/v11/ws";
type ClientSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct NoopCore;

struct RecordingCore {
    requests: Mutex<Vec<CoreRequest>>,
    reply: String,
}

struct CoordinatedCore {
    requests: Mutex<Vec<CoreRequest>>,
    first_release: Option<Arc<Notify>>,
    concurrent_barrier: Option<Arc<Barrier>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    new_completed: AtomicBool,
    followup_observed_new: AtomicBool,
}

impl CoordinatedCore {
    fn first_blocked() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            first_release: Some(Arc::new(Notify::new())),
            concurrent_barrier: None,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            new_completed: AtomicBool::new(false),
            followup_observed_new: AtomicBool::new(false),
        }
    }

    fn concurrent(barrier: Arc<Barrier>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            first_release: None,
            concurrent_barrier: Some(barrier),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            new_completed: AtomicBool::new(false),
            followup_observed_new: AtomicBool::new(false),
        }
    }

    fn requests(&self) -> Vec<CoreRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn release_first(&self) {
        self.first_release.as_ref().unwrap().notify_one();
    }
}

impl RecordingCore {
    fn new(reply: &str) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            reply: reply.to_owned(),
        }
    }

    fn requests(&self) -> Vec<CoreRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl CoreService for NoopCore {
    async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
        Ok(CoreRespondOutput::Complete(Box::new(CoreResponse {
            output: None,
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
            kind: CoreInboundKind::Immediate,
        })
    }

    async fn upstream_check(&self) -> Result<(), CoreError> {
        Ok(())
    }

    fn health_snapshot(&self) -> CoreHealthSnapshot {
        CoreHealthSnapshot {
            ok: true,
            provider: "noop".to_owned(),
            model: "noop".to_owned(),
            stream: false,
            upstream: UpstreamStatusSnapshot::default(),
        }
    }
}

#[async_trait]
impl CoreService for RecordingCore {
    async fn respond(&self, request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
        self.requests.lock().unwrap().push(request);
        Ok(CoreRespondOutput::Complete(Box::new(CoreResponse {
            output: Some(qq_maid_core::service::AssistantOutput::text(
                self.reply.clone(),
            )),
            handled: Some(true),
            session_id: Some("session-1".to_owned()),
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
            kind: CoreInboundKind::Immediate,
        })
    }

    async fn upstream_check(&self) -> Result<(), CoreError> {
        Ok(())
    }

    fn health_snapshot(&self) -> CoreHealthSnapshot {
        CoreHealthSnapshot {
            ok: true,
            provider: "recording".to_owned(),
            model: "recording".to_owned(),
            stream: false,
            upstream: UpstreamStatusSnapshot::default(),
        }
    }
}

#[async_trait]
impl CoreService for CoordinatedCore {
    async fn respond(&self, request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
        let call_index = {
            let mut requests = self.requests.lock().unwrap();
            let call_index = requests.len();
            requests.push(request.clone());
            call_index
        };
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);

        if let Some(barrier) = self.concurrent_barrier.as_ref() {
            barrier.wait().await;
        }
        if call_index == 0
            && let Some(release) = self.first_release.as_ref()
        {
            release.notified().await;
        }
        if request.text.starts_with("/new") {
            self.new_completed.store(true, Ordering::SeqCst);
        } else if request.text == "新会话里的消息" {
            self.followup_observed_new
                .store(self.new_completed.load(Ordering::SeqCst), Ordering::SeqCst);
        }
        self.active.fetch_sub(1, Ordering::SeqCst);

        Ok(CoreRespondOutput::Complete(Box::new(CoreResponse {
            output: Some(qq_maid_core::service::AssistantOutput::text(format!(
                "完成:{}",
                request.text
            ))),
            handled: Some(true),
            session_id: Some(format!("session-{call_index}")),
            command: request.text.starts_with("/new").then(|| "new".to_owned()),
            diagnostics: None,
            visible_entity_snapshot: None,
        })))
    }

    async fn classify_inbound(
        &self,
        _request: CoreRequest,
    ) -> Result<CoreInboundClassification, CoreError> {
        Ok(CoreInboundClassification {
            kind: CoreInboundKind::Immediate,
        })
    }

    async fn upstream_check(&self) -> Result<(), CoreError> {
        Ok(())
    }

    fn health_snapshot(&self) -> CoreHealthSnapshot {
        CoreHealthSnapshot {
            ok: true,
            provider: "coordinated".to_owned(),
            model: "coordinated".to_owned(),
            stream: false,
            upstream: UpstreamStatusSnapshot::default(),
        }
    }
}

fn test_config() -> AppConfig {
    let mut config = AppConfig::from_map(&HashMap::new()).unwrap();
    config.onebot11.enabled = true;
    config.onebot11.bind_host = "127.0.0.1".to_owned();
    config.onebot11.bind_port = 0;
    config.onebot11.websocket_path = PATH.to_owned();
    config.onebot11.access_token = Some(TOKEN.to_owned());
    config.onebot11.request_timeout = Duration::from_millis(500);
    config.onebot11.max_message_bytes = 1024;
    config
}

async fn spawn_server() -> (OneBotServerHandle, GatewayRuntimeStatus, CancellationToken) {
    spawn_server_with_config(test_config()).await
}

async fn spawn_server_with_config(
    config: AppConfig,
) -> (OneBotServerHandle, GatewayRuntimeStatus, CancellationToken) {
    spawn_server_with_core(config, Arc::new(NoopCore)).await
}

async fn spawn_server_with_core(
    config: AppConfig,
    core: Arc<dyn CoreService>,
) -> (OneBotServerHandle, GatewayRuntimeStatus, CancellationToken) {
    let runtime = GatewayRuntimeStatus::new();
    let shutdown = CancellationToken::new();
    let handle = spawn_reverse_websocket_server(
        config,
        RespondClient::new(core),
        Arc::new(MessageDedupe::new(Duration::from_secs(10 * 60))),
        runtime.clone(),
        shutdown.clone(),
    )
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

async fn next_action_request(socket: &mut ClientSocket) -> ActionRequest {
    match socket.next().await.unwrap().unwrap() {
        Message::Text(payload) => serde_json::from_str(&payload).unwrap(),
        other => panic!("expected text action request, got {other:?}"),
    }
}

fn action_response(request: &ActionRequest, data: Value) -> ActionResponse {
    ActionResponse {
        status: "ok".to_owned(),
        retcode: 0,
        data,
        message: None,
        wording: None,
        echo: Some(request.echo.clone()),
    }
}

fn private_message_event(message_id: &str, text: &str) -> String {
    private_message_event_for(message_id, "20002", text)
}

fn private_message_event_for(message_id: &str, user_id: &str, text: &str) -> String {
    json!({
        "time": 1720000000,
        "self_id": "10001",
        "post_type": "message",
        "message_type": "private",
        "user_id": user_id,
        "message_id": message_id,
        "sender": {"nickname": "测试用户"},
        "message": [{"type": "text", "data": {"text": text}}]
    })
    .to_string()
}

async fn send_text_event(socket: &mut ClientSocket, event: String) {
    socket.send(Message::Text(event.into())).await.unwrap();
}

fn group_message_event(message_id: &str, segments: Value) -> String {
    json!({
        "time": 1720000001,
        "self_id": "10001",
        "post_type": "message",
        "message_type": "group",
        "user_id": "20002",
        "group_id": "30003",
        "message_id": message_id,
        "sender": {"card": "群成员", "role": "member"},
        "message": segments
    })
    .to_string()
}

async fn complete_action(socket: &mut ClientSocket, request: &ActionRequest, message_id: &str) {
    socket
        .send(Message::Text(
            serde_json::to_string(&action_response(request, json!({"message_id": message_id})))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn private_command_reaches_core_and_sends_one_final_action() {
    let core = Arc::new(RecordingCore::new("Core 命令结果"));
    let (handle, _runtime, shutdown) = spawn_server_with_core(test_config(), core.clone()).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let event = private_message_event("private-help-1", "/help");

    client
        .send(Message::Text(event.clone().into()))
        .await
        .unwrap();
    let action = next_action_request(&mut client).await;
    assert_eq!(action.action, "send_private_msg");
    assert_eq!(action.params["user_id"], json!(20002_u64));
    assert_eq!(
        action.params["message"],
        json!([{"type": "text", "data": {"text": "Core 命令结果"}}])
    );
    complete_action(&mut client, &action, "reply-private-1").await;
    wait_until(|| core.requests().len() == 1).await;
    let request = core.requests().remove(0);
    assert_eq!(request.text, "/help");
    assert_eq!(request.account_id.as_deref(), Some("10001"));
    assert_eq!(
        request.scope_key(),
        "platform:onebot:account:10001:private:20002"
    );

    // 同一平台消息重投不能再次调用 Core，也不能发送第二条回复。
    client.send(Message::Text(event.into())).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), client.next())
            .await
            .is_err()
    );
    assert_eq!(core.requests().len(), 1);

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn private_media_and_reply_flow_reaches_unified_core_context() {
    let core = Arc::new(RecordingCore::new("媒体结果"));
    let (handle, _runtime, shutdown) = spawn_server_with_core(test_config(), core.clone()).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();

    send_text_event(
        &mut client,
        json!({
            "time": 1720000000,
            "self_id": "10001",
            "post_type": "message",
            "message_type": "private",
            "user_id": "20002",
            "message_id": "private-media-1",
            "message": [
                {"type": "text", "data": {"text": "看图"}},
                {"type": "image", "data": {
                    "file": "photo.png",
                    "url": "https://example.test/photo.png"
                }},
                {"type": "file", "data": {
                    "file_id": "file-1",
                    "name": "report.pdf",
                    "mime_type": "application/pdf"
                }}
            ]
        })
        .to_string(),
    )
    .await;
    let first_action = next_action_request(&mut client).await;
    complete_action(&mut client, &first_action, "reply-media-1").await;
    wait_until(|| core.requests().len() == 1).await;
    let first_request = core.requests().remove(0);
    assert_eq!(first_request.input_parts.len(), 3);
    assert!(matches!(
        first_request.input_parts[1],
        qq_maid_common::input_part::MessageInputPart::Image { .. }
    ));
    assert!(matches!(
        first_request.input_parts[2],
        qq_maid_common::input_part::MessageInputPart::File { .. }
    ));

    send_text_event(
        &mut client,
        json!({
            "time": 1720000001,
            "self_id": "10001",
            "post_type": "message",
            "message_type": "private",
            "user_id": "20002",
            "message_id": "private-reply-2",
            "message": [
                {"type": "reply", "data": {"id": "reply-media-1"}},
                {"type": "text", "data": {"text": "继续"}}
            ]
        })
        .to_string(),
    )
    .await;
    let second_action = next_action_request(&mut client).await;
    complete_action(&mut client, &second_action, "reply-media-2").await;
    wait_until(|| core.requests().len() == 2).await;
    let second_request = core.requests().remove(1);
    let quoted = second_request.quoted.expect("reply should reach Core");
    assert!(quoted.lookup_found);
    assert_eq!(quoted.reference_id.as_deref(), Some("reply-media-1"));
    assert_eq!(quoted.text_summary.as_deref(), Some("媒体结果"));
    assert_eq!(quoted.from_bot, Some(true));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn same_scope_waits_for_first_core_and_sender_before_starting_second_core() {
    let core = Arc::new(CoordinatedCore::first_blocked());
    let (handle, _runtime, shutdown) = spawn_server_with_core(test_config(), core.clone()).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();

    send_text_event(&mut client, private_message_event("serial-1", "第一条")).await;
    send_text_event(&mut client, private_message_event("serial-2", "第二条")).await;
    wait_until(|| core.requests().len() == 1).await;
    assert_eq!(core.requests()[0].text, "第一条");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), async {
            while core.requests().len() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err(),
        "第一条 Core 未释放前，第二条不得进入 Core"
    );

    core.release_first();
    let first_action = next_action_request(&mut client).await;
    assert_eq!(
        first_action.params["message"][0]["data"]["text"],
        "完成:第一条"
    );
    assert_eq!(
        core.requests().len(),
        1,
        "第一条 sender echo 前仍需保持串行"
    );
    complete_action(&mut client, &first_action, "serial-reply-1").await;

    wait_until(|| core.requests().len() == 2).await;
    assert_eq!(core.requests()[1].text, "第二条");
    let second_action = next_action_request(&mut client).await;
    complete_action(&mut client, &second_action, "serial-reply-2").await;

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn different_private_scopes_reach_core_concurrently() {
    let barrier = Arc::new(Barrier::new(3));
    let core = Arc::new(CoordinatedCore::concurrent(barrier.clone()));
    let (handle, _runtime, shutdown) = spawn_server_with_core(test_config(), core.clone()).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();

    send_text_event(
        &mut client,
        private_message_event_for("parallel-1", "20002", "会话一"),
    )
    .await;
    send_text_event(
        &mut client,
        private_message_event_for("parallel-2", "20003", "会话二"),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(1), barrier.wait())
        .await
        .expect("两个不同 scope 的 Core 请求应同时到达 barrier");
    assert_eq!(core.max_active.load(Ordering::SeqCst), 2);

    let first_action = next_action_request(&mut client).await;
    let second_action = next_action_request(&mut client).await;
    complete_action(&mut client, &first_action, "parallel-reply-1").await;
    complete_action(&mut client, &second_action, "parallel-reply-2").await;

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn new_command_finishes_before_followup_in_same_onebot_scope() {
    let core = Arc::new(CoordinatedCore::first_blocked());
    let (handle, _runtime, shutdown) = spawn_server_with_core(test_config(), core.clone()).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();

    send_text_event(
        &mut client,
        private_message_event("new-order-1", "/new 新话题"),
    )
    .await;
    send_text_event(
        &mut client,
        private_message_event("new-order-2", "新会话里的消息"),
    )
    .await;
    wait_until(|| core.requests().len() == 1).await;
    core.release_first();

    let new_action = next_action_request(&mut client).await;
    complete_action(&mut client, &new_action, "new-order-reply-1").await;
    wait_until(|| core.requests().len() == 2).await;
    assert!(
        core.followup_observed_new.load(Ordering::SeqCst),
        "后续消息必须在 /new 完成状态更新后进入 Core"
    );
    let followup_action = next_action_request(&mut client).await;
    complete_action(&mut client, &followup_action, "new-order-reply-2").await;

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn group_message_only_reaches_core_after_at_current_bot() {
    let core = Arc::new(RecordingCore::new("群聊结果"));
    let (handle, _runtime, shutdown) = spawn_server_with_core(test_config(), core.clone()).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();

    client
        .send(Message::Text(
            group_message_event(
                "group-untriggered",
                json!([{"type": "text", "data": {"text": "路过"}}]),
            )
            .into(),
        ))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), client.next())
            .await
            .is_err()
    );
    assert!(core.requests().is_empty());

    client
        .send(Message::Text(
            group_message_event(
                "group-at-1",
                json!([
                    {"type": "at", "data": {"qq": "10001"}},
                    {"type": "text", "data": {"text": " 请帮忙"}}
                ]),
            )
            .into(),
        ))
        .await
        .unwrap();
    let action = next_action_request(&mut client).await;
    assert_eq!(action.action, "send_group_msg");
    assert_eq!(action.params["group_id"], json!(30003_u64));
    complete_action(&mut client, &action, "reply-group-1").await;
    wait_until(|| core.requests().len() == 1).await;
    let request = core.requests().remove(0);
    assert_eq!(request.text, " 请帮忙");
    assert_eq!(
        request.scope_key(),
        "platform:onebot:account:10001:group:30003"
    );

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
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
    let request = next_action_request(&mut client).await;
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
async fn concurrent_text_sends_use_unique_echo_and_complete_out_of_order() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let private_sender = sender.clone();
    let private = tokio::spawn(async move {
        private_sender
            .send_private_text("20002", "private body")
            .await
    });
    let group = tokio::spawn(async move { sender.send_group_text("30003", "group body").await });

    let first = next_action_request(&mut client).await;
    let second = next_action_request(&mut client).await;
    assert_ne!(first.echo, second.echo);
    for request in [&first, &second] {
        let (target_key, target_id, body) = match request.action.as_str() {
            "send_private_msg" => ("user_id", 20002_u64, "private body"),
            "send_group_msg" => ("group_id", 30003_u64, "group body"),
            action => panic!("unexpected action {action}"),
        };
        assert_eq!(request.params[target_key], json!(target_id));
        assert!(request.params[target_key].is_u64());
        assert_eq!(
            request.params["message"],
            json!([{"type": "text", "data": {"text": body}}])
        );
    }

    for request in [&second, &first] {
        let message_id = if request.action == "send_private_msg" {
            json!(90001)
        } else {
            json!("90002")
        };
        client
            .send(Message::Text(
                serde_json::to_string(&action_response(request, json!({"message_id": message_id})))
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
    }

    assert_eq!(private.await.unwrap().unwrap().message_id, "90001");
    assert_eq!(group.await.unwrap().unwrap().message_id, "90002");

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn large_target_id_is_sent_as_exact_json_number() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let send = tokio::spawn(async move {
        sender
            .send_private_text("18446744073709551615", "large target")
            .await
    });

    let request = next_action_request(&mut client).await;
    assert_eq!(request.params["user_id"], json!(u64::MAX));
    assert_eq!(request.params["user_id"].as_u64(), Some(u64::MAX));
    client
        .send(Message::Text(
            serde_json::to_string(&action_response(&request, json!({"message_id": "90003"})))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(send.await.unwrap().unwrap().message_id, "90003");

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn invalid_target_id_returns_error_without_writing_action() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());

    assert!(matches!(
        sender.send_group_text("not-a-number", "invalid").await,
        Err(OneBotSendError::InvalidTargetId)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), client.next())
            .await
            .is_err(),
        "无效目标 ID 不应向 WebSocket 写入 action"
    );

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unknown_echo_does_not_complete_pending_send() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let mut send = tokio::spawn(async move { sender.send_private_text("20002", "hello").await });
    let request = next_action_request(&mut client).await;
    let mut unknown = action_response(&request, json!({"message_id": 1}));
    unknown.echo = Some(crate::gateway::onebot11::protocol::Echo(json!("unknown")));
    client
        .send(Message::Text(
            serde_json::to_string(&unknown).unwrap().into(),
        ))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut send)
            .await
            .is_err()
    );

    client
        .send(Message::Text(
            serde_json::to_string(&action_response(&request, json!({"message_id": "real-id"})))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(send.await.unwrap().unwrap().message_id, "real-id");

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn sender_propagates_retcode_failure_without_forging_success() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let send = tokio::spawn(async move { sender.send_group_text("30003", "hello").await });
    let request = next_action_request(&mut client).await;
    client
        .send(Message::Text(
            json!({
                "status": "failed",
                "retcode": 1404,
                "data": null,
                "message": "failed remotely",
                "wording": "target unavailable",
                "echo": request.echo
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    assert!(matches!(
        send.await.unwrap(),
        Err(OneBotSendError::Rejected {
            retcode: 1404,
            remote_message_present: true,
            ..
        })
    ));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn sender_returns_explicit_offline_and_disconnect_errors() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let sender = OneBotSender::new(handle.connection.clone());
    assert!(matches!(
        sender.send_private_text("20002", "offline").await,
        Err(OneBotSendError::Transport(OneBotCallError::NotConnected))
    ));

    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let pending_sender = sender.clone();
    let pending = tokio::spawn(async move {
        pending_sender
            .send_private_text("20002", "disconnect")
            .await
    });
    let _request = next_action_request(&mut client).await;
    client.close(None).await.unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(OneBotSendError::Transport(
            OneBotCallError::ConnectionClosed
        ))
    ));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn sender_propagates_action_timeout() {
    let mut config = test_config();
    config.onebot11.request_timeout = Duration::from_millis(50);
    let (handle, _runtime, shutdown) = spawn_server_with_config(config).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let send = tokio::spawn(async move { sender.send_private_text("20002", "timeout").await });
    let _request = next_action_request(&mut client).await;

    assert!(matches!(
        send.await.unwrap(),
        Err(OneBotSendError::Transport(OneBotCallError::Timeout))
    ));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unregistered_connection_cannot_complete_active_request_with_forged_echo() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut active = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let mut unregistered = connect(handle.local_addr, PATH, TOKEN, None).await.unwrap();

    let connection = handle.connection.clone();
    let mut call = tokio::spawn(async move { connection.call("get_status", json!({})).await });
    let request = next_action_request(&mut active).await;
    unregistered
        .send(Message::Text(
            serde_json::to_string(&action_response(&request, json!({"forged": true})))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();

    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut call)
            .await
            .is_err(),
        "未注册连接伪造的 echo 不应完成活动请求"
    );
    active
        .send(Message::Text(
            serde_json::to_string(&action_response(&request, json!({"forged": false})))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    let response = call.await.unwrap().unwrap();
    assert_eq!(response.data, json!({"forged": false}));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn stale_generation_response_cannot_complete_current_request() {
    let context = OneBotConnectionContext::new(Duration::from_millis(500));
    let self_id = OneBotId::new("10001").unwrap();
    let (old_outbound, mut old_requests) = mpsc::channel(1);
    let old_registration = context
        .register(self_id.clone(), old_outbound, CancellationToken::new())
        .unwrap();
    let old_context = context.clone();
    let old_call = tokio::spawn(async move { old_context.call("get_status", json!({})).await });
    let _old_request: ActionRequest =
        serde_json::from_str(&old_requests.recv().await.unwrap()).unwrap();

    let (current_outbound, mut current_requests) = mpsc::channel(1);
    let current_registration = context
        .register(self_id, current_outbound, CancellationToken::new())
        .unwrap();
    assert!(matches!(
        old_call.await.unwrap(),
        Err(OneBotCallError::ConnectionClosed)
    ));

    let current_context = context.clone();
    let mut current_call =
        tokio::spawn(async move { current_context.call("get_status", json!({})).await });
    let current_request: ActionRequest =
        serde_json::from_str(&current_requests.recv().await.unwrap()).unwrap();
    context.dispatch_response(
        old_registration.generation,
        action_response(&current_request, json!({"generation": "old"})),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut current_call)
            .await
            .is_err(),
        "旧 generation 的 response 不应完成当前请求"
    );

    context.dispatch_response(
        current_registration.generation,
        action_response(&current_request, json!({"generation": "current"})),
    );
    let response = current_call.await.unwrap().unwrap();
    assert_eq!(response.data, json!({"generation": "current"}));
}

#[tokio::test]
async fn outbound_queue_wait_is_included_in_request_timeout() {
    let request_timeout = Duration::from_millis(50);
    let context = OneBotConnectionContext::new(request_timeout);
    let (outbound, _requests) = mpsc::channel(1);
    outbound.send("occupied".to_owned()).await.unwrap();
    context
        .register(
            OneBotId::new("10001").unwrap(),
            outbound,
            CancellationToken::new(),
        )
        .unwrap();

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        context.call("get_status", json!({})),
    )
    .await
    .expect("call 应由 request_timeout 自行结束，而不是持续阻塞在 outbound 队列");

    assert!(matches!(result, Err(OneBotCallError::Timeout)));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test]
async fn heartbeat_is_optional_until_client_sends_first_heartbeat() {
    let mut config = test_config();
    config.onebot11.request_timeout = Duration::from_millis(50);
    let (handle, runtime, shutdown) = spawn_server_with_config(config).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(runtime.snapshot().onebot_connected);
    let connection = handle.connection.clone();
    let call = tokio::spawn(async move { connection.call("get_status", json!({})).await });
    assert_eq!(next_action_request(&mut client).await.action, "get_status");
    assert!(matches!(call.await.unwrap(), Err(OneBotCallError::Timeout)));

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
    config.onebot11.request_timeout = Duration::from_millis(100);
    let (handle, runtime, shutdown) = spawn_server_with_config(config).await;
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
    config.onebot11.request_timeout = Duration::from_millis(100);
    let (handle, runtime, shutdown) = spawn_server_with_config(config).await;
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
