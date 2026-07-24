use super::*;
use qq_maid_common::input_part::TextSource;

#[test]
fn qq_quote_hit_overwrites_contaminated_payload_with_indexed_original() {
    let mut store = RefIndex::default();
    store.insert_inbound(&group_inbound("gm-quoted", Some("REFIDX_quoted"), "测试"));
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "引用内容查看");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_quoted".to_owned()),
        text_summary: Some("测试引用内容查看".to_owned()),
        input_parts: vec![MessageInputPart::Text {
            text: "测试引用内容查看".to_owned(),
            source: Some(TextSource::QuoteContaminated),
        }],
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(quoted.lookup_found, "应明确走 RefIndex hit");
    assert_eq!(quoted.fallback_reason, None);
    assert_eq!(quoted.text_summary.as_deref(), Some("测试"));
    assert_eq!(quoted.input_parts.len(), 1);
    assert_eq!(quoted.input_parts[0].text_content(), Some("测试"));
    assert!(!quoted.input_parts.iter().any(|part| {
        matches!(
            part,
            MessageInputPart::Text {
                source: Some(TextSource::QuoteContaminated),
                ..
            }
        )
    }));
}

#[test]
fn qq_quote_miss_discards_contaminated_text_and_keeps_trusted_media() {
    let mut store = RefIndex::default();
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "引用内容查看");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_missing".to_owned()),
        text_summary: Some("测试引用内容查看".to_owned()),
        input_parts: vec![
            MessageInputPart::Text {
                text: "测试引用内容查看".to_owned(),
                source: Some(TextSource::QuoteContaminated),
            },
            MessageInputPart::image(MessageMedia {
                mime_type: Some("image/png".to_owned()),
                filename: Some("quoted.png".to_owned()),
                url: Some("https://example.test/quoted.png".to_owned()),
                ..Default::default()
            }),
        ],
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(
        quoted.lookup_found,
        "RefIndex miss 后应仅保留 payload 可信媒体"
    );
    assert_eq!(quoted.fallback_reason.as_deref(), Some("quoted_payload"));
    assert_eq!(quoted.text_summary, None);
    assert_eq!(quoted.input_parts.len(), 1);
    assert!(matches!(
        quoted.input_parts[0],
        MessageInputPart::Image { .. }
    ));
}
