//! Gateway 单测共享 fixture。
//!
//! 这里只收敛跨多个 Gateway 领域都稳定复用的完整配置与基础消息样本；具体发送器、
//! 聚合器和平台 fake 仍留在各自测试模块，避免隐藏被测协议和关键断言。

use std::time::Duration;

use qq_maid_core::service::AssistantOutput;

use super::event::C2cMessage;
use crate::{
    config::{
        AgentTypingConfig, AppConfig, DEFAULT_CONVERSATION_QUEUE_CAPACITY,
        DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT, DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS,
        DEFAULT_MEDIA_MAX_BYTES, DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS,
        DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS, DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES,
        DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS, DEFAULT_MESSAGE_AGGREGATION_QUIET_MS,
        DEFAULT_TEXT_CHUNK_SOFT_LIMIT, GroupMessageMode, MessageAggregationConfig, OneBot11Config,
        WechatServiceConfig,
    },
    respond::RespondResponse,
};

/// 返回已绑定 QQ 官方渠道的稳定测试基线。
///
/// 测试若关心流式、群聊、聚合窗口或分段限制，应在调用处显式修改对应字段，确保关键输入
/// 仍然可见；此处只统一与被测行为无关的配置样板。
pub(crate) fn qq_official_test_config() -> AppConfig {
    AppConfig {
        command_prefix: Default::default(),
        qq_official_enabled: true,
        app_id: Some("app".to_owned()),
        app_secret: Some("secret".to_owned()),
        bot_mention_ids: Vec::new(),
        sandbox: false,
        api_base: "https://example.test".to_owned(),
        token_refresh_margin: Duration::from_secs(60),
        enable_markdown: true,
        enable_image: false,
        enable_group_messages: false,
        verbose_log: false,
        member_detail_enrich_enabled: false,
        group_message_mode: GroupMessageMode::Mention,
        bot_display_name: "小女仆".to_owned(),
        group_active_keywords: vec!["小女仆".to_owned()],
        conversation_queue_capacity: DEFAULT_CONVERSATION_QUEUE_CAPACITY,
        max_active_conversation_workers: DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS,
        conversation_worker_idle_timeout: Duration::from_secs(300),
        message_aggregation: MessageAggregationConfig {
            private_enabled: true,
            group_enabled: false,
            quiet: Duration::from_millis(DEFAULT_MESSAGE_AGGREGATION_QUIET_MS),
            max_wait: Duration::from_millis(DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS),
            max_messages: DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES,
            max_chars: DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
            max_active_keys: DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS,
        },
        c2c_final_reply_stream_enabled: false,
        c2c_visible_progress_status_enabled: true,
        agent_typing: AgentTypingConfig {
            enabled: false,
            delay: Duration::from_secs(1),
        },
        markdown_chunk_soft_limit: DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT,
        text_chunk_soft_limit: DEFAULT_TEXT_CHUNK_SOFT_LIMIT,
        media_dir: std::path::PathBuf::from("media/inbound"),
        media_download_timeout: Duration::from_secs(10),
        media_max_bytes: DEFAULT_MEDIA_MAX_BYTES,
        wechat_service: WechatServiceConfig::default(),
        onebot11: OneBot11Config::default(),
    }
}

pub(crate) fn c2c_message_fixture() -> C2cMessage {
    C2cMessage {
        message_id: "msg-1".to_owned(),
        current_msg_idx: None,
        event_id: Some("event-1".to_owned()),
        source_message_ids: vec!["msg-1".to_owned()],
        source_event_ids: vec!["event-1".to_owned()],
        user_openid: "user-1".to_owned(),
        content: "晚上好".to_owned(),
        reply: None,
        timestamp: None,
        first_message_timestamp: None,
        last_message_timestamp: None,
        input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("晚上好")],
        attachments: Vec::new(),
    }
}

pub(crate) fn respond_response_fixture(text: &str) -> RespondResponse {
    RespondResponse {
        output: Some(AssistantOutput::markdown(text, text)),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    }
}
