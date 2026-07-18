use super::*;
use crate::config::AppConfig;
use crate::gateway::command::GatewayCommandService;

fn command_state(core: Arc<MockCore>, aes: bool) -> WechatServiceState {
    let mut state = if aes {
        aes_state(core.clone())
    } else {
        state(core.clone())
    };
    let mut app_config = AppConfig::from_map(&HashMap::new()).unwrap();
    app_config.wechat_service = state.config.clone();
    state.commands = Some(GatewayCommandService::from_config(
        app_config,
        GatewayRuntimeStatus::new(),
        RespondClient::new(core),
    ));
    state
}

#[tokio::test]
async fn plaintext_ping_reuses_sync_xml_and_bypasses_core() {
    let core = Arc::new(MockCore::default());
    let response = handle_message(
        State(command_state(core.clone(), false)),
        Query(signed_post_query()),
        Bytes::from(text_xml("ping-plain", "/ping")),
    )
    .await;
    let (status, body) = response_body(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<MsgType>text</MsgType>"));
    assert!(body.contains("核心链路"));
    assert_eq!(core.request_count(), 0);
}

#[tokio::test]
async fn encrypted_ping_reuses_signed_encrypted_reply_and_bypasses_core() {
    let core = Arc::new(MockCore::default());
    let (query, body) = encrypted_post(&text_xml("ping-aes", "/ping"));
    let response =
        handle_message(State(command_state(core.clone(), true)), Query(query), body).await;
    let (status, body) = response_body(response).await;

    assert_eq!(status, StatusCode::OK);
    let encrypted = xml_field(&body, "Encrypt").expect("encrypted ping reply");
    let crypto = WechatMessageCrypto::new("token", TEST_WECHAT_APP_ID, TEST_ENCODING_AES_KEY)
        .expect("valid test crypto");
    let decrypted = crypto.decrypt(&encrypted).unwrap();
    assert!(decrypted.contains("核心链路"));
    assert_eq!(core.request_count(), 0);
}

#[tokio::test]
async fn similar_ping_text_still_enters_core() {
    let core = Arc::new(MockCore::default());
    let response = handle_message(
        State(command_state(core.clone(), false)),
        Query(signed_post_query()),
        Bytes::from(text_xml("ping-similar", "/pingxxx")),
    )
    .await;
    let (status, body) = response_body(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("hello &lt;wx&gt;"));
    assert_eq!(core.request_count(), 1);
}
