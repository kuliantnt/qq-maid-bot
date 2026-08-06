//! QQ 官方群消息轻量入站预处理。
//!
//! 此阶段由 Aggregator actor 按到达顺序调用，只执行本地确定性操作。确定不需要回复的
//! 消息在这里完成 RefIndex 被动观察，不进入可能被 Core、LLM 或媒体下载阻塞的群 worker。

use std::sync::{Arc, Mutex};

use tracing::{debug, info, warn};

use crate::{
    config::AppConfig,
    gateway::{
        BotOutboundCache,
        bot_identity::SharedBotIdentity,
        dedupe::{MessageDedupe, dedupe_qq_composite_key},
        event::GroupMessage,
        group_filter::{
            contains_active_keyword, is_reply_to_bot, mentions_current_bot,
            normalize_current_bot_mentions, should_ignore_group_message,
            should_process_group_message_with_prefix,
        },
        logging::mask_openid,
        ref_index::SharedRefIndex,
    },
    respond::{
        RespondClient, build_group_command_content_with_prefix,
        build_group_respond_content_with_prefix, normalized_group_inbound_with_prefix,
    },
};

/// 预处理阶段已经确定的本地触发事实，随重型 envelope 传递，避免 worker 隐式重算。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupIngressFacts {
    pub(crate) local_command_candidate: bool,
    pub(crate) mentions_current_bot: bool,
    pub(crate) active_keyword: bool,
    pub(crate) replies_to_bot: bool,
}

/// 只有确实需要回复的 QQ 群消息才能构造此类型。
#[derive(Debug, Clone)]
pub(crate) struct PreparedGroupMessage {
    pub(crate) message: GroupMessage,
    pub(crate) respond_content: String,
    pub(crate) command_content: String,
    pub(crate) facts: GroupIngressFacts,
    pub(crate) dedupe_checked: bool,
    pub(crate) passive_observed: bool,
}

/// Dispatcher handle 持有的轻量预处理依赖。所有方法都不调用 Core 或外部网络。
pub(crate) struct GroupIngressPreprocessor {
    config: AppConfig,
    respond: RespondClient,
    dedupe: Arc<MessageDedupe>,
    group_outbound_cache: Arc<Mutex<BotOutboundCache>>,
    bot_identity: SharedBotIdentity,
    ref_index: SharedRefIndex,
}

impl GroupIngressPreprocessor {
    pub(crate) fn new(
        config: AppConfig,
        respond: RespondClient,
        dedupe: Arc<MessageDedupe>,
        group_outbound_cache: Arc<Mutex<BotOutboundCache>>,
        bot_identity: SharedBotIdentity,
        ref_index: SharedRefIndex,
    ) -> Self {
        Self {
            config,
            respond,
            dedupe,
            group_outbound_cache,
            bot_identity,
            ref_index,
        }
    }

    pub(crate) fn preprocess(&self, message: GroupMessage) -> Option<PreparedGroupMessage> {
        preprocess_group_message(
            message,
            &self.config,
            &self.respond,
            &self.dedupe,
            &self.group_outbound_cache,
            &self.bot_identity,
            &self.ref_index,
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn preprocess_group_message(
    mut message: GroupMessage,
    config: &AppConfig,
    respond: &RespondClient,
    dedupe: &MessageDedupe,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    bot_identity: &SharedBotIdentity,
    ref_index: &SharedRefIndex,
) -> Option<PreparedGroupMessage> {
    normalize_current_bot_mentions(&mut message, bot_identity);
    super::log_group_message_received(&message, config.verbose_log);
    let masked_group = mask_openid(&message.group_openid);
    let respond_content = build_group_respond_content_with_prefix(
        &message,
        &config.group_active_keywords,
        config.command_prefix,
    );
    let command_content = build_group_command_content_with_prefix(
        &message,
        &config.group_active_keywords,
        config.command_prefix,
    );
    if should_ignore_group_message(
        &message,
        &respond_content,
        &masked_group,
        group_outbound_cache,
    ) {
        return None;
    }

    // QQ 同一 message_id 可能对应多个拆分消息，必须保留 msg_idx 参与复合去重。
    let dedupe_key = dedupe_qq_composite_key(
        "group",
        &message.group_openid,
        &message.message_id,
        message.current_msg_idx.as_deref(),
    );
    if !dedupe_key.is_empty()
        && dedupe.check_and_insert_many([dedupe_key], std::time::Instant::now())
    {
        info!(
            message_id = %message.message_id,
            group = %masked_group,
            "重复的群聊消息已在轻量入站阶段忽略"
        );
        return None;
    }

    let mentions_current_bot = mentions_current_bot(&message);
    let facts = GroupIngressFacts {
        local_command_candidate: config.command_prefix.is_candidate(&message.content)
            || (mentions_current_bot && config.command_prefix.is_candidate(&command_content)),
        mentions_current_bot,
        active_keyword: contains_active_keyword(&message.content, &config.group_active_keywords),
        replies_to_bot: is_reply_to_bot(&message, group_outbound_cache),
    };
    if !should_process_group_message_with_prefix(
        config.group_message_mode,
        &config.group_active_keywords,
        config.command_prefix,
        &message,
        &command_content,
        bot_identity,
        group_outbound_cache,
    ) {
        observe_passive_group_message(&message, config, respond, ref_index);
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            event_type = message.event_type.as_respond_event_type(),
            mode = ?config.group_message_mode,
            local_command_candidate = facts.local_command_candidate,
            mentions_current_bot = facts.mentions_current_bot,
            active_keyword = facts.active_keyword,
            replies_to_bot = facts.replies_to_bot,
            "群聊消息已在轻量入站阶段按模式策略忽略"
        );
        return None;
    }

    Some(PreparedGroupMessage {
        message,
        respond_content,
        command_content,
        facts,
        dedupe_checked: true,
        passive_observed: false,
    })
}

/// 只保存事件已经携带的标准化正文和轻量媒体引用，不下载或补全任何内容。
fn observe_passive_group_message(
    message: &GroupMessage,
    config: &AppConfig,
    respond: &RespondClient,
    ref_index: &SharedRefIndex,
) {
    if !message
        .current_msg_idx
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return;
    }
    let inbound = respond.prepare_inbound(normalized_group_inbound_with_prefix(
        message,
        &config.group_active_keywords,
        config.command_prefix,
    ));
    match ref_index.lock() {
        Ok(mut index) => index.insert_passive_observation(&inbound),
        Err(_) => warn!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            "ref_index 锁已中毒，跳过群聊轻量被动观察"
        ),
    }
}
