//! 会话（Session）存储模块。
//!
//! 会话状态使用项目级 SQLite 数据库持久化，并通过 `SessionStore` 继续暴露
//! 原有的整条会话读写接口。业务层仍然操作 `SessionRecord`，存储层负责把
//! 会话元信息、活跃会话映射和消息顺序拆分保存到数据库中。

pub use qq_maid_common::redaction::redact_sensitive_text;
use qq_maid_common::time_context;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

use crate::storage::database::{SqliteDatabase, SqliteMigration};

mod error;
mod jsonio;
mod model;
mod normalize;
mod row;
mod write;

pub use error::SessionError;
pub use model::{
    LastMemoryQuery, LastTodoAction, LastTodoQuery, SessionMessage, SessionMeta, SessionRecord,
};
use normalize::{
    build_session_id, infer_scope, initial_session_state, normalize_session,
    normalize_session_title,
};
use row::load_session_unlocked;
use write::{
    collect_sql_rows, replace_messages_tx, set_active_session_id_conn, set_active_session_id_tx,
    upsert_session_tx,
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

/// 会话存储器，基于项目通用 SQLite 连接实现。
///
/// 数据库连接由应用启动时统一打开并执行 migration；SessionStore 不再读取
/// Session 专用目录，也不兼容旧 JSON 文件，SQLite 是会话状态的事实来源。
#[derive(Debug, Clone)]
pub struct SessionStore {
    database: SqliteDatabase,
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

/// 获取当前北京时间 ISO8601 字符串。
pub fn now_iso_cn() -> String {
    time_context::now_iso_cn()
}

#[cfg(test)]
mod tests;
