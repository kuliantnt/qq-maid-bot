//! Respond 交互状态边界。
//!
//! Conversation session 承载公开聊天历史；群聊中的 pending、Todo 可见快照和最近操作
//! 属于 actor-aware interaction session。本模块集中这些 scope 派生和状态探测，避免路由、
//! 命令分派和聊天流程各自复制状态判断。

use qq_maid_common::identity_context::{
    ConversationContext, IdentitySource, MessageActorContext, MessageContext,
};

use crate::{
    identity::{interaction_scope_key, parse_stable_scope_key},
    runtime::{
        session::{
            LAST_QUERY_TTL_SECONDS, SessionMeta, SessionRecord, query_is_fresh,
            valid_last_visible_todo_query,
        },
        todo::TodoStore,
        visible_entity::visible_snapshot_has_todo_items,
    },
    service::{CoreInboundClassification, CoreInboundKind},
};

use super::{
    RespondRequest,
    common::clean_string,
    memory_flow, rss_flow, search_flow, session_flow,
    set_flow::{parse_set_command, parse_unset_command},
    todo_flow, train_flow, translation_flow, weather_flow,
};

pub(super) fn respond_meta(req: &RespondRequest) -> SessionMeta {
    SessionMeta::new_with_account(
        req.scope_key.clone(),
        req.user_id.clone(),
        req.group_id.clone(),
        req.guild_id.clone(),
        req.channel_id.clone(),
        clean_string(req.platform.clone()).unwrap_or_else(|| "qq".to_owned()),
        req.account_id.clone(),
    )
}

pub(super) fn respond_interaction_meta(req: &RespondRequest) -> SessionMeta {
    let mut meta = respond_meta(req);
    if req
        .group_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && req
            .user_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && parse_stable_scope_key(&req.scope_key).is_some()
    {
        meta.scope_key = interaction_scope_key(req.user_id.as_deref(), &req.scope_key);
    }
    meta
}

pub(super) fn pending_blocks_immediate(
    user_text: &str,
    active_interaction_session: Option<&SessionRecord>,
    active_conversation_session: Option<&SessionRecord>,
    user_id: Option<&str>,
) -> bool {
    !command_bypasses_pending(user_text)
        && (active_interaction_session
            .and_then(|session| session.pending_operation.as_ref())
            .is_some()
            || active_conversation_session
                .is_some_and(|session| session_pending_visible_to_user(session, user_id)))
}

pub(super) fn session_pending_visible_to_user(
    session: &SessionRecord,
    user_id: Option<&str>,
) -> bool {
    let Some(pending) = session.pending_operation.as_ref() else {
        return false;
    };
    match pending.initiator_user_id() {
        Some(initiator) => user_id == Some(initiator),
        None => true,
    }
}

pub(super) fn command_bypasses_pending(user_text: &str) -> bool {
    session_flow::parse_pending_bypass_session_command(user_text).is_some()
        || parse_set_command(user_text).is_some()
        || parse_unset_command(user_text).is_some()
}

pub(super) fn should_try_todo_flow(user_text: &str) -> bool {
    todo_flow::parse_todo_command(user_text).is_some()
        || todo_flow::is_natural_todo_query_text(user_text)
        || todo_flow::is_full_todo_result_request(user_text)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InteractionDomain {
    /// 当前首个接入通用交互快照的 domain；后续 domain 继续在本层扩展。
    Todo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InteractionDomainState {
    pub domain: InteractionDomain,
    pub has_visible_snapshot: bool,
    pub has_recent_operation: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct InteractionStateSnapshot {
    /// Phase E 的第一步只把最近可见列表/最近操作状态包装成通用快照；
    /// 底层 `last_todo_query` / `last_todo_action` 仍保持现有存储语义。
    domains: Vec<InteractionDomainState>,
}

impl InteractionStateSnapshot {
    pub(super) fn has_recent_context(&self, domain: InteractionDomain) -> bool {
        self.domains.iter().any(|state| {
            state.domain == domain && (state.has_visible_snapshot || state.has_recent_operation)
        })
    }

    #[cfg(test)]
    pub(super) fn with_recent_todo_context_for_test() -> Self {
        Self {
            domains: vec![InteractionDomainState {
                domain: InteractionDomain::Todo,
                has_visible_snapshot: true,
                has_recent_operation: false,
            }],
        }
    }
}

pub(super) fn interaction_snapshot(
    req: &RespondRequest,
    active_session: Option<&SessionRecord>,
) -> InteractionStateSnapshot {
    let mut domains = Vec::new();
    let todo_state = todo_interaction_state(req, active_session);
    if todo_state.has_visible_snapshot || todo_state.has_recent_operation {
        domains.push(todo_state);
    }
    InteractionStateSnapshot { domains }
}

fn todo_interaction_state(
    req: &RespondRequest,
    active_session: Option<&SessionRecord>,
) -> InteractionDomainState {
    let request_visible_snapshot =
        visible_snapshot_has_todo_items(req.visible_entity_snapshot.as_ref());
    let Some(session) = active_session else {
        return InteractionDomainState {
            domain: InteractionDomain::Todo,
            has_visible_snapshot: request_visible_snapshot,
            has_recent_operation: false,
        };
    };
    let owner = TodoStore::owner(req.user_id.as_deref(), &req.scope_key);

    let mut snapshot = session.clone();
    let session_visible_snapshot = valid_last_visible_todo_query(&mut snapshot, &owner.key)
        .is_some_and(|query| !query.result_ids.is_empty());
    let has_recent_operation = session.last_todo_action.as_ref().is_some_and(|action| {
        action.owner_key == owner.key && query_is_fresh(&action.created_at, LAST_QUERY_TTL_SECONDS)
    });

    InteractionDomainState {
        domain: InteractionDomain::Todo,
        has_visible_snapshot: request_visible_snapshot || session_visible_snapshot,
        has_recent_operation,
    }
}

pub(super) fn route_context_session<'a>(
    req: &RespondRequest,
    active_interaction_session: Option<&'a SessionRecord>,
    active_conversation_session: Option<&'a SessionRecord>,
) -> Option<&'a SessionRecord> {
    // 新 session 状态以 interaction scope 为准；旧 conversation 可见快照只作为路由提示
    // 兼容读取，不迁移、不回写，实际 Todo/Memory 状态仍落在 interaction session。
    if interaction_snapshot(req, active_interaction_session)
        .has_recent_context(InteractionDomain::Todo)
    {
        return active_interaction_session;
    }
    if interaction_snapshot(req, active_conversation_session)
        .has_recent_context(InteractionDomain::Todo)
    {
        return active_conversation_session;
    }
    active_interaction_session.or(active_conversation_session)
}

pub(super) fn classify_inbound_with_active(
    user_text: &str,
    active_interaction_session: Option<&SessionRecord>,
    active_conversation_session: Option<&SessionRecord>,
    user_id: Option<&str>,
) -> CoreInboundClassification {
    if pending_blocks_immediate(
        user_text,
        active_interaction_session,
        active_conversation_session,
        user_id,
    ) {
        return CoreInboundClassification {
            kind: CoreInboundKind::Immediate,
        };
    }

    let is_command = session_flow::parse_session_command(user_text).is_some()
        || translation_flow::parse_translation_command(user_text).is_some()
        || weather_flow::parse_weather_command(user_text).is_some()
        || train_flow::parse_train_command(user_text).is_some()
        || search_flow::parse_web_search_command(user_text).is_some()
        || rss_flow::parse_rss_command(user_text).is_some()
        || todo_flow::parse_todo_command(user_text).is_some()
        || todo_flow::is_natural_todo_query_text(user_text)
        || memory_flow::parse_memory_command(user_text).is_some();

    CoreInboundClassification {
        kind: if is_command {
            CoreInboundKind::Immediate
        } else {
            CoreInboundKind::NormalChat
        },
    }
}

/// 用手动展示名增强 `message_context` 与 `quoted.sender` 中的展示名（#326）。
///
/// 优先级：`manual_display_name` > 平台 `display_name` > fallback。
/// 这里只补齐 LLM 可见身份上下文并覆盖展示名 / display_name_source，不改动权限、
/// owner 或 request 权威身份字段；拉取失败时静默跳过，不阻断主流程。
/// `meta.scope_key` 是 conversation scope，与展示名存储的绑定键一致。
pub(super) fn apply_manual_display_names(
    store: &crate::runtime::display_name::DisplayNameStore,
    meta: &SessionMeta,
    req: &mut RespondRequest,
) {
    let scope_key = meta.scope_key.as_str();
    ensure_message_context_actor_identity(meta, req);
    if let Some(context) = req.message_context.as_mut() {
        if let Some(actor) = context.actor.as_mut() {
            apply_manual_display_name_to_actor(store, scope_key, actor);
        }
        for mention in &mut context.mentions {
            apply_manual_display_name_to_actor(store, scope_key, &mut mention.target);
        }
    }
    // 引用消息 sender 来自 ref_index 回填；若有稳定 user_id，也按同一 conversation scope 查手动展示名。
    if let Some(quoted) = &mut req.quoted
        && let Some(sender) = &mut quoted.sender
    {
        apply_manual_display_name_to_actor(store, scope_key, sender);
    }
}

fn ensure_message_context_actor_identity(meta: &SessionMeta, req: &mut RespondRequest) {
    let fallback_user_id = req
        .user_id
        .as_deref()
        .or(meta.user_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let fallback_role = req
        .group_member_role
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if req.message_context.is_none() {
        req.message_context = Some(MessageContext {
            actor: None,
            mentions: Vec::new(),
            conversation: fallback_conversation_context(meta),
        });
    }

    let Some(context) = req.message_context.as_mut() else {
        return;
    };
    fill_empty_conversation_context(&mut context.conversation, meta);

    if context.actor.is_none() && (fallback_user_id.is_some() || fallback_role.is_some()) {
        context.actor = Some(MessageActorContext {
            user_id: fallback_user_id.map(str::to_owned),
            group_member_role: fallback_role.map(str::to_owned),
            source: IdentitySource::LegacyFallback,
            ..Default::default()
        });
        return;
    }

    let Some(actor) = context.actor.as_mut() else {
        return;
    };
    if actor
        .user_id
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
        && let Some(user_id) = fallback_user_id
    {
        actor.user_id = Some(user_id.to_owned());
    }
    if actor
        .group_member_role
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
        && let Some(role) = fallback_role
    {
        actor.group_member_role = Some(role.to_owned());
    }
}

fn fallback_conversation_context(meta: &SessionMeta) -> ConversationContext {
    ConversationContext {
        kind: if meta.scope.trim().is_empty() {
            "unknown".to_owned()
        } else {
            meta.scope.clone()
        },
        id: meta
            .group_id
            .clone()
            .or_else(|| meta.channel_id.clone())
            .or_else(|| meta.user_id.clone())
            .or_else(|| Some(meta.scope_key.clone())),
        platform: Some(meta.platform.clone()),
        account_id: meta.account_id.clone(),
    }
}

fn fill_empty_conversation_context(context: &mut ConversationContext, meta: &SessionMeta) {
    let fallback = fallback_conversation_context(meta);
    if context.kind.trim().is_empty() {
        context.kind = fallback.kind;
    }
    if context
        .id
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        context.id = fallback.id;
    }
    if context
        .platform
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        context.platform = fallback.platform;
    }
    if context
        .account_id
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        context.account_id = fallback.account_id;
    }
}

fn apply_manual_display_name_to_actor(
    store: &crate::runtime::display_name::DisplayNameStore,
    scope_key: &str,
    actor: &mut qq_maid_common::identity_context::MessageActorContext,
) {
    if let Some(user_id) = actor.user_id.as_deref()
        && let Ok(Some(name)) = store.get(scope_key, user_id)
    {
        let name = name.trim();
        if !name.is_empty() {
            actor.display_name = Some(name.to_owned());
            actor.display_name_source = Some("manual".to_owned());
            return;
        }
    }
    if actor.display_name_source.is_none()
        && actor
            .display_name
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
    {
        actor.display_name_source = Some(actor.source.as_str().to_owned());
    }
}
