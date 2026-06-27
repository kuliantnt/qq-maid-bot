//! 会话（Session）存储模块。
//!
//! 会话状态使用项目级 SQLite 数据库持久化，并通过 `SessionStore` 继续暴露
//! 原有的整条会话读写接口。业务层仍然操作 `SessionRecord`，存储层负责把
//! 会话元信息、活跃会话映射和消息顺序拆分保存到数据库中。

use std::fmt;

use regex::Regex;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    runtime::pending::PendingOperation,
    storage::database::{DatabaseError, SqliteDatabase, SqliteMigration},
    util::time_context,
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

pub const SESSION_MIGRATIONS: &[SqliteMigration] = &[SESSION_SCHEMA_V1];

/// 默认会话标题，当用户未指定标题时使用。
pub const DEFAULT_SESSION_TITLE: &str = "未命名会话";

/// 敏感信息匹配模式列表，存储会话时自动脱敏。
static SENSITIVE_PATTERNS: std::sync::LazyLock<Vec<(Regex, &'static str)>> =
    std::sync::LazyLock::new(|| {
        vec![
            (
                Regex::new(r"(?i)(OPENAI_API_KEY\s*=\s*)\S+").unwrap(),
                "$1<redacted>",
            ),
            (
                Regex::new(r"(?i)(DEEPSEEK_API_KEY\s*=\s*)\S+").unwrap(),
                "$1<redacted>",
            ),
            (
                Regex::new(r"(?i)(QQ_SECRET\s*=\s*)\S+").unwrap(),
                "$1<redacted>",
            ),
            (
                Regex::new(r"(?i)(API[_ -]?KEY\s*[:=]\s*)\S+").unwrap(),
                "$1<redacted>",
            ),
            (
                Regex::new(r"(?i)(SECRET\s*[:=]\s*)\S+").unwrap(),
                "$1<redacted>",
            ),
            (
                Regex::new(r"(?i)(TOKEN\s*[:=]\s*)\S+").unwrap(),
                "$1<redacted>",
            ),
            (
                Regex::new(r"sk-[A-Za-z0-9_-]{20,}").unwrap(),
                "<redacted:openai_api_key>",
            ),
            (
                Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._-]{20,}").unwrap(),
                "Bearer <redacted>",
            ),
        ]
    });

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
}

/// 会话存储器，基于项目通用 SQLite 连接实现。
///
/// 数据库连接由应用启动时统一打开并执行 migration；SessionStore 不再读取
/// Session 专用目录，也不兼容旧 JSON 文件，SQLite 是会话状态的事实来源。
#[derive(Debug, Clone)]
pub struct SessionStore {
    database: SqliteDatabase,
}

#[derive(Debug)]
struct StoredSessionRow {
    session_id: String,
    scope: String,
    scope_key: String,
    user_id: Option<String>,
    group_id: Option<String>,
    guild_id: Option<String>,
    channel_id: Option<String>,
    platform: String,
    created_at: String,
    updated_at: String,
    title: String,
    state_json: String,
    summary: String,
    pending_operation_json: Option<String>,
    last_todo_query_json: Option<String>,
    last_memory_query_json: Option<String>,
    extra_json: String,
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

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SessionError> {
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
        self.last_memory_query = None;
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
        }
    }
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

/// 规范化会话记录，补全缺失字段。
fn normalize_session(session: &mut SessionRecord) {
    if session.session_id.trim().is_empty() {
        session.session_id = build_session_id(&session.scope_key);
    }
    if session.scope_key.trim().is_empty() {
        session.scope_key = "unknown".to_owned();
    }
    if session.scope.trim().is_empty() {
        session.scope = infer_scope(
            &session.scope_key,
            session.group_id.as_deref(),
            session.guild_id.as_deref(),
        );
    }
    if session.created_at.trim().is_empty() {
        session.created_at = now_iso_cn();
    }
    if session.updated_at.trim().is_empty() {
        session.updated_at = session.created_at.clone();
    }
    if session.platform.trim().is_empty() {
        session.platform = "qq".to_owned();
    }
    if session.title.trim().is_empty() {
        session.title = DEFAULT_SESSION_TITLE.to_owned();
    }
}

/// 根据 scope_key 前缀推断会话作用域类型。
fn infer_scope(scope_key: &str, group_id: Option<&str>, guild_id: Option<&str>) -> String {
    if scope_key.starts_with("guild:") || guild_id.is_some() {
        "guild_channel".to_owned()
    } else if scope_key.starts_with("group:") || group_id.is_some() {
        "group".to_owned()
    } else {
        "private".to_owned()
    }
}

/// 初始化会话状态，根据标题设置当前话题、活跃场景和预期模式。
fn initial_session_state(title: &str) -> Map<String, Value> {
    let mut state = Map::new();
    if title.trim().is_empty() || title.trim() == DEFAULT_SESSION_TITLE {
        return state;
    }
    state.insert(
        "current_topic".to_owned(),
        Value::String(title.trim().to_owned()),
    );
    state.insert(
        "active_scene".to_owned(),
        Value::String("默认会话".to_owned()),
    );
    state.insert(
        "expected_mode".to_owned(),
        Value::String("陪聊 + 轻量整理".to_owned()),
    );
    state
}

/// 规范化会话标题，空值时使用默认标题。
fn normalize_session_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        DEFAULT_SESSION_TITLE.to_owned()
    } else {
        title.to_owned()
    }
}

/// 生成会话 ID：时间戳 + 作用域键 + UUID 片段组合。
fn build_session_id(scope_key: &str) -> String {
    let timestamp = now_iso_cn()
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .take(14)
        .collect::<String>();
    format!(
        "{}-{}-{}",
        timestamp,
        safe_id_part(scope_key, "unknown"),
        &Uuid::new_v4().to_string()[..6]
    )
}

/// 将字符串转为安全的 ID 片段：仅保留字母数字、下划线、点、连字符。
fn safe_id_part(value: &str, fallback: &str) -> String {
    let safe = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches(&['-', '.', '_'][..])
        .to_owned();
    if safe.is_empty() {
        fallback.to_owned()
    } else {
        safe
    }
}

fn load_session_unlocked(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<SessionRecord>, SessionError> {
    let row = conn
        .query_row(
            "SELECT session_id, scope, scope_key, user_id, group_id, guild_id,
                    channel_id, platform, created_at, updated_at, title,
                    state_json, summary, pending_operation_json, last_todo_query_json,
                    last_memory_query_json, extra_json
             FROM sessions
             WHERE session_id = ?1",
            params![session_id],
            stored_session_row,
        )
        .optional()
        .map_err(SessionError::from_sql)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let messages = load_messages_unlocked(conn, &row.session_id)?;
    let mut session = row.into_record(messages)?;
    normalize_session(&mut session);
    Ok(Some(session))
}

fn load_messages_unlocked(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<SessionMessage>, SessionError> {
    let mut stmt = conn
        .prepare(
            "SELECT role, content, ts
             FROM session_messages
             WHERE session_id = ?1
             ORDER BY message_index ASC, id ASC",
        )
        .map_err(SessionError::from_sql)?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(SessionMessage {
                role: row.get(0)?,
                content: row.get(1)?,
                ts: row.get(2)?,
            })
        })
        .map_err(SessionError::from_sql)?;
    collect_sql_rows(rows)
}

fn upsert_session_tx(tx: &Transaction<'_>, session: &SessionRecord) -> Result<(), SessionError> {
    let state_json = encode_json(&session.state, "session state")?;
    let pending_operation_json = encode_optional_json(&session.pending_operation, "pending")?;
    let last_todo_query_json = encode_optional_json(&session.last_todo_query, "last todo query")?;
    let last_memory_query_json =
        encode_optional_json(&session.last_memory_query, "last memory query")?;
    let extra_json = encode_json(&session.extra, "session extra")?;
    tx.execute(
        "INSERT INTO sessions (
            session_id, scope, scope_key, user_id, group_id, guild_id, channel_id, platform,
            created_at, updated_at, title, state_json, summary, pending_operation_json,
            last_todo_query_json, last_memory_query_json, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
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

fn stored_session_row(row: &Row<'_>) -> rusqlite::Result<StoredSessionRow> {
    Ok(StoredSessionRow {
        session_id: row.get(0)?,
        scope: row.get(1)?,
        scope_key: row.get(2)?,
        user_id: row.get(3)?,
        group_id: row.get(4)?,
        guild_id: row.get(5)?,
        channel_id: row.get(6)?,
        platform: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        title: row.get(10)?,
        state_json: row.get(11)?,
        summary: row.get(12)?,
        pending_operation_json: row.get(13)?,
        last_todo_query_json: row.get(14)?,
        last_memory_query_json: row.get(15)?,
        extra_json: row.get(16)?,
    })
}

impl StoredSessionRow {
    fn into_record(self, history: Vec<SessionMessage>) -> Result<SessionRecord, SessionError> {
        Ok(SessionRecord {
            session_id: self.session_id,
            scope: self.scope,
            scope_key: self.scope_key,
            user_id: self.user_id,
            group_id: self.group_id,
            guild_id: self.guild_id,
            channel_id: self.channel_id,
            platform: self.platform,
            created_at: self.created_at,
            updated_at: self.updated_at,
            title: self.title,
            state: decode_json(&self.state_json, "session state")?,
            summary: self.summary,
            history,
            pending_operation: decode_optional_json(
                self.pending_operation_json.as_deref(),
                "pending operation",
            )?,
            last_todo_query: decode_optional_json(
                self.last_todo_query_json.as_deref(),
                "last todo query",
            )?,
            last_memory_query: decode_optional_json(
                self.last_memory_query_json.as_deref(),
                "last memory query",
            )?,
            extra: decode_json(&self.extra_json, "session extra")?,
        })
    }
}

fn encode_json<T: Serialize>(value: &T, field: &str) -> Result<String, SessionError> {
    serde_json::to_string(value)
        .map_err(|err| SessionError::encode(format!("failed to encode {field}: {err}")))
}

fn encode_optional_json<T: Serialize>(
    value: &Option<T>,
    field: &str,
) -> Result<Option<String>, SessionError> {
    value
        .as_ref()
        .map(|value| encode_json(value, field))
        .transpose()
}

fn decode_json<T: DeserializeOwned>(text: &str, field: &str) -> Result<T, SessionError> {
    serde_json::from_str(text)
        .map_err(|err| SessionError::decode(format!("failed to decode {field}: {err}")))
}

fn decode_optional_json<T: DeserializeOwned>(
    text: Option<&str>,
    field: &str,
) -> Result<Option<T>, SessionError> {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    serde_json::from_str(text)
        .map(Some)
        .map_err(|err| SessionError::decode(format!("failed to decode {field}: {err}")))
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

/// 脱敏文本中的敏感信息。
pub fn redact_sensitive_text(text: impl AsRef<str>) -> String {
    let mut redacted = text.as_ref().to_owned();
    for (pattern, replacement) in SENSITIVE_PATTERNS.iter() {
        redacted = pattern.replace_all(&redacted, *replacement).to_string();
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::pending::PendingMemory;

    fn test_store() -> SessionStore {
        SessionStore::new(
            SqliteDatabase::open_temp("qq-maid-session-test", SESSION_MIGRATIONS).unwrap(),
        )
    }

    fn test_meta() -> SessionMeta {
        SessionMeta::new(
            "group:g1",
            Some("u1".to_owned()),
            Some("g1".to_owned()),
            None,
            None,
            "qq_official",
        )
    }

    #[test]
    fn create_active_and_list_sessions_for_scope() {
        let store = test_store();
        let meta = test_meta();

        let mut first = store.create(&meta, "旧话题", true).unwrap();
        first.updated_at = "2026-06-01T10:00:00+08:00".to_owned();
        first.append_message("user", "hello");
        store.save(&mut first).unwrap();
        let second = store.create(&meta, "新话题", true).unwrap();

        let active = store.get_or_create_active(&meta).unwrap();
        assert_eq!(active.session_id, second.session_id);

        let sessions = store
            .list_for_scope("group:g1", Some(&second.session_id))
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "旧话题");
    }

    #[test]
    fn reset_keeps_session_but_clears_context() {
        let store = test_store();
        let meta = test_meta();
        let mut session = store.create(&meta, "话题", true).unwrap();
        session.summary = "摘要".to_owned();
        session.append_message("user", "hi");
        session.pending_operation = Some(PendingOperation::MemoryCreate {
            initiator_user_id: Some("u1".to_owned()),
            memory: PendingMemory {
                content: "新记忆".to_owned(),
                source_text: "/memory 新记忆".to_owned(),
                memory_type: "note".to_owned(),
                scope: "general".to_owned(),
                created_at: now_iso_cn(),
                target_scope_type: Some("personal".to_owned()),
                target_scope_id: Some("u1".to_owned()),
            },
        });

        session.reset();
        store.save(&mut session).unwrap();
        let reloaded = store.get_or_create_active(&meta).unwrap();

        assert!(reloaded.summary.is_empty());
        assert!(reloaded.history.is_empty());
        assert!(reloaded.pending_operation.is_none());
    }

    #[test]
    fn sqlite_reopen_restores_active_title_and_message_order() {
        let path =
            std::env::temp_dir().join(format!("qq-maid-session-reopen-{}.db", Uuid::new_v4()));
        let meta = test_meta();
        let first_db = SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap();
        let store = SessionStore::new(first_db);
        let mut session = store.create(&meta, "重启测试", true).unwrap();
        session.append_message("user", "第一条");
        session.append_message("assistant", "第二条");
        session.append_message("user", "第三条");
        store.save(&mut session).unwrap();
        let expected_id = session.session_id.clone();
        drop(store);

        let reopened = SessionStore::new(SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap());
        let restored = reopened.get_or_create_active(&meta).unwrap();

        assert_eq!(restored.session_id, expected_id);
        assert_eq!(restored.title, "重启测试");
        assert_eq!(
            restored
                .history
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["第一条", "第二条", "第三条"]
        );
    }

    #[test]
    fn compact_history_persists_summary_and_archive() {
        let store = test_store();
        let meta = test_meta();
        let mut session = store.create(&meta, "压缩测试", true).unwrap();
        for index in 0..6 {
            session.append_message("user", &format!("消息 {index}"));
        }

        store.compact_history(&mut session, "摘要", 2).unwrap();
        let reloaded = store.get_or_create_active(&meta).unwrap();

        assert_eq!(reloaded.summary, "摘要");
        assert_eq!(reloaded.history.len(), 2);
        assert!(
            reloaded
                .extra
                .get("archived_history")
                .and_then(Value::as_array)
                .is_some_and(|items| items.len() == 1)
        );
    }

    #[test]
    fn sqlite_reopen_restores_pending_and_last_queries() {
        let path =
            std::env::temp_dir().join(format!("qq-maid-session-json-fields-{}.db", Uuid::new_v4()));
        let meta = test_meta();
        let store = SessionStore::new(SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap());
        let mut session = store.create(&meta, "跨进程状态", true).unwrap();
        session.pending_operation = Some(PendingOperation::MemoryCreate {
            initiator_user_id: Some("u1".to_owned()),
            memory: PendingMemory {
                content: "需要确认的记忆".to_owned(),
                source_text: "/memory 需要确认的记忆".to_owned(),
                memory_type: "note".to_owned(),
                scope: "general".to_owned(),
                created_at: now_iso_cn(),
                target_scope_type: Some("personal".to_owned()),
                target_scope_id: Some("u1".to_owned()),
            },
        });
        session.last_todo_query = Some(LastTodoQuery {
            owner_key: "u1".to_owned(),
            query_type: "pending".to_owned(),
            condition: "全部".to_owned(),
            result_ids: vec!["1".to_owned(), "2".to_owned()],
            created_at: now_iso_cn(),
        });
        session.last_memory_query = Some(LastMemoryQuery {
            query_type: "list".to_owned(),
            condition: "全部".to_owned(),
            scope_type: Some("personal".to_owned()),
            scope_id: Some("u1".to_owned()),
            result_ids: vec!["m1".to_owned()],
            created_at: now_iso_cn(),
        });
        store.save(&mut session).unwrap();
        drop(store);

        let reopened = SessionStore::new(SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap());
        let restored = reopened.get_or_create_active(&meta).unwrap();

        assert_eq!(restored.pending_operation, session.pending_operation);
        assert_eq!(restored.last_todo_query, session.last_todo_query);
        assert_eq!(restored.last_memory_query, session.last_memory_query);
    }

    #[test]
    fn set_active_rejects_missing_session_without_changing_current() {
        let store = test_store();
        let meta = test_meta();
        let current = store.create(&meta, "当前", true).unwrap();

        let err = store
            .set_active_session_id(&meta.scope_key, "missing-session")
            .unwrap_err();

        assert_eq!(err.code(), "database_error");
        assert_eq!(
            store.get_or_create_active(&meta).unwrap().session_id,
            current.session_id
        );
    }

    #[test]
    fn broken_active_pointer_reports_data_error() {
        let database =
            SqliteDatabase::open_temp("qq-maid-session-broken-active", SESSION_MIGRATIONS).unwrap();
        let conn = database.connection().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO session_active (scope_key, session_id, updated_at)
             VALUES ('group:g1', 'missing-session', '2026-06-01T10:00:00+08:00');
             PRAGMA foreign_keys = ON;",
        )
        .unwrap();
        drop(conn);
        let store = SessionStore::new(database);

        let err = store.get_active(&test_meta()).unwrap_err();

        assert_eq!(err.code(), "data_error");
        assert!(err.message().contains("active session"));
    }

    #[test]
    fn session_record_defaults_still_deserialize_for_tests() {
        let mut session = serde_json::from_str::<SessionRecord>(
            r#"{
                "session_id": "legacy-session",
                "scope": "group",
                "scope_key": "group:g1",
                "created_at": "2026-06-01T10:00:00+08:00",
                "updated_at": "2026-06-01T10:00:00+08:00"
            }"#,
        )
        .unwrap();

        normalize_session(&mut session);

        assert_eq!(session.session_id, "legacy-session");
        assert!(session.last_todo_query.is_none());
    }
}
