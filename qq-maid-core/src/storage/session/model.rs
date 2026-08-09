//! Session 数据结构与纯状态操作。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use qq_maid_common::identity_context::MessageActorContext;

use crate::{
    identity::{parse_stable_scope_key, stable_scope_key},
    runtime::pending::PreparedAction,
    runtime::tools::todo::{TodoItem, TodoStatus},
};

use super::{infer_scope, now_iso_cn, redact_sensitive_text};

/// 会话记录，包含完整的会话状态和历史。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionRecord {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub scope_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub state: Map<String, Value>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub history: Vec<SessionMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_operation: Option<PreparedAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_todo_query: Option<LastTodoQuery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_todo_action: Option<LastTodoAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_memory_query: Option<LastMemoryQuery>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
    /// 当前请求准备写入历史的群聊 turn actor，只在内存中短暂存在。
    ///
    /// 它会在 `append_exchange` 时复制到 user / assistant 两条消息，随后立即清空；
    /// 不能把本字段当成会话级当前用户，因为群聊 conversation session 会被多人共享。
    #[serde(skip)]
    pub(super) turn_actor: Option<SessionTurnActor>,
}

/// 会话中的单条消息，包含角色、内容和时间戳。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMessage {
    /// SQLite 分配的稳定消息 ID。首次持久化后保持不变，并随压缩归档一起保存。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    pub role: String,
    pub content: String,
    pub ts: String,
    /// 群聊消息的当轮 actor 归属。旧消息缺失时保持 None，不能回退为当前 actor。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_actor: Option<SessionTurnActor>,
}

/// 群聊历史中一轮 user / assistant 共享的 actor 快照。
///
/// `actor_ref` 是 conversation scope 内稳定的脱敏引用，不具备现实身份认证含义；
/// 原始平台 user_id 不进入 SessionMessage，也不会通过历史前缀交给模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTurnActor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_member_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_source: Option<String>,
}

/// 判断 Session scope 是否由多名成员共享聊天历史。
///
/// 当前群聊和频道会话均共享 conversation session；私聊保持单用户历史格式。
pub fn is_shared_conversation_scope(scope: &str) -> bool {
    matches!(scope.trim(), "group" | "guild_channel")
}

/// 上次待办查询记录，用于在会话上下文中快速引用查询结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastTodoQuery {
    pub owner_key: String,
    pub query_type: String,
    pub condition: String,
    /// 由所属业务域解释的不透明查询重放上下文；session 层不得解析其内部字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_context: Option<Value>,
    #[serde(default)]
    pub result_ids: Vec<String>,
    pub created_at: String,
}

/// 最近一次成功改变 Todo 状态的条目快照。
///
/// 该结构只保存“刚才那个/它/恢复的那个”所需的最小信息；真正执行新操作时，
/// 仍必须回到 TodoStore 用 owner + item_id 再查一次当前状态，不能信任 session 缓存。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastTodoAction {
    pub owner_key: String,
    pub item_id: String,
    pub title: String,
    pub action: String,
    pub resulting_status: TodoStatus,
    pub created_at: String,
}

/// 上次记忆查询记录，用于在会话上下文中快速引用查询结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastMemoryQuery {
    /// 列表所属 actor；旧快照缺失时运行时会要求重新列表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    pub query_type: String,
    pub condition: String,
    /// 列表生成时的记忆访问边界；旧快照缺失时运行时会要求重新列表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    /// v3 记忆命名空间与可选画像主体；用于阻止 personal/profile/group 序号串用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    #[serde(default)]
    pub result_ids: Vec<String>,
    pub created_at: String,
}

/// 会话元信息，用于标识和创建会话。
///
/// scope_key 的格式如 "group:g1"、"private:u1"、"guild:guild_id:channel_id"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub scope: String,
    pub scope_key: String,
    pub user_id: Option<String>,
    pub group_id: Option<String>,
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub platform: String,
    pub account_id: Option<String>,
}

impl SessionRecord {
    /// 返回稳定摘要锚点的修订号；旧会话没有该字段时按 0 处理。
    pub fn summary_revision(&self) -> u64 {
        self.extra
            .get("summary_revision")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    pub(super) fn advance_summary_revision(&mut self) {
        let revision = self.summary_revision().saturating_add(1);
        self.extra
            .insert("summary_revision".to_owned(), Value::from(revision));
    }

    /// 比较两个会话的持久化快照，忽略仅在当前请求内使用的 turn actor。
    pub(super) fn persistent_snapshot_matches(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.scope == other.scope
            && self.scope_key == other.scope_key
            && self.user_id == other.user_id
            && self.group_id == other.group_id
            && self.guild_id == other.guild_id
            && self.channel_id == other.channel_id
            && self.platform == other.platform
            && self.created_at == other.created_at
            && self.updated_at == other.updated_at
            && self.title == other.title
            && self.state == other.state
            && self.summary == other.summary
            && self.history == other.history
            && self.pending_operation == other.pending_operation
            && self.last_todo_query == other.last_todo_query
            && self.last_todo_action == other.last_todo_action
            && self.last_memory_query == other.last_memory_query
            && self.extra == other.extra
    }

    /// 追加一条消息到会话历史（仅允许 user 和 assistant 角色），内容自动脱敏。
    pub fn append_message(&mut self, role: &str, content: &str) {
        if !matches!(role, "user" | "assistant") {
            return;
        }
        self.history.push(SessionMessage {
            message_id: None,
            role: role.to_owned(),
            content: redact_sensitive_text(content),
            ts: now_iso_cn(),
            turn_actor: None,
        });
    }

    /// 为当前请求绑定群聊 turn actor；私聊调用方应传 None。
    pub(crate) fn set_turn_actor(&mut self, actor: Option<SessionTurnActor>) {
        self.turn_actor = actor;
    }

    pub(super) fn take_turn_actor(&mut self) -> Option<SessionTurnActor> {
        self.turn_actor.take()
    }

    pub(super) fn append_message_with_turn_actor(
        &mut self,
        role: &str,
        content: &str,
        turn_actor: Option<SessionTurnActor>,
    ) {
        if !matches!(role, "user" | "assistant") {
            return;
        }
        self.history.push(SessionMessage {
            message_id: None,
            role: role.to_owned(),
            content: redact_sensitive_text(content),
            ts: now_iso_cn(),
            turn_actor,
        });
    }

    /// 清空上下文相关状态，保留会话元信息。
    pub fn reset(&mut self) {
        self.summary.clear();
        self.extra.remove("summary_revision");
        self.state.clear();
        self.history.clear();
        self.pending_operation = None;
        self.last_todo_query = None;
        self.last_todo_action = None;
        self.last_memory_query = None;
    }

    /// 合并追加回复前已由业务 flow 更新的短期交互状态。
    ///
    /// 调用方手里的 current 可能已经更新 pending、最近查询或最近操作快照；
    /// 这些字段必须合并到重新读取的 latest session，不能被数据库旧状态反向覆盖。
    pub fn merge_interaction_side_effects_from(&mut self, current: &SessionRecord) {
        self.state = current.state.clone();
        self.pending_operation = current.pending_operation.clone();
        self.last_memory_query = current.last_memory_query.clone();
        self.last_todo_query = current.last_todo_query.clone();
        self.last_todo_action = current.last_todo_action.clone();
    }

    /// 记录最近一次真正展示给用户的 Todo 列表快照。
    ///
    /// `result_ids` 必须与最终展示顺序完全一致；后续“第一条 / 第二条 / 它”
    /// 只允许按这份快照映射，不能回退数据库默认顺序。
    pub fn remember_last_todo_query(
        &mut self,
        owner_key: &str,
        query_type: impl Into<String>,
        condition: impl Into<String>,
        result_ids: Vec<String>,
    ) {
        self.last_todo_query = Some(LastTodoQuery {
            owner_key: owner_key.to_owned(),
            query_type: query_type.into(),
            condition: condition.into(),
            replay_context: None,
            result_ids,
            created_at: now_iso_cn(),
        });
    }

    /// 记录最近一次成功操作的单条 Todo。
    ///
    /// 这里只保存自然语言续指所需的最小快照；下次真正执行时仍需重新读取当前 Todo。
    pub fn remember_last_todo_action(&mut self, owner_key: &str, item: &TodoItem, action: &str) {
        self.last_todo_action = Some(LastTodoAction {
            owner_key: owner_key.to_owned(),
            item_id: item.id.clone(),
            title: item.title.clone(),
            action: action.to_owned(),
            resulting_status: item.status.clone(),
            created_at: if item.updated_at.trim().is_empty() {
                now_iso_cn()
            } else {
                item.updated_at.clone()
            },
        });
    }

    /// 根据一次批量结果维护最近操作对象。
    ///
    /// 成功 0 条时保持原值，成功 1 条时记录该条，成功多条时清空，避免续指歧义。
    pub fn update_last_todo_action_from_items(
        &mut self,
        owner_key: &str,
        action: &str,
        items: &[TodoItem],
    ) {
        match items {
            [] => {}
            [item] => self.remember_last_todo_action(owner_key, item, action),
            _ => self.last_todo_action = None,
        }
    }

    /// 当物理删除命中最近对象时清空该快照。
    pub fn clear_last_todo_action_if_matches_any(&mut self, owner_key: &str, item_ids: &[String]) {
        let should_clear = self.last_todo_action.as_ref().is_some_and(|last_action| {
            last_action.owner_key == owner_key
                && item_ids
                    .iter()
                    .any(|item_id| item_id == &last_action.item_id)
        });
        if should_clear {
            self.last_todo_action = None;
        }
    }
}

impl SessionTurnActor {
    /// 从 Gateway/Core 已确认的 actor 字段生成历史快照。
    ///
    /// 稳定引用只在当前 conversation scope 内保持稳定，避免跨群关联同一平台 ID。
    pub fn from_message_actor(scope_key: &str, actor: &MessageActorContext) -> Self {
        Self {
            actor_ref: clean_optional_str(actor.user_id.as_deref())
                .map(|user_id| actor_ref(scope_key, user_id)),
            display_name: clean_actor_value(actor.display_name.as_deref(), 64),
            display_name_source: clean_actor_value(actor.display_name_source.as_deref(), 32),
            group_member_role: clean_actor_value(actor.group_member_role.as_deref(), 32),
            identity_source: Some(actor.source.as_str().to_owned()),
        }
    }

    /// 计算共享会话内使用的脱敏 actor 引用。
    ///
    /// Dream 必须与写入 SessionMessage 时使用完全相同的算法，才能只读取当前群成员
    /// 的历史；调用方不得持久化或记录传入的原始平台用户 ID。
    pub(crate) fn actor_ref_for_user(scope_key: &str, user_id: &str) -> Option<String> {
        let scope_key = scope_key.trim();
        let user_id = user_id.trim();
        (!scope_key.is_empty() && !user_id.is_empty()).then(|| actor_ref(scope_key, user_id))
    }
}

impl SessionMeta {
    /// 创建会话元信息，自动推断作用域类型。
    pub fn new(
        scope_key: impl Into<String>,
        user_id: Option<String>,
        group_id: Option<String>,
        guild_id: Option<String>,
        channel_id: Option<String>,
        platform: impl Into<String>,
    ) -> Self {
        Self::new_with_account(
            scope_key, user_id, group_id, guild_id, channel_id, platform, None,
        )
    }

    /// 创建带平台账号维度的会话元信息。
    ///
    /// account_id 只用于业务隔离键和后续 owner/scope 推导，不是平台发送目标。
    pub fn new_with_account(
        scope_key: impl Into<String>,
        user_id: Option<String>,
        group_id: Option<String>,
        guild_id: Option<String>,
        channel_id: Option<String>,
        platform: impl Into<String>,
        account_id: Option<String>,
    ) -> Self {
        let scope_key = scope_key.into();
        let scope = infer_scope(&scope_key, group_id.as_deref(), guild_id.as_deref());
        Self {
            scope,
            scope_key,
            user_id,
            group_id,
            guild_id,
            channel_id,
            platform: platform.into(),
            account_id,
        }
    }

    /// 当前 actor 的个人业务隔离键。
    ///
    /// 返回值用于 Memory / Todo 等业务归属判断；平台发送仍使用原始 user_id。
    pub fn personal_scope_id(&self) -> Option<String> {
        let user_id = clean_optional_str(self.user_id.as_deref())?;
        if should_namespace_scope(self) {
            Some(stable_scope_key(
                platform_or_default(&self.platform),
                self.account_id.as_deref(),
                "private",
                user_id,
            ))
        } else {
            Some(user_id.to_owned())
        }
    }

    /// 当前群会话的群级业务隔离键。
    ///
    /// 返回值只用于群 Memory / 群 Pending 等状态隔离，不作为群消息发送目标。
    pub fn group_scope_id(&self) -> Option<String> {
        let group_id = clean_optional_str(self.group_id.as_deref())?;
        if let Some(parsed) = parse_stable_scope_key(&self.scope_key)
            && parsed.target_type == "group"
        {
            return Some(self.scope_key.clone());
        }
        if should_namespace_scope(self) {
            Some(stable_scope_key(
                platform_or_default(&self.platform),
                self.account_id.as_deref(),
                "group",
                group_id,
            ))
        } else {
            Some(group_id.to_owned())
        }
    }
}

fn should_namespace_scope(meta: &SessionMeta) -> bool {
    meta.account_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || parse_stable_scope_key(&meta.scope_key).is_some()
}

fn platform_or_default(value: &str) -> &str {
    let value = value.trim();
    if value.is_empty() { "qq" } else { value }
}

fn clean_optional_str(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn clean_actor_value(value: Option<&str>, max_chars: usize) -> Option<String> {
    let value = clean_optional_str(value)?;
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let redacted = redact_sensitive_text(&compact);
    let value = redacted.chars().take(max_chars).collect::<String>();
    (!value.is_empty()).then_some(value)
}

fn actor_ref(scope_key: &str, user_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope_key.trim().as_bytes());
    hasher.update([0]);
    hasher.update(user_id.trim().as_bytes());
    let digest = hasher.finalize();
    let short = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("actor_{short}")
}
