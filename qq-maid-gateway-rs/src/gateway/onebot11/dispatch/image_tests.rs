use std::sync::Arc;

use qq_maid_common::output_part::{AssistantOutput, OutputMedia, OutputPart};
use qq_maid_core::service::CoreResponse;

use super::OneBotCoreTransport;
use super::tests::{FakeSender, dispatcher, inbound};

fn mixed_response() -> Box<CoreResponse> {
    Box::new(CoreResponse {
        output: Some(AssistantOutput {
            text_fallback: String::new(),
            markdown: None,
            parts: vec![
                OutputPart::Text {
                    text: "说明".to_owned(),
                },
                OutputPart::Image {
                    media: OutputMedia {
                        data_base64: Some("aGVsbG8=".to_owned()),
                        fallback_text: Some("图片发送失败".to_owned()),
                        ..OutputMedia::default()
                    },
                },
            ],
        }),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
        delivery_hint: None,
    })
}

fn image_response() -> Box<CoreResponse> {
    Box::new(CoreResponse {
        output: Some(AssistantOutput {
            text_fallback: "图片发送失败".to_owned(),
            markdown: None,
            parts: vec![OutputPart::Image {
                media: OutputMedia {
                    data_base64: Some("aGVsbG8=".to_owned()),
                    fallback_text: Some("图片发送失败".to_owned()),
                    ..OutputMedia::default()
                },
            }],
        }),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
        delivery_hint: None,
    })
}

#[tokio::test]
async fn group_text_and_image_are_sent_in_order() {
    let sender = Arc::new(FakeSender::default());
    let (dispatcher, _) = dispatcher(
        vec![Ok(OneBotCoreTransport::Complete(mixed_response()))],
        sender.clone(),
    );

    let mut message = inbound("mixed", true);
    message.mentioned_bot = false;
    dispatcher.dispatch(message).await.unwrap();

    assert_eq!(
        sender.sent.lock().unwrap().as_slice(),
        &[
            ("group".to_owned(), "30003".to_owned(), "说明".to_owned()),
            (
                "group_image".to_owned(),
                "30003".to_owned(),
                "[image]".to_owned(),
            ),
        ]
    );
}

#[tokio::test]
async fn failed_image_send_falls_back_without_repeating_prior_text() {
    let sender = Arc::new(FakeSender {
        fail_images: true,
        ..FakeSender::default()
    });
    let (dispatcher, _) = dispatcher(
        vec![Ok(OneBotCoreTransport::Complete(mixed_response()))],
        sender.clone(),
    );

    dispatcher
        .dispatch(inbound("fallback", false))
        .await
        .unwrap();

    assert_eq!(
        sender.sent.lock().unwrap().as_slice(),
        &[
            ("private".to_owned(), "20002".to_owned(), "说明".to_owned()),
            (
                "private".to_owned(),
                "20002".to_owned(),
                "图片发送失败".to_owned(),
            ),
        ]
    );
}

#[tokio::test]
async fn successful_group_image_preserves_native_actor_mention() {
    let sender = Arc::new(FakeSender::default());
    let (dispatcher, _) = dispatcher(
        vec![Ok(OneBotCoreTransport::Complete(image_response()))],
        sender.clone(),
    );

    dispatcher
        .dispatch(inbound("image-at-mention", true))
        .await
        .unwrap();

    assert_eq!(
        sender.sent.lock().unwrap().as_slice(),
        &[(
            "group_image_with_mentions".to_owned(),
            "30003".to_owned(),
            "20002|[image]".to_owned(),
        )]
    );
}
