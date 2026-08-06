use super::*;

#[test]
fn group_send_records_message_id_for_cache_and_refidx_for_ref_index() {
    let config = test_config();
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let message = group_message("小女仆 你好", GroupEventType::GroupMessage);
    let response = CoreResponse {
        output: Some(qq_maid_common::output_part::AssistantOutput::markdown(
            "机器人回复",
            "机器人回复",
        )),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
        delivery_hint: None,
    };
    let sent_ids = SendMessageIds {
        message_id: Some("qq_msg_1".to_owned()),
        ref_index_id: Some("REFIDX_1".to_owned()),
    };

    record_group_bot_outbound_send(
        &cache,
        &ref_index,
        &message,
        &response,
        &config,
        &sent_ids,
        "机器人回复",
    );

    assert!(cache.lock().unwrap().contains("qq_msg_1"));
    assert!(!cache.lock().unwrap().contains("REFIDX_1"));
    assert!(cache.lock().unwrap().contains_ref_index_id("REFIDX_1"));

    let mut quoted = group_message("继续", GroupEventType::GroupMessage);
    quoted.reply = Some(crate::gateway::event::MessageReply {
        message_id: "qq_reply_payload_id".to_owned(),
        ref_msg_idx: Some("REFIDX_1".to_owned()),
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });
    assert!(should_process_group_message(
        crate::config::GroupMessageMode::Mention,
        &[],
        &quoted,
        &quoted.content,
        &bot_identity(),
        &cache
    ));

    let mut inbound = platform::qq_official::inbound_from_group(&quoted);
    inbound.account_id = config.app_id.clone();
    ref_index.lock().unwrap().enrich_inbound(&mut inbound);
    let quoted_context = inbound.quoted.as_ref().unwrap();
    assert!(quoted_context.lookup_found);
    assert_eq!(quoted_context.text_summary.as_deref(), Some("机器人回复"));
    assert_eq!(quoted_context.from_bot, Some(true));
}

#[test]
fn group_send_records_rendered_fallback_when_output_text_field_is_empty() {
    let config = test_config();
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let message = group_message("小女仆 看图", GroupEventType::GroupMessage);
    let response = CoreResponse {
        output: Some(qq_maid_common::output_part::AssistantOutput {
            text_fallback: String::new(),
            markdown: None,
            parts: vec![qq_maid_common::output_part::OutputPart::Image {
                media: qq_maid_common::output_part::OutputMedia {
                    fallback_text: Some("图片：天气雷达".to_owned()),
                    ..qq_maid_common::output_part::OutputMedia::default()
                },
            }],
        }),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
        delivery_hint: None,
    };

    record_group_bot_outbound_send(
        &cache,
        &ref_index,
        &message,
        &response,
        &config,
        &SendMessageIds {
            message_id: Some("qq_msg_1".to_owned()),
            ref_index_id: Some("REFIDX_rendered".to_owned()),
        },
        "图片：天气雷达",
    );

    let mut quoted = group_message("继续", GroupEventType::GroupMessage);
    quoted.reply = Some(crate::gateway::event::MessageReply {
        message_id: "qq_reply_payload_id".to_owned(),
        ref_msg_idx: Some("REFIDX_rendered".to_owned()),
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });
    let mut inbound = platform::qq_official::inbound_from_group(&quoted);
    inbound.account_id = config.app_id.clone();
    ref_index.lock().unwrap().enrich_inbound(&mut inbound);

    let quoted_context = inbound.quoted.as_ref().unwrap();
    assert!(quoted_context.lookup_found);
    assert_eq!(
        quoted_context.text_summary.as_deref(),
        Some("图片：天气雷达")
    );
}

#[test]
fn group_send_does_not_cross_use_message_id_and_refidx_when_one_is_missing() {
    let config = test_config();
    let response = CoreResponse {
        output: Some(qq_maid_common::output_part::AssistantOutput::text(
            "机器人回复",
        )),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
        delivery_hint: None,
    };

    let message_only_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let message_only_index = crate::gateway::ref_index::ref_index();
    let message = group_message("小女仆 你好", GroupEventType::GroupMessage);
    record_group_bot_outbound_send(
        &message_only_cache,
        &message_only_index,
        &message,
        &response,
        &config,
        &SendMessageIds {
            message_id: Some("qq_msg_only".to_owned()),
            ref_index_id: None,
        },
        "机器人回复",
    );
    assert!(message_only_cache.lock().unwrap().contains("qq_msg_only"));
    assert!(
        !message_only_cache
            .lock()
            .unwrap()
            .contains_ref_index_id("qq_msg_only")
    );
    let mut message_only_quote = platform::qq_official::inbound_from_group(&message);
    message_only_quote.account_id = config.app_id.clone();
    message_only_quote.quoted = Some(qq_maid_common::input_part::QuotedMessageContext {
        ref_msg_idx: Some("qq_msg_only".to_owned()),
        ..Default::default()
    });
    message_only_index
        .lock()
        .unwrap()
        .enrich_inbound(&mut message_only_quote);
    assert!(!message_only_quote.quoted.as_ref().unwrap().lookup_found);

    let refidx_only_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let refidx_only_index = crate::gateway::ref_index::ref_index();
    record_group_bot_outbound_send(
        &refidx_only_cache,
        &refidx_only_index,
        &message,
        &response,
        &config,
        &SendMessageIds {
            message_id: None,
            ref_index_id: Some("REFIDX_only".to_owned()),
        },
        "机器人回复",
    );
    assert!(!refidx_only_cache.lock().unwrap().contains("REFIDX_only"));
    assert!(
        refidx_only_cache
            .lock()
            .unwrap()
            .contains_ref_index_id("REFIDX_only")
    );
}
