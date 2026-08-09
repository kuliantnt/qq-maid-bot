use super::*;

// 平台投递行为与 QQ RefIndex/缓存语义分别放在子模块，避免主测试文件继续膨胀。
use qq_maid_core::runtime::push::{PushMention, PushTarget, PushTargetType};

#[derive(Default)]
struct MockPushSender {
    calls: Mutex<Vec<String>>,
    fail_markdown: bool,
    fail_text: bool,
    message_id: Option<String>,
    ref_index_id: Option<String>,
}

impl MockPushSender {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

struct MockOneBotSender {
    account_id: Option<String>,
    calls: Mutex<Vec<String>>,
    fail: bool,
}

impl MockOneBotSender {
    fn connected(account_id: &str) -> Self {
        Self {
            account_id: Some(account_id.to_owned()),
            calls: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl PushOneBotSender for MockOneBotSender {
    fn connected_account_id(&self) -> Option<String> {
        self.account_id.clone()
    }

    async fn send_private_text(
        &self,
        target_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("private:{target_id}:{text}"));
        if self.fail {
            Err(OneBotSendError::Transport(
                crate::gateway::onebot11::OneBotCallError::ConnectionClosed,
            ))
        } else {
            Ok(OneBotSendResult {
                message_id: "ob-private-1".to_owned(),
            })
        }
    }

    async fn send_group_text(
        &self,
        target_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("group:{target_id}:{text}"));
        if self.fail {
            Err(OneBotSendError::Transport(
                crate::gateway::onebot11::OneBotCallError::ConnectionClosed,
            ))
        } else {
            Ok(OneBotSendResult {
                message_id: "ob-group-1".to_owned(),
            })
        }
    }

    async fn send_group_text_with_mentions(
        &self,
        target_id: &str,
        mention_user_ids: &[String],
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        self.calls.lock().unwrap().push(format!(
            "group:{target_id}:at={}:{}",
            mention_user_ids.join(","),
            text
        ));
        if self.fail {
            Err(OneBotSendError::Transport(
                crate::gateway::onebot11::OneBotCallError::ConnectionClosed,
            ))
        } else {
            Ok(OneBotSendResult {
                message_id: "ob-group-1".to_owned(),
            })
        }
    }
}

#[async_trait]
impl PushQqSender for MockPushSender {
    async fn send_c2c_text(&self, target_id: &str, text: &str) -> SendResult {
        self.calls
            .lock()
            .unwrap()
            .push(format!("c2c-text:{target_id}:{text}"));
        if self.fail_text {
            Err(crate::api::ApiError::Unsupported("text"))
        } else {
            Ok(SendMessageIds {
                message_id: self.message_id.clone(),
                ref_index_id: self.ref_index_id.clone(),
            })
        }
    }

    async fn send_c2c_markdown(&self, target_id: &str, markdown: &MarkdownPayload) -> SendResult {
        self.calls
            .lock()
            .unwrap()
            .push(format!("c2c-markdown:{target_id}:{}", markdown.content));
        if self.fail_markdown {
            Err(crate::api::ApiError::Unsupported("markdown"))
        } else {
            Ok(SendMessageIds {
                message_id: self.message_id.clone(),
                ref_index_id: self.ref_index_id.clone(),
            })
        }
    }

    async fn send_group_text(&self, target_id: &str, text: &str) -> SendResult {
        self.calls
            .lock()
            .unwrap()
            .push(format!("group-text:{target_id}:{text}"));
        if self.fail_text {
            Err(crate::api::ApiError::Unsupported("text"))
        } else {
            Ok(SendMessageIds {
                message_id: self.message_id.clone(),
                ref_index_id: self.ref_index_id.clone(),
            })
        }
    }

    async fn send_group_markdown(&self, target_id: &str, markdown: &MarkdownPayload) -> SendResult {
        self.calls
            .lock()
            .unwrap()
            .push(format!("group-markdown:{target_id}:{}", markdown.content));
        if self.fail_markdown {
            Err(crate::api::ApiError::Unsupported("markdown"))
        } else {
            Ok(SendMessageIds {
                message_id: self.message_id.clone(),
                ref_index_id: self.ref_index_id.clone(),
            })
        }
    }
}

fn quoted_group_context(
    ref_index: &SharedRefIndex,
    group_id: &str,
    ref_id: &str,
) -> qq_maid_common::input_part::QuotedMessageContext {
    quoted_group_context_for_account(ref_index, "app", group_id, ref_id)
}

fn quoted_group_context_for_account(
    ref_index: &SharedRefIndex,
    account_id: &str,
    group_id: &str,
    ref_id: &str,
) -> qq_maid_common::input_part::QuotedMessageContext {
    let mut quoted = crate::gateway::platform::InboundMessage {
        platform: crate::gateway::platform::Platform::QqOfficial,
        account_id: Some(account_id.to_owned()),
        conversation: ConversationTarget::Group {
            target_id: group_id.to_owned(),
        },
        actor: crate::gateway::platform::Actor {
            sender_id: Some("member-1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            source: qq_maid_common::identity_context::IdentitySource::Event,
        },
        visible_entity_snapshot: None,
        message_id: "gm-quote".to_owned(),
        current_msg_idx: None,
        timestamp: None,
        text: "继续".to_owned(),
        input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("继续")],
        attachments: Vec::new(),
        quoted: Some(qq_maid_common::input_part::QuotedMessageContext {
            ref_msg_idx: Some(ref_id.to_owned()),
            ..Default::default()
        }),
        mentions: Vec::new(),
        mentioned_bot: false,
    };
    ref_index.lock().unwrap().enrich_inbound(&mut quoted);
    quoted.quoted.unwrap()
}

fn quoted_onebot_context(
    ref_index: &SharedRefIndex,
    account_id: &str,
    conversation: ConversationTarget,
    ref_id: &str,
) -> qq_maid_common::input_part::QuotedMessageContext {
    let mut quoted = crate::gateway::platform::InboundMessage {
        platform: crate::gateway::platform::Platform::OneBot11,
        account_id: Some(account_id.to_owned()),
        conversation,
        actor: crate::gateway::platform::Actor {
            sender_id: Some("member-1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            source: qq_maid_common::identity_context::IdentitySource::Event,
        },
        visible_entity_snapshot: None,
        message_id: "onebot-quote".to_owned(),
        current_msg_idx: None,
        timestamp: None,
        text: "继续".to_owned(),
        input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("继续")],
        attachments: Vec::new(),
        quoted: Some(qq_maid_common::input_part::QuotedMessageContext {
            reference_id: Some(ref_id.to_owned()),
            ..Default::default()
        }),
        mentions: Vec::new(),
        mentioned_bot: false,
    };
    ref_index.lock().unwrap().enrich_inbound(&mut quoted);
    quoted.quoted.unwrap()
}

mod onebot;
mod qq;
