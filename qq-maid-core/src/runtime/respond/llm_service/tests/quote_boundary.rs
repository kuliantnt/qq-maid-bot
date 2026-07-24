use super::*;

#[test]
fn ref_index_hit_and_payload_miss_keep_quote_and_current_once_in_provider_parts() {
    for (case, from_bot, fallback_reason) in [
        ("ref_index_hit", Some(false), None),
        (
            "ref_index_miss_payload",
            None,
            Some("quoted_payload".to_owned()),
        ),
    ] {
        let req = RespondRequest {
            purpose: RespondPurpose::Chat,
            user_text: "引用内容查看".to_owned(),
            input_parts: vec![MessageInputPart::text("引用内容查看")],
            quoted: Some(QuotedMessageContext {
                reference_id: Some("REFIDX_quoted".to_owned()),
                ref_msg_idx: Some("REFIDX_quoted".to_owned()),
                lookup_found: true,
                text_summary: Some("测试".to_owned()),
                input_parts: vec![MessageInputPart::Text {
                    text: "测试".to_owned(),
                    source: Some(TextSource::Quote),
                }],
                from_bot,
                fallback_reason,
                ..Default::default()
            }),
            ..Default::default()
        };

        let messages = build_respond_messages_for_model(&req, true);
        let provider_parts = &messages.last().unwrap().content_parts;
        let payload_text = provider_parts
            .iter()
            .map(MessageInputPart::fallback_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(payload_text.matches("测试").count(), 1, "case={case}");
        assert_eq!(
            payload_text.matches("引用内容查看").count(),
            1,
            "case={case}"
        );
        assert!(!payload_text.contains("测试引用内容查看"), "case={case}");
    }
}
