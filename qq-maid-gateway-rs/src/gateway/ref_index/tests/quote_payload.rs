use super::*;

#[test]
fn qq_quote_hit_uses_original_message_saved_by_current_msg_idx() {
    let mut store = RefIndex::default();
    store.insert_inbound(&group_inbound("gm-quoted", Some("REFIDX_quoted"), "测试"));
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "引用内容查看");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_quoted".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.fallback_reason, None);
    assert_eq!(quoted.text_summary.as_deref(), Some("测试"));
    assert_eq!(quoted.input_parts.len(), 1);
    assert_eq!(quoted.input_parts[0].text_content(), Some("测试"));
}

#[test]
fn qq_quote_miss_keeps_metadata_without_unconfirmed_text() {
    let mut store = RefIndex::default();
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "引用内容查看");
    current.quoted = Some(QuotedMessageContext {
        reference_id: Some("REFIDX_missing".to_owned()),
        current_msg_idx: Some("REFIDX_current".to_owned()),
        ref_msg_idx: Some("REFIDX_missing".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(!quoted.lookup_found);
    assert_eq!(quoted.reference_id.as_deref(), Some("REFIDX_missing"));
    assert_eq!(quoted.current_msg_idx.as_deref(), Some("REFIDX_current"));
    assert_eq!(quoted.ref_msg_idx.as_deref(), Some("REFIDX_missing"));
    assert_eq!(quoted.fallback_reason.as_deref(), Some("ref_index_miss"));
    assert_eq!(quoted.text_summary, None);
    assert!(quoted.input_parts.is_empty());
}

#[test]
fn qq_quote_hit_uses_ref_index_text_over_payload_fallback() {
    // RefIndex 命中时使用索引中保存的原文覆盖事件 payload。
    let mut store = RefIndex::default();
    store.insert_inbound(&group_inbound(
        "gm-original",
        Some("REFIDX_quoted"),
        "索引中保存的原文",
    ));
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "引用内容查看");
    // 模拟事件 payload 携带了不完整或展示用文本。
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_quoted".to_owned()),
        text_summary: Some("payload 展示文本".to_owned()),
        input_parts: vec![MessageInputPart::text("payload 展示文本")],
        lookup_found: true,
        fallback_reason: Some("pending_ref_index_lookup".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.fallback_reason, None);
    // RefIndex 原文覆盖 payload。
    assert_eq!(quoted.text_summary.as_deref(), Some("索引中保存的原文"));
    assert_eq!(
        quoted.input_parts[0].text_content(),
        Some("索引中保存的原文")
    );
}

#[test]
fn qq_quote_miss_with_payload_fallback_keeps_event_content() {
    // RefIndex miss 但事件携带 msg_elements payload 时，使用 payload 内容。
    let mut store = RefIndex::default();
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "引用内容查看");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_missing".to_owned()),
        text_summary: Some("事件 payload 原文".to_owned()),
        input_parts: vec![MessageInputPart::text("事件 payload 原文")],
        lookup_found: true,
        fallback_reason: Some("pending_ref_index_lookup".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.fallback_reason.as_deref(), Some("quoted_payload"));
    assert_eq!(quoted.text_summary.as_deref(), Some("事件 payload 原文"));
}

#[test]
fn qq_current_message_is_not_saved_under_ref_msg_idx() {
    let mut store = RefIndex::default();
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "当前正文");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_quoted".to_owned()),
        ..Default::default()
    });
    store.insert_inbound(&current);

    let mut quote_current = group_inbound("gm-next", Some("REFIDX_next"), "继续");
    quote_current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_current".to_owned()),
        ..Default::default()
    });
    store.enrich_inbound(&mut quote_current);
    assert_eq!(
        quote_current
            .quoted
            .as_ref()
            .and_then(|quoted| quoted.text_summary.as_deref()),
        Some("当前正文")
    );

    let mut quote_referenced = group_inbound("gm-other", Some("REFIDX_other"), "继续");
    quote_referenced.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_quoted".to_owned()),
        ..Default::default()
    });
    store.enrich_inbound(&mut quote_referenced);
    assert!(!quote_referenced.quoted.as_ref().unwrap().lookup_found);
}

/// ref_msg_idx 缺失但 msg_elements 携带有效 payload 时，
/// 标记为 quoted_payload_without_reference_id，引用正文和媒体仍可进入模型。
#[test]
fn missing_ref_msg_idx_with_payload_marks_quoted_payload_without_reference_id() {
    let mut store = RefIndex::default();
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "引用内容查看");
    // ref_msg_idx 缺失，但 payload 存在。
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: None,
        text_summary: Some("被引用原文".to_owned()),
        input_parts: vec![MessageInputPart::text("被引用原文")],
        lookup_found: true,
        fallback_reason: Some("pending_ref_index_lookup".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    // 有 payload 时仍标记为 lookup_found，不输出"引用内容不可用"。
    assert!(quoted.lookup_found);
    assert_eq!(
        quoted.fallback_reason.as_deref(),
        Some("quoted_payload_without_reference_id")
    );
    // 引用 payload 保留。
    assert_eq!(quoted.text_summary.as_deref(), Some("被引用原文"));
    assert_eq!(quoted.input_parts.len(), 1);
    assert_eq!(quoted.input_parts[0].text_content(), Some("被引用原文"));
    // reference_id 不产生 Some("")。
    assert_eq!(quoted.reference_id, None);
}
