//! 会话（Session）存储模块。
//!
//! 会话状态使用项目级 SQLite 数据库持久化，并通过 `SessionStore` 继续暴露
//! 原有的整条会话读写接口。业务层仍然操作 `SessionRecord`，存储层负责把
//! 会话元信息、活跃会话映射和消息顺序拆分保存到数据库中。

use std::fmt;

pub use qq_maid_common::redaction::redact_sensitive_text;
use qq_maid_common::time_context;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    identity::{parse_stable_scope_key, stable_scope_key},
    runtime::pending::PendingOperation,
    storage::database::{DatabaseError, SqliteDatabase, SqliteMigration},
    storage::todo::{TodoItem, TodoStatus},
};

// 拆分出的纯 helper 子模块：均不改变 schema 与对外 API。
mod freshness;
mod jsonio;
mod normalize;
mod row;

use jsonio::{encode_json, encode_optional_json};
use normalize::{
    build_session_id, infer_scope, initial_session_state, normalize_session,
    normalize_session_title,
};
use row::load_session_unlocked;
// 最近查询 helper 是 storage::session 的对外公开 API，保持原路径可访问。
pub use freshness::{
    is_visible_todo_query_type, query_is_fresh, valid_last_todo_query,
    valid_last_visible_todo_query,
};

/// Session schema migration，由应用启动时的通用数据库初始化流程统一执行。
///
/// `session_messages.message_index` 是显式顺序字段，不能只依赖时间戳排序；
/// 同一秒内可能写入多条消息，重启后仍必须按保存时的 Vec 顺序恢复。
pub const SESSION_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "session_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            scope TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            user_id TEXT,
            group_id TEXT,
            guild_id TEXT,
            channel_id TEXT,
            platform TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            title TEXT NOT NULL,
            state_json TEXT NOT NULL,
            summary TEXT NOT NULL DEFAULT '',
            pending_operation_json TEXT,
            last_todo_query_json TEXT,
            last_memory_query_json TEXT,
            extra_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_scope_updated
            ON sessions(scope_key, updated_at, session_id);

        CREATE TABLE IF NOT EXISTS session_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            message_index INTEGER NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            ts TEXT NOT NULL,
            UNIQUE(session_id, message_index),
            FOREIGN KEY(session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_session_messages_order
            ON session_messages(session_id, message_index, id);

        CREATE TABLE IF NOT EXISTS session_active (
            scope_key TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
        );",
};

/// V2 为 session 独立补充最近一次 Todo 成功操作快照。
///
/// 这里不复用 `extra_json`，避免运行时把“最近对象”与任意扩展字段混在一起，
/// 也避免模型侧引用能力和最近列表编号快照共用同一结构。
pub const SESSION_SCHEMA_V2: SqliteMigration = SqliteMigration {
    name: "session_schema_v2_last_todo_action",
    sql: "ALTER TABLE sessions ADD COLUMN last_todo_action_json TEXT;",
};

/// V3 清理早期普通聊天启发式状态和已移除的成员编号身份提示。
///
/// 使用应用注册的 Rust 标量函数处理 JSON，避免依赖部署环境 SQLite 是否启用 JSON1。
pub const SESSION_CLEAN_REMOVED_CHAT_STATE_V3: SqliteMigration = SqliteMigration {
    name: "session_clean_removed_chat_state_v3",
    sql: concat!(
        "UPDATE sessions
            SET state_json = qq_maid_json_remove_object_keys(
                state_json,
                'current_speaker_hint\n",
        "recent_session_focus\n",
        "recent_innerworld_focus\n",
        "active_scene\n",
        "expected_mode\n",
        "last_user_correction\n",
        "known_correction'
            );"
    ),
};

pub const SESSION_MIGRATIONS: &[SqliteMigration] = &[
    SESSION_SCHEMA_V1,
    SESSION_SCHEMA_V2,
    SESSION_CLEAN_REMOVED_CHAT_STATE_V3,
];

/// 默认会话标题，当用户未指定标题时使用。
pub const DEFAULT_SESSION_TITLE: &str = "未命名会话";
/// 最近列表/查询快照的统一有效期（秒）。
pub const LAST_QUERY_TTL_SECONDS: i64 = 10 * 60;

/// 会话记录，包含完整的会话状态和历史。
///
/// 每个会话对应一个 scope_key（如 "group:g1"），包含消息历史、对话摘要、
/// 对话状态、挂起操作以及上次的待办/记忆查询记录。
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
    pub pending_operation: Option<PendingOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_todo_query: Option<LastTodoQuery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_todo_action: Option<LastTodoAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_memory_query: Option<LastMemoryQuery>,
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

/// 会话中的单条消息，包含角色、内容和时间戳。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
    pub ts: String,
}

/// 上次待办查询记录，用于在会话上下文中快速引用查询结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastTodoQuery {
    pub owner_key: String,
    pub query_type: String,
    pub condition: String,
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
    pub query_type: String,
    pub condition: String,
    /// 列表生成时的记忆访问边界；旧快照缺失时运行时会要求重新列表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub result_ids: Vec<String>,
    pub created_at: String,
}

/// 会话元信息，用于标识和创建会话。
///
/// 包含作用域、作用域键值、用户/群组/频道信息以及平台标识。
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

/// 会话存储器，基于项目通用 SQLite 连接实现。
///
/// 数据库连接由应用启动时统一打开并执行 migration；SessionStore 不再读取
/// Session 专用目录，也不兼容旧 JSON 文件，SQLite 是会话状态的事实来源。
#[derive(Debug, Clone)]
pub struct SessionStore {
    database: SqliteDatabase,
}

/// 会话操作错误类型。
#[derive(Debug, Clone)]
pub struct SessionError {
    code: &'static str,
    message: String,
}

impl SessionStore {
    /// 创建一个新的 SessionStore，复用应用级 SQLite 句柄。
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    /// 获取当前活跃会话，不存在则自动创建新会话。
    pub fn get_or_create_active(&self, meta: &SessionMeta) -> Result<SessionRecord, SessionError> {
        let mut conn = self.connection()?;
        if let Some(session) = self.active_session_unlocked(&conn, &meta.scope_key)? {
            return Ok(session);
        }
        self.create_unlocked(&mut conn, meta, String::new(), true)
    }

    /// 获取当前活跃会话，不自动创建。
    pub fn get_active(&self, meta: &SessionMeta) -> Result<Option<SessionRecord>, SessionError> {
        let conn = self.connection()?;
        self.active_session_unlocked(&conn, &meta.scope_key)
    }

    /// 按会话 ID 重新读取最新记录。
    ///
    /// Tool Loop 内部可能已经保存 pending 或最近查询快照；外层聊天流程追加历史前
    /// 必须基于最新记录继续写入，避免旧快照整行覆盖工具刚写入的字段。
    pub fn get(&self, session_id: &str) -> Result<Option<SessionRecord>, SessionError> {
        let conn = self.connection()?;
        load_session_unlocked(&conn, session_id)
    }

    /// 创建一个新会话，可选是否设为当前活跃会话。
    pub fn create(
        &self,
        meta: &SessionMeta,
        title: impl Into<String>,
        set_active: bool,
    ) -> Result<SessionRecord, SessionError> {
        let mut conn = self.connection()?;
        self.create_unlocked(&mut conn, meta, title.into(), set_active)
    }

    /// 保存会话，更新 updated_at 时间戳。
    pub fn save(&self, session: &mut SessionRecord) -> Result<(), SessionError> {
        let mut conn = self.connection()?;
        self.save_unlocked(&mut conn, session, true)
    }

    /// 仅当当前标题仍为预期旧标题时更新标题，不回写会话历史或其他状态。
    ///
    /// 后台自动标题会基于某一时刻的会话快照生成结果；这里必须用条件更新，
    /// 避免旧快照通过 `save` 覆盖后续聊天写入的消息、pending 或手工重命名。
    pub fn update_title_if_current(
        &self,
        session_id: &str,
        expected_title: &str,
        new_title: &str,
    ) -> Result<bool, SessionError> {
        let title = normalize_session_title(new_title);
        let now = now_iso_cn();
        let conn = self.connection()?;
        let changed = conn
            .execute(
                "UPDATE sessions
                 SET title = ?1,
                     updated_at = ?2
                 WHERE session_id = ?3
                   AND title = ?4",
                params![title, now, session_id, expected_title],
            )
            .map_err(SessionError::from_sql)?;
        Ok(changed > 0)
    }

    /// 将指定会话设为某个作用域的活跃会话。
    pub fn set_active_session_id(
        &self,
        scope_key: &str,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let conn = self.connection()?;
        let now = now_iso_cn();
        set_active_session_id_conn(&conn, scope_key, session_id, &now)
    }

    /// 列出某个作用域下的所有会话（可选排除当前会话）。
    pub fn list_for_scope(
        &self,
        scope_key: &str,
        exclude_session_id: Option<&str>,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT session_id
                 FROM sessions
                 WHERE scope_key = ?1
                   AND (?2 IS NULL OR session_id != ?2)
                 ORDER BY updated_at DESC, session_id DESC",
            )
            .map_err(SessionError::from_sql)?;
        let rows = stmt
            .query_map(params![scope_key, exclude_session_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(SessionError::from_sql)?;
        let session_ids = collect_sql_rows(rows)?;
        drop(stmt);

        let mut sessions = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            let Some(session) = load_session_unlocked(&conn, &session_id)? else {
                continue;
            };
            sessions.push(session);
        }
        Ok(sessions)
    }

    /// 追加一次完整的用户-AI 对话交互到会话历史并保存。
    pub fn append_exchange(
        &self,
        session: &mut SessionRecord,
        user_text: &str,
        reply: &str,
    ) -> Result<(), SessionError> {
        session.append_message("user", user_text);
        session.append_message("assistant", reply);
        self.save(session)
    }

    /// 先按 session_id 重新读取最新记录，再合并当前调用方显式变更的字段并追加本轮对话。
    ///
    /// 这类“先重读再合并”的写法主要用于会话中途可能已有其他路径落库的场景，
    /// 例如 Tool Loop 已经写入 pending / 最近查询 / 最近操作对象后，
    /// 旧 `SessionRecord` 不能再整行覆盖最新记录。
    pub fn append_exchange_with_latest<F>(
        &self,
        session: &mut SessionRecord,
        user_text: &str,
        reply: &str,
        merge_latest: F,
    ) -> Result<(), SessionError>
    where
        F: FnOnce(&mut SessionRecord, &SessionRecord),
    {
        let mut latest = if session.session_id.trim().is_empty() {
            session.clone()
        } else {
            self.get(&session.session_id)?
                .unwrap_or_else(|| session.clone())
        };
        merge_latest(&mut latest, session);
        self.append_exchange(&mut latest, user_text, reply)?;
        *session = latest;
        Ok(())
    }

    /// 压缩会话历史：保留最近的 N 条消息，将更早的消息归档到 extra 中，
    /// 并更新会话摘要。
    pub fn compact_history(
        &self,
        session: &mut SessionRecord,
        summary: impl Into<String>,
        keep_messages: usize,
    ) -> Result<(), SessionError> {
        let summary = redact_sensitive_text(summary.into().trim());
        if session.history.len() > keep_messages {
            let archived = session
                .history
                .drain(..session.history.len() - keep_messages)
                .collect::<Vec<_>>();
            let archive = serde_json::json!({
                "archived_at": now_iso_cn(),
                "summary_before": session.summary,
                "history": archived,
            });
            let archived_history = session
                .extra
                .entry("archived_history")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(items) = archived_history.as_array_mut() {
                items.push(archive);
            }
        }
        session.summary = summary;
        self.save(session)
    }

    fn connection(&self) -> Result<crate::storage::database::PooledSqliteConnection, SessionError> {
        self.database
            .connection()
            .map_err(SessionError::from_database)
    }

    fn active_session_unlocked(
        &self,
        conn: &Connection,
        scope_key: &str,
    ) -> Result<Option<SessionRecord>, SessionError> {
        let session_id = conn
            .query_row(
                "SELECT session_id FROM session_active WHERE scope_key = ?1",
                params![scope_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(SessionError::from_sql)?;
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let session = load_session_unlocked(conn, &session_id)?.ok_or_else(|| {
            SessionError::data(format!(
                "active session `{session_id}` for scope `{scope_key}` is missing"
            ))
        })?;
        Ok(Some(session))
    }

    fn create_unlocked(
        &self,
        conn: &mut Connection,
        meta: &SessionMeta,
        title: String,
        set_active: bool,
    ) -> Result<SessionRecord, SessionError> {
        let now = now_iso_cn();
        let title = normalize_session_title(&title);
        let session = SessionRecord {
            session_id: build_session_id(&meta.scope_key),
            scope: meta.scope.clone(),
            scope_key: meta.scope_key.clone(),
            user_id: meta.user_id.clone(),
            group_id: meta.group_id.clone(),
            guild_id: meta.guild_id.clone(),
            channel_id: meta.channel_id.clone(),
            platform: meta.platform.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
            title: title.clone(),
            state: initial_session_state(&title),
            summary: String::new(),
            history: Vec::new(),
            pending_operation: None,
            last_todo_query: None,
            last_todo_action: None,
            last_memory_query: None,
            extra: Map::new(),
        };
        let tx = conn.transaction().map_err(SessionError::from_sql)?;
        upsert_session_tx(&tx, &session)?;
        replace_messages_tx(&tx, &session)?;
        if set_active {
            set_active_session_id_tx(&tx, &meta.scope_key, &session.session_id, &now)?;
        }
        tx.commit().map_err(SessionError::from_sql)?;
        Ok(session)
    }

    fn save_unlocked(
        &self,
        conn: &mut Connection,
        session: &mut SessionRecord,
        touch: bool,
    ) -> Result<(), SessionError> {
        normalize_session(session);
        if touch {
            session.updated_at = now_iso_cn();
        }
        let tx = conn.transaction().map_err(SessionError::from_sql)?;
        upsert_session_tx(&tx, session)?;
        replace_messages_tx(&tx, session)?;
        tx.commit().map_err(SessionError::from_sql)
    }
}

impl SessionRecord {
    /// 追加一条消息到会话历史（仅允许 user 和 assistant 角色）。
    /// 内容会自动脱敏。
    pub fn append_message(&mut self, role: &str, content: &str) {
        if !matches!(role, "user" | "assistant") {
            return;
        }
        self.history.push(SessionMessage {
            role: role.to_owned(),
            content: redact_sensitive_text(content),
            ts: now_iso_cn(),
        });
    }

    /// 重置会话上下文：清空历史、摘要、状态和挂起操作，保留会话元信息。
    pub fn reset(&mut self) {
        self.summary.clear();
        self.state.clear();
        self.history.clear();
        self.pending_operation = None;
        self.last_todo_query = None;
        self.last_todo_action = None;
        self.last_memory_query = None;
    }

    /// 记录最近一次真正展示给用户的 Todo 列表快照。
    ///
    /// `result_ids` 必须与最终展示顺序完全一致；后续“第一条 / 第二条 / 它”
    /// 等续指只允许按这份快照映射，不能回退数据库默认顺序。
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
    /// 成功 0 条时保持原值，成功 1 条时记录该条，成功多条时清空，避免“刚才那个”歧义。
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

    /// 当物理删除命中最近对象时清空该快照，避免 session 持续引用已不存在条目。
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

impl SessionMeta {
    /// 创建会话元信息，自动推断作用域类型（guild_channel / group / private）。
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

impl SessionError {
    /// 获取错误码。
    pub fn code(&self) -> &str {
        self.code
    }

    /// 获取错误消息。
    pub fn message(&self) -> &str {
        &self.message
    }

    fn encode(message: impl Into<String>) -> Self {
        Self {
            code: "encode_error",
            message: message.into(),
        }
    }

    fn decode(message: impl Into<String>) -> Self {
        Self {
            code: "decode_error",
            message: message.into(),
        }
    }

    fn data(message: impl Into<String>) -> Self {
        Self {
            code: "data_error",
            message: message.into(),
        }
    }

    fn from_database(err: DatabaseError) -> Self {
        Self {
            code: "database_error",
            message: format!("sqlite database failed: {}", err.message()),
        }
    }

    fn from_sql(err: rusqlite::Error) -> Self {
        Self {
            code: "database_error",
            message: format!("sqlite session failed: {err}"),
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SessionError {}

fn upsert_session_tx(tx: &Transaction<'_>, session: &SessionRecord) -> Result<(), SessionError> {
    let state_json = encode_json(&session.state, "session state")?;
    let pending_operation_json = encode_optional_json(&session.pending_operation, "pending")?;
    let last_todo_query_json = encode_optional_json(&session.last_todo_query, "last todo query")?;
    let last_todo_action_json =
        encode_optional_json(&session.last_todo_action, "last todo action")?;
    let last_memory_query_json =
        encode_optional_json(&session.last_memory_query, "last memory query")?;
    let extra_json = encode_json(&session.extra, "session extra")?;
    tx.execute(
        "INSERT INTO sessions (
            session_id, scope, scope_key, user_id, group_id, guild_id, channel_id, platform,
            created_at, updated_at, title, state_json, summary, pending_operation_json,
            last_todo_query_json, last_todo_action_json, last_memory_query_json, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(session_id) DO UPDATE SET
            scope = excluded.scope,
            scope_key = excluded.scope_key,
            user_id = excluded.user_id,
            group_id = excluded.group_id,
            guild_id = excluded.guild_id,
            channel_id = excluded.channel_id,
            platform = excluded.platform,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            title = excluded.title,
            state_json = excluded.state_json,
            summary = excluded.summary,
            pending_operation_json = excluded.pending_operation_json,
            last_todo_query_json = excluded.last_todo_query_json,
            last_todo_action_json = excluded.last_todo_action_json,
            last_memory_query_json = excluded.last_memory_query_json,
            extra_json = excluded.extra_json",
        params![
            session.session_id.as_str(),
            session.scope.as_str(),
            session.scope_key.as_str(),
            session.user_id.as_deref(),
            session.group_id.as_deref(),
            session.guild_id.as_deref(),
            session.channel_id.as_deref(),
            session.platform.as_str(),
            session.created_at.as_str(),
            session.updated_at.as_str(),
            session.title.as_str(),
            state_json,
            session.summary.as_str(),
            pending_operation_json,
            last_todo_query_json,
            last_todo_action_json,
            last_memory_query_json,
            extra_json,
        ],
    )
    .map_err(SessionError::from_sql)?;
    Ok(())
}

fn replace_messages_tx(tx: &Transaction<'_>, session: &SessionRecord) -> Result<(), SessionError> {
    tx.execute(
        "DELETE FROM session_messages WHERE session_id = ?1",
        params![session.session_id.as_str()],
    )
    .map_err(SessionError::from_sql)?;
    for (index, message) in session.history.iter().enumerate() {
        tx.execute(
            "INSERT INTO session_messages (session_id, message_index, role, content, ts)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.session_id.as_str(),
                index as i64,
                message.role.as_str(),
                message.content.as_str(),
                message.ts.as_str(),
            ],
        )
        .map_err(SessionError::from_sql)?;
    }
    Ok(())
}

fn set_active_session_id_conn(
    conn: &Connection,
    scope_key: &str,
    session_id: &str,
    now: &str,
) -> Result<(), SessionError> {
    conn.execute(
        "INSERT INTO session_active (scope_key, session_id, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(scope_key) DO UPDATE SET
            session_id = excluded.session_id,
            updated_at = excluded.updated_at",
        params![scope_key, session_id, now],
    )
    .map_err(SessionError::from_sql)?;
    Ok(())
}

fn set_active_session_id_tx(
    tx: &Transaction<'_>,
    scope_key: &str,
    session_id: &str,
    now: &str,
) -> Result<(), SessionError> {
    tx.execute(
        "INSERT INTO session_active (scope_key, session_id, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(scope_key) DO UPDATE SET
            session_id = excluded.session_id,
            updated_at = excluded.updated_at",
        params![scope_key, session_id, now],
    )
    .map_err(SessionError::from_sql)?;
    Ok(())
}
fn collect_sql_rows<T, F>(rows: rusqlite::MappedRows<'_, F>) -> Result<Vec<T>, SessionError>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
{
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SessionError::from_sql)
}

/// 获取当前北京时间 ISO8601 字符串。
pub fn now_iso_cn() -> String {
    time_context::now_iso_cn()
}

#[cfg(test)]
mod tests;
