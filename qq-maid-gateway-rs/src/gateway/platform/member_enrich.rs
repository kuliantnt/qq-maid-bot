//! 入站成员详情补全（#319 Phase 3）。
//!
//! 在群聊入站消息构造完成后，调用 #229 `get_group_member_cached` 对 actor /
//! mention target / 引用 sender 的 `display_name` / `group_member_role` / `is_bot` /
//! `union_openid` 等**展示字段**做 best-effort 补全，`source` 标注为 `MemberApi`
//! 或 `Cache`。拉取失败 / 负缓存命中时返回 `Unavailable`，保持 `source=Event` 不伪造，
//! 不阻断主回复流程。
//!
//! 只对群聊补全；私聊无 `group_openid`，直接跳过。只补全缺失字段，不覆盖事件已提供
//! 的稳定 ID；稳定身份 key 仍以平台结构化 ID 为准，`username` 仅作展示和 LLM 理解。

use std::{future::Future, pin::Pin};

use qq_maid_common::identity_context::{IdentitySource, MessageActorContext};

use crate::api::{
    QqApiClient,
    member_detail::{GroupMemberDetail, MemberFetchResult},
};

use super::model::{Actor, ConversationTarget, GroupMemberRoleKind, InboundMessage};

/// 成员详情拉取抽象，便于在入站补全链路注入 mock（对齐 `OutboundSender` 模式）。
///
/// `QqApiClient` 实现走真实 #229 + 缓存；测试可用内存 mock 验证补全 / 降级路径。
pub(crate) trait MemberDetailFetcher: Send + Sync {
    fn fetch_member_detail<'a>(
        &'a self,
        group_openid: &'a str,
        member_openid: &'a str,
    ) -> Pin<Box<dyn Future<Output = MemberFetchResult> + Send + 'a>>;
}

impl MemberDetailFetcher for QqApiClient {
    fn fetch_member_detail<'a>(
        &'a self,
        group_openid: &'a str,
        member_openid: &'a str,
    ) -> Pin<Box<dyn Future<Output = MemberFetchResult> + Send + 'a>> {
        Box::pin(self.get_group_member_cached(group_openid, member_openid))
    }
}

/// 把 #229 成员详情接口返回的原始角色字符串映射为平台无关 `GroupMemberRoleKind`。
fn role_kind_from_str(s: &str) -> GroupMemberRoleKind {
    match s.trim().to_ascii_lowercase().as_str() {
        "owner" => GroupMemberRoleKind::Owner,
        "admin" => GroupMemberRoleKind::Admin,
        "member" => GroupMemberRoleKind::Member,
        _ => GroupMemberRoleKind::Unknown,
    }
}

/// 把 `GroupMemberDetail` 补全到 gateway `Actor`（消息发送者）。
///
/// 只补齐缺失或更强的展示字段，不覆盖事件已提供的结构化信号：
/// - 事件已有角色时保持事件值，避免 API 未知 / 普通成员响应把 owner/admin 降级。
/// - `is_bot=true` 是更强信号，API 返回 false 不得降级。
/// - 只有实际使用了成员详情字段时才更新 `source` 为 `MemberApi` / `Cache`。
fn apply_detail_to_actor(actor: &mut Actor, detail: &GroupMemberDetail, source: IdentitySource) {
    let mut used_detail = false;
    if actor.display_name.is_none() && detail.username.is_some() {
        actor.display_name = detail.username.clone();
        used_detail = true;
    }
    if actor.union_id.is_none() && detail.union_openid.is_some() {
        actor.union_id = detail.union_openid.clone();
        used_detail = true;
    }
    if actor.group_member_role.is_none()
        && let Some(role) = detail.member_role.as_deref()
    {
        actor.group_member_role = Some(role_kind_from_str(role));
        used_detail = true;
    }
    if !actor.is_bot && detail.bot.unwrap_or(false) {
        actor.is_bot = true;
        used_detail = true;
    }
    if used_detail {
        actor.source = source;
    }
}

/// 把 `GroupMemberDetail` 补全到 `MessageActorContext`（mention target / 引用 sender）。
fn apply_detail_to_context(
    ctx: &mut MessageActorContext,
    detail: &GroupMemberDetail,
    source: IdentitySource,
) {
    if ctx.display_name.is_none() {
        ctx.display_name = detail.username.clone();
        if ctx.display_name.is_some() {
            ctx.display_name_source = Some(source.as_str().to_owned());
        }
    }
    if ctx.union_id.is_none() {
        ctx.union_id = detail.union_openid.clone();
    }
    if ctx.group_member_role.is_none() {
        ctx.group_member_role = detail.member_role.clone();
    }
    if ctx.is_bot != Some(true)
        && let Some(bot) = detail.bot
    {
        ctx.is_bot = Some(bot);
    }
    ctx.source = source;
}

/// 按命中结果标注 `IdentitySource`。
fn source_for(result: &MemberFetchResult) -> Option<IdentitySource> {
    match result {
        MemberFetchResult::Fresh(_) => Some(IdentitySource::MemberApi),
        MemberFetchResult::Cached(_) => Some(IdentitySource::Cache),
        MemberFetchResult::Unavailable => None,
    }
}

/// 取出命中详情（`Unavailable` 返回 None）。
fn detail_of(result: &MemberFetchResult) -> Option<&GroupMemberDetail> {
    match result {
        MemberFetchResult::Fresh(detail) | MemberFetchResult::Cached(detail) => Some(detail),
        MemberFetchResult::Unavailable => None,
    }
}

/// 对群聊入站消息做成员详情补全（#319）。
///
/// 调用顺序：ref_index.enrich_inbound 之后调用本函数（此时 quoted.sender.user_id 已回填），
/// 再之后才 insert_inbound（让索引里存的是补全后的 sender）。
///
/// 任何拉取失败都降级为保持 `source=Event`，不阻断、不 panic。
pub(crate) async fn enrich_inbound_member_details<F: MemberDetailFetcher>(
    fetcher: &F,
    inbound: &mut InboundMessage,
) {
    // 只对群聊补全；私聊无 group_openid，跳过。
    let ConversationTarget::Group { target_id } = &inbound.conversation else {
        return;
    };
    let group_openid = target_id.clone();

    // 1) actor（当前发言人）补全。
    if let Some(member_openid) = inbound.actor.sender_id.as_deref() {
        let result = fetcher
            .fetch_member_detail(&group_openid, member_openid)
            .await;
        if let (Some(detail), Some(source)) = (detail_of(&result), source_for(&result)) {
            apply_detail_to_actor(&mut inbound.actor, detail, source);
        }
    }

    // 2) mention target 补全。self mention 若有稳定 target id 也可复用同一接口补全；
    // synthetic self mention 没有 user_id 时自然跳过。
    for mention in &mut inbound.mentions {
        let Some(member_openid) = mention.target.user_id.as_deref() else {
            continue;
        };
        let result = fetcher
            .fetch_member_detail(&group_openid, member_openid)
            .await;
        if let (Some(detail), Some(source)) = (detail_of(&result), source_for(&result)) {
            apply_detail_to_context(&mut mention.target, detail, source);
        }
    }

    // 3) 引用消息 sender 补全（ref_index 已回填 user_id）。
    if let Some(quoted) = &mut inbound.quoted
        && let Some(sender) = &mut quoted.sender
        && let Some(member_openid) = sender.user_id.as_deref()
    {
        let result = fetcher
            .fetch_member_detail(&group_openid, member_openid)
            .await;
        if let (Some(detail), Some(source)) = (detail_of(&result), source_for(&result)) {
            apply_detail_to_context(sender, detail, source);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_kind_from_str_maps_known_and_unknown() {
        assert_eq!(role_kind_from_str("owner"), GroupMemberRoleKind::Owner);
        assert_eq!(role_kind_from_str("Admin"), GroupMemberRoleKind::Admin);
        assert_eq!(role_kind_from_str(" member "), GroupMemberRoleKind::Member);
        assert_eq!(role_kind_from_str("其他"), GroupMemberRoleKind::Unknown);
    }

    #[test]
    fn source_for_distinguishes_fresh_cached_unavailable() {
        let detail = GroupMemberDetail {
            member_openid: Some("m".to_owned()),
            username: None,
            member_role: None,
            bot: None,
            joined_at: None,
            union_openid: None,
        };
        assert_eq!(
            source_for(&MemberFetchResult::Fresh(detail.clone())),
            Some(IdentitySource::MemberApi)
        );
        assert_eq!(
            source_for(&MemberFetchResult::Cached(detail)),
            Some(IdentitySource::Cache)
        );
        assert_eq!(source_for(&MemberFetchResult::Unavailable), None);
    }

    #[test]
    fn detail_of_returns_some_for_fresh_and_cached() {
        let detail = GroupMemberDetail {
            member_openid: Some("m".to_owned()),
            username: None,
            member_role: None,
            bot: None,
            joined_at: None,
            union_openid: None,
        };
        assert!(detail_of(&MemberFetchResult::Fresh(detail.clone())).is_some());
        assert!(detail_of(&MemberFetchResult::Cached(detail)).is_some());
        assert!(detail_of(&MemberFetchResult::Unavailable).is_none());
    }

    // ---- 补全集成测试（注入 mock fetcher，不触发真实 HTTP）----

    use qq_maid_common::identity_context::{
        MentionConfidence, MentionIdentity, MessageActorContext,
    };
    use qq_maid_common::input_part::QuotedMessageContext;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::model::{ConversationTarget, Platform};

    /// 按 member_openid 返回预设结果的 mock fetcher；记录调用次数。
    struct MockFetcher {
        by_member: std::sync::Mutex<Vec<(String, MemberFetchResult)>>,
        calls: AtomicUsize,
    }

    impl MockFetcher {
        fn new(pairs: Vec<(&str, MemberFetchResult)>) -> Self {
            Self {
                by_member: std::sync::Mutex::new(
                    pairs.into_iter().map(|(m, r)| (m.to_owned(), r)).collect(),
                ),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl MemberDetailFetcher for MockFetcher {
        fn fetch_member_detail<'a>(
            &'a self,
            _group_openid: &'a str,
            member_openid: &'a str,
        ) -> Pin<Box<dyn Future<Output = MemberFetchResult> + Send + 'a>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let member = member_openid.to_owned();
            let guard = self.by_member.lock().unwrap();
            let result = guard
                .iter()
                .find(|(m, _)| m == &member)
                .map(|(_, r)| r.clone())
                .unwrap_or(MemberFetchResult::Unavailable);
            Box::pin(async move { result })
        }
    }

    fn sample_detail_for(member: &str) -> GroupMemberDetail {
        GroupMemberDetail {
            member_openid: Some(member.to_owned()),
            username: Some(format!("昵称-{member}")),
            member_role: Some("admin".to_owned()),
            bot: Some(false),
            joined_at: None,
            union_openid: Some(format!("union-{member}")),
        }
    }

    fn group_inbound_for_enrich() -> InboundMessage {
        // actor = sender-1；一条普通 mention target=member-2；引用 sender=member-3。
        InboundMessage {
            platform: Platform::QqOfficial,
            account_id: None,
            conversation: ConversationTarget::Group {
                target_id: "group-1".to_owned(),
            },
            actor: Actor {
                sender_id: Some("sender-1".to_owned()),
                union_id: None,
                display_name: None,
                group_member_role: None,
                is_bot: false,
                source: IdentitySource::Event,
            },
            message_id: "msg-1".to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "hi".to_owned(),
            input_parts: Vec::new(),
            attachments: Vec::new(),
            quoted: Some(QuotedMessageContext {
                sender: Some(MessageActorContext {
                    user_id: Some("member-3".to_owned()),
                    source: IdentitySource::Event,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            mentions: vec![MentionIdentity {
                raw_text: None,
                target: MessageActorContext {
                    user_id: Some("member-2".to_owned()),
                    source: IdentitySource::Event,
                    ..Default::default()
                },
                is_self: false,
                confidence: MentionConfidence::Event,
            }],
            mentioned_bot: false,
            tools_visible_snapshot: None,
        }
    }

    #[tokio::test]
    async fn enrich_fills_actor_mention_and_quoted_sender_with_member_api_source() {
        let fetcher = MockFetcher::new(vec![
            (
                "sender-1",
                MemberFetchResult::Fresh(sample_detail_for("sender-1")),
            ),
            (
                "member-2",
                MemberFetchResult::Fresh(sample_detail_for("member-2")),
            ),
            (
                "member-3",
                MemberFetchResult::Fresh(sample_detail_for("member-3")),
            ),
        ]);
        let mut inbound = group_inbound_for_enrich();
        enrich_inbound_member_details(&fetcher, &mut inbound).await;

        // actor 补全：Fresh -> source=MemberApi，展示字段填充，稳定 ID 不覆盖。
        assert_eq!(inbound.actor.source, IdentitySource::MemberApi);
        assert_eq!(inbound.actor.display_name.as_deref(), Some("昵称-sender-1"));
        assert_eq!(inbound.actor.union_id.as_deref(), Some("union-sender-1"));
        assert_eq!(
            inbound.actor.group_member_role,
            Some(GroupMemberRoleKind::Admin)
        );
        assert!(!inbound.actor.is_bot);

        // mention target 补全：source=MemberApi。
        assert_eq!(inbound.mentions[0].target.source, IdentitySource::MemberApi);
        assert_eq!(
            inbound.mentions[0].target.display_name.as_deref(),
            Some("昵称-member-2")
        );
        assert_eq!(inbound.mentions[0].target.is_bot, Some(false));

        // 引用 sender 补全：source=MemberApi。
        let sender = inbound.quoted.as_ref().unwrap().sender.as_ref().unwrap();
        assert_eq!(sender.source, IdentitySource::MemberApi);
        assert_eq!(sender.display_name.as_deref(), Some("昵称-member-3"));
        assert_eq!(sender.union_id.as_deref(), Some("union-member-3"));
    }

    #[tokio::test]
    async fn actor_existing_role_and_bot_true_are_not_downgraded() {
        let mut inbound = group_inbound_for_enrich();
        inbound.actor.group_member_role = Some(GroupMemberRoleKind::Owner);
        inbound.actor.is_bot = true;
        inbound.actor.display_name = Some("事件昵称".to_owned());
        inbound.actor.union_id = Some("event-union".to_owned());
        inbound.mentions.clear();
        inbound.quoted = None;

        let detail = GroupMemberDetail {
            member_openid: Some("sender-1".to_owned()),
            username: None,
            // API 返回未知 / 普通角色不能覆盖事件已知 owner。
            member_role: Some("member".to_owned()),
            // API false 不能把已知 bot/self true 降级。
            bot: Some(false),
            joined_at: None,
            union_openid: None,
        };
        let fetcher = MockFetcher::new(vec![("sender-1", MemberFetchResult::Fresh(detail))]);

        enrich_inbound_member_details(&fetcher, &mut inbound).await;

        assert_eq!(fetcher.calls(), 1);
        assert_eq!(
            inbound.actor.group_member_role,
            Some(GroupMemberRoleKind::Owner)
        );
        assert!(inbound.actor.is_bot);
        assert_eq!(inbound.actor.display_name.as_deref(), Some("事件昵称"));
        assert_eq!(inbound.actor.union_id.as_deref(), Some("event-union"));
        // 没有实际使用 API 字段补全时，不把来源误标为 MemberApi。
        assert_eq!(inbound.actor.source, IdentitySource::Event);
    }

    #[tokio::test]
    async fn actor_missing_display_and_union_are_still_enriched() {
        let mut inbound = group_inbound_for_enrich();
        inbound.actor.group_member_role = Some(GroupMemberRoleKind::Admin);
        inbound.mentions.clear();
        inbound.quoted = None;

        let detail = GroupMemberDetail {
            member_openid: Some("sender-1".to_owned()),
            username: Some("API昵称".to_owned()),
            member_role: Some("member".to_owned()),
            bot: Some(false),
            joined_at: None,
            union_openid: Some("api-union".to_owned()),
        };
        let fetcher = MockFetcher::new(vec![("sender-1", MemberFetchResult::Fresh(detail))]);

        enrich_inbound_member_details(&fetcher, &mut inbound).await;

        assert_eq!(inbound.actor.display_name.as_deref(), Some("API昵称"));
        assert_eq!(inbound.actor.union_id.as_deref(), Some("api-union"));
        // 已有事件角色仍不被 API member 覆盖。
        assert_eq!(
            inbound.actor.group_member_role,
            Some(GroupMemberRoleKind::Admin)
        );
        assert_eq!(inbound.actor.source, IdentitySource::MemberApi);
    }

    #[tokio::test]
    async fn enrich_marks_cached_source_on_cache_hit() {
        let fetcher = MockFetcher::new(vec![
            (
                "sender-1",
                MemberFetchResult::Cached(sample_detail_for("sender-1")),
            ),
            (
                "member-2",
                MemberFetchResult::Cached(sample_detail_for("member-2")),
            ),
            (
                "member-3",
                MemberFetchResult::Cached(sample_detail_for("member-3")),
            ),
        ]);
        let mut inbound = group_inbound_for_enrich();
        enrich_inbound_member_details(&fetcher, &mut inbound).await;
        assert_eq!(inbound.actor.source, IdentitySource::Cache);
        assert_eq!(inbound.mentions[0].target.source, IdentitySource::Cache);
        assert_eq!(
            inbound
                .quoted
                .as_ref()
                .unwrap()
                .sender
                .as_ref()
                .unwrap()
                .source,
            IdentitySource::Cache
        );
    }

    #[tokio::test]
    async fn enrich_degrades_on_unavailable_keeps_event_source() {
        // 全部 Unavailable：不改 source（保持 Event），不伪造字段。
        let fetcher = MockFetcher::new(vec![]);
        let mut inbound = group_inbound_for_enrich();
        enrich_inbound_member_details(&fetcher, &mut inbound).await;
        assert_eq!(inbound.actor.source, IdentitySource::Event);
        assert!(inbound.actor.display_name.is_none());
        assert!(inbound.actor.union_id.is_none());
        assert_eq!(inbound.mentions[0].target.source, IdentitySource::Event);
        assert!(inbound.mentions[0].target.display_name.is_none());
        assert_eq!(
            inbound
                .quoted
                .as_ref()
                .unwrap()
                .sender
                .as_ref()
                .unwrap()
                .source,
            IdentitySource::Event
        );
        // 三次调用（actor + mention + quoted sender）都尝试过。
        assert_eq!(fetcher.calls(), 3);
    }

    #[tokio::test]
    async fn enrich_skips_private_chat_without_fetch() {
        // 私聊无 group_openid，直接跳过，不调用 fetcher。
        let fetcher = MockFetcher::new(vec![(
            "sender-1",
            MemberFetchResult::Fresh(sample_detail_for("sender-1")),
        )]);
        let mut inbound = group_inbound_for_enrich();
        inbound.conversation = ConversationTarget::Private {
            target_id: "user-1".to_owned(),
        };
        enrich_inbound_member_details(&fetcher, &mut inbound).await;
        assert_eq!(fetcher.calls(), 0);
        assert_eq!(inbound.actor.source, IdentitySource::Event);
    }

    #[tokio::test]
    async fn enriches_self_mention_with_target_id_and_preserves_known_bot_flag() {
        // self mention 若带稳定 target id，也复用同一成员详情接口补全展示字段；
        // 但不把已知 is_bot=true 降级为 API 返回的 false。
        let mut inbound = group_inbound_for_enrich();
        inbound.mentions[0].is_self = true;
        inbound.mentions[0].target.is_bot = Some(true);
        let fetcher = MockFetcher::new(vec![
            (
                "sender-1",
                MemberFetchResult::Fresh(sample_detail_for("sender-1")),
            ),
            (
                "member-2",
                MemberFetchResult::Fresh(sample_detail_for("member-2")),
            ),
            (
                "member-3",
                MemberFetchResult::Fresh(sample_detail_for("member-3")),
            ),
        ]);
        enrich_inbound_member_details(&fetcher, &mut inbound).await;
        // actor + self mention target + quoted sender 都尝试补全。
        assert_eq!(fetcher.calls(), 3);
        assert_eq!(inbound.mentions[0].target.source, IdentitySource::MemberApi);
        assert_eq!(
            inbound.mentions[0].target.display_name.as_deref(),
            Some("昵称-member-2")
        );
        assert_eq!(inbound.mentions[0].target.is_bot, Some(true));
    }
}
