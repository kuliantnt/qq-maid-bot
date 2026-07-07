//! 手动展示名（manual display name）存储。
//!
//! 平台侧“获取成员信息 / 获取群成员昵称”接口不可用后，机器人无法可靠取得用户昵称。
//! 本模块按稳定身份绑定用户在某个会话空间内手动设置的展示名，仅用于显示和帮助 LLM
//! 理解上下文，**不参与权限判断、owner 或稳定身份认证**。
//!
//! 作用域隔离复用 [`crate::identity::conversation_scope_key`] 产出的 scope_key：
//! - 群聊：`platform + account + group + user_id`
//! - 私聊：`platform + account + private + user_id`
//!
//! 同一用户在不同群 / 私聊中各自独立，互不污染；缺少稳定 `user_id` 时不允许设置。

use rusqlite::{Connection, OptionalExtension, params};

use crate::storage::{
    database::{DatabaseError, SqliteDatabase, SqliteMigration},
    session::now_iso_cn,
};

/// 手动展示名表 migration，由应用启动时的通用数据库初始化流程统一执行。
///
/// 表以 `(scope_key, user_id)` 作为唯一键，对应一个会话空间内的单个用户；
/// `scope_key` 是 conversation scope（不含 actor），保证群 A 设置不污染群 B。
/// SQL 保持幂等，启动期可安全重放。
pub const MANUAL_DISPLAY_NAMES_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "manual_display_names_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS manual_display_names (
            scope_key TEXT NOT NULL,
            user_id TEXT NOT NULL,
            display_name TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (scope_key, user_id)
        );
        CREATE INDEX IF NOT EXISTS idx_manual_display_names_scope
            ON manual_display_names(scope_key);",
};

/// 手动展示名存储错误。
///
/// 仅暴露错误码与消息，不泄露稳定身份或展示名原文给调用方日志以外的地方。
#[derive(Debug, Clone)]
pub struct DisplayNameError {
    code: &'static str,
    message: String,
}

impl DisplayNameError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            code: "io_error",
            message: message.into(),
        }
    }

    fn from_database(err: DatabaseError) -> Self {
        Self {
            code: err.code(),
            message: err.message().to_owned(),
        }
    }

    fn from_sql(err: rusqlite::Error) -> Self {
        Self::io(format!("sqlite failed: {err}"))
    }
}

impl std::fmt::Display for DisplayNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DisplayNameError {}

/// 手动展示名存储器，复用应用级 SQLite 句柄。
///
/// 不自行建表或读取数据库路径，所有 SQL 经项目级 migration 注册后执行。
#[derive(Debug, Clone)]
pub struct DisplayNameStore {
    database: SqliteDatabase,
}

impl DisplayNameStore {
    /// 创建一个新的 `DisplayNameStore`，复用应用级 SQLite 句柄。
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, DisplayNameError> {
        self.database
            .connection()
            .map_err(DisplayNameError::from_database)
    }

    /// 设置或覆盖某个会话空间内某用户的手动展示名。
    ///
    /// `scope_key` 必须是 conversation scope（不含 actor），`user_id` 必须是非空稳定身份。
    /// 调用方负责校验展示名长度、换行和空白，本方法只做最基本的非空校验。
    pub fn set(
        &self,
        scope_key: &str,
        user_id: &str,
        display_name: &str,
    ) -> Result<(), DisplayNameError> {
        let scope_key = scope_key.trim();
        let user_id = user_id.trim();
        let display_name = display_name.trim();
        if scope_key.is_empty() {
            return Err(DisplayNameError::bad_request("缺少有效的会话作用域"));
        }
        if user_id.is_empty() {
            return Err(DisplayNameError::bad_request(
                "缺少稳定身份，无法绑定展示名",
            ));
        }
        if display_name.is_empty() {
            return Err(DisplayNameError::bad_request("展示名不能为空"));
        }

        let conn = self.connection()?;
        let now = now_iso_cn();
        conn.execute(
            "INSERT INTO manual_display_names (scope_key, user_id, display_name, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(scope_key, user_id) DO UPDATE SET
                 display_name = excluded.display_name,
                 updated_at = excluded.updated_at",
            params![scope_key, user_id, display_name, now],
        )
        .map_err(DisplayNameError::from_sql)?;
        Ok(())
    }

    /// 查询某个会话空间内某用户的手动展示名。
    ///
    /// 未设置或查询失败时返回 `None`，调用方按 fallback 优先级继续兜底。
    pub fn get(&self, scope_key: &str, user_id: &str) -> Result<Option<String>, DisplayNameError> {
        let scope_key = scope_key.trim();
        let user_id = user_id.trim();
        if scope_key.is_empty() || user_id.is_empty() {
            return Ok(None);
        }
        let conn = self.connection()?;
        conn.query_row(
            "SELECT display_name FROM manual_display_names
             WHERE scope_key = ?1 AND user_id = ?2",
            params![scope_key, user_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(DisplayNameError::from_sql)
    }

    /// 清除某个会话空间内某用户的手动展示名。
    ///
    /// 返回是否实际删除了一行；未设置时返回 `false`，调用方据此决定提示文案。
    pub fn unset(&self, scope_key: &str, user_id: &str) -> Result<bool, DisplayNameError> {
        let scope_key = scope_key.trim();
        let user_id = user_id.trim();
        if scope_key.is_empty() || user_id.is_empty() {
            return Ok(false);
        }
        let conn = self.connection()?;
        let affected = conn
            .execute(
                "DELETE FROM manual_display_names
                 WHERE scope_key = ?1 AND user_id = ?2",
                params![scope_key, user_id],
            )
            .map_err(DisplayNameError::from_sql)?;
        Ok(affected > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> DisplayNameStore {
        let path =
            std::env::temp_dir().join(format!("qq-maid-display-name-{}.db", uuid::Uuid::new_v4()));
        let database = SqliteDatabase::open(path, &[MANUAL_DISPLAY_NAMES_SCHEMA_V1]).unwrap();
        DisplayNameStore::new(database)
    }

    #[test]
    fn set_get_unset_roundtrip() {
        let store = store();
        let scope = "platform:qq_official:account:app-1:group:group-1";
        assert!(store.get(scope, "u1").unwrap().is_none());

        store.set(scope, "u1", "脸脸").unwrap();
        assert_eq!(store.get(scope, "u1").unwrap().as_deref(), Some("脸脸"));

        // 覆盖写入
        store.set(scope, "u1", "小脸").unwrap();
        assert_eq!(store.get(scope, "u1").unwrap().as_deref(), Some("小脸"));

        assert!(store.unset(scope, "u1").unwrap());
        assert!(store.get(scope, "u1").unwrap().is_none());
        // 重复清除返回 false
        assert!(!store.unset(scope, "u1").unwrap());
    }

    #[test]
    fn scope_isolation_between_group_and_private() {
        let store = store();
        let group = "platform:qq_official:account:app-1:group:group-1";
        let private = "platform:qq_official:account:app-1:private:u1";
        store.set(group, "u1", "群昵称").unwrap();
        store.set(private, "u1", "私聊昵称").unwrap();
        assert_eq!(store.get(group, "u1").unwrap().as_deref(), Some("群昵称"));
        assert_eq!(
            store.get(private, "u1").unwrap().as_deref(),
            Some("私聊昵称")
        );
        // 清除群昵称不影响私聊
        store.unset(group, "u1").unwrap();
        assert!(store.get(group, "u1").unwrap().is_none());
        assert_eq!(
            store.get(private, "u1").unwrap().as_deref(),
            Some("私聊昵称")
        );
    }

    #[test]
    fn group_a_does_not_leak_to_group_b() {
        let store = store();
        let group_a = "platform:qq_official:account:app-1:group:group-a";
        let group_b = "platform:qq_official:account:app-1:group:group-b";
        store.set(group_a, "u1", "脸脸").unwrap();
        assert_eq!(store.get(group_a, "u1").unwrap().as_deref(), Some("脸脸"));
        assert!(store.get(group_b, "u1").unwrap().is_none());
    }

    #[test]
    fn rejects_empty_scope_or_user() {
        let store = store();
        assert!(store.set("", "u1", "脸脸").is_err());
        assert!(store.set("scope", "", "脸脸").is_err());
        assert!(store.set("scope", "u1", "  ").is_err());
        // 查询空参数不报错，只返回 None
        assert!(store.get("", "u1").unwrap().is_none());
        assert!(store.get("scope", "").unwrap().is_none());
        assert!(!store.unset("", "u1").unwrap());
    }

    #[test]
    fn trim_input_display_name() {
        let store = store();
        let scope = "platform:qq_official:account:app-1:group:group-1";
        store.set(scope, "u1", "  脸脸  ").unwrap();
        assert_eq!(store.get(scope, "u1").unwrap().as_deref(), Some("脸脸"));
    }
}
