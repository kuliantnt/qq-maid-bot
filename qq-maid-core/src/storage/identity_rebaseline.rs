//! 旧 QQ 业务归属键一次性归一。
//!
//! 历史库里的 Memory、Todo、RSS 和 Session 使用 `private:*` / `group:*` 裸 scope。
//! 这些字段是业务归属键，不是平台发送目标；启动时用 QQ AppID 补齐 account 维度，
//! 让运行时代码只处理新的跨平台 scope/owner。RSS 的 `target_id` 和 Notification 的
//! PushTarget 不参与迁移，继续保存平台原始投递 ID。

use std::path::PathBuf;

use rusqlite::{Transaction, params};

use crate::storage::{
    database::{DatabaseError, SqliteDatabase},
    migrations::APP_MIGRATIONS,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityRebaselineReport {
    pub sessions: usize,
    pub session_active: usize,
    pub memories: usize,
    pub todos: usize,
    pub rss_subscriptions: usize,
    pub rss_duplicates_removed: usize,
}

impl IdentityRebaselineReport {
    pub fn changed(&self) -> bool {
        self.sessions
            + self.session_active
            + self.memories
            + self.todos
            + self.rss_subscriptions
            + self.rss_duplicates_removed
            > 0
    }
}

/// 将历史 QQ 裸 scope/owner 归一为带 platform/account 的业务隔离键。
///
/// 该函数应在统一入口读取 Gateway 配置后、Core runtime 构建前执行；普通 SQLite
/// schema migration 拿不到 QQ AppID，不能在 `APP_MIGRATIONS` 中硬写 account。
pub fn rebaseline_qq_official_identity(
    db_path: impl Into<PathBuf>,
    qq_app_id: &str,
) -> Result<IdentityRebaselineReport, DatabaseError> {
    let account_id = qq_app_id.trim();
    if account_id.is_empty() {
        return Ok(IdentityRebaselineReport::default());
    }
    let database = SqliteDatabase::open(db_path, APP_MIGRATIONS)?;
    let mut conn = database.connection()?;
    let tx = conn.transaction().map_err(DatabaseError::from_sql)?;
    let report = rebaseline_qq_official_identity_tx(&tx, account_id)?;
    tx.commit().map_err(DatabaseError::from_sql)?;
    Ok(report)
}

fn rebaseline_qq_official_identity_tx(
    tx: &Transaction<'_>,
    account_id: &str,
) -> Result<IdentityRebaselineReport, DatabaseError> {
    let rss_duplicates_removed = remove_duplicate_legacy_rss(tx, account_id)?;
    Ok(IdentityRebaselineReport {
        sessions: rebaseline_sessions(tx, account_id)?,
        session_active: rebaseline_session_active(tx, account_id)?,
        memories: rebaseline_memories(tx, account_id)?,
        todos: rebaseline_todos(tx, account_id)?,
        rss_subscriptions: rebaseline_rss(tx, account_id)?,
        rss_duplicates_removed,
    })
}

fn rebaseline_sessions(tx: &Transaction<'_>, account_id: &str) -> Result<usize, DatabaseError> {
    let private = tx
        .execute(
            "UPDATE sessions
             SET scope_key = ?1 || substr(scope_key, 9),
                 pending_operation_json = NULL,
                 last_todo_query_json = NULL,
                 last_todo_action_json = NULL,
                 last_memory_query_json = NULL
             WHERE platform = 'qq_official'
               AND scope_key LIKE 'private:%'",
            params![private_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    let group = tx
        .execute(
            "UPDATE sessions
             SET scope_key = ?1 || substr(scope_key, 7),
                 pending_operation_json = NULL,
                 last_todo_query_json = NULL,
                 last_todo_action_json = NULL,
                 last_memory_query_json = NULL
             WHERE platform = 'qq_official'
               AND scope_key LIKE 'group:%'",
            params![group_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    Ok(private + group)
}

fn rebaseline_session_active(
    tx: &Transaction<'_>,
    account_id: &str,
) -> Result<usize, DatabaseError> {
    remove_duplicate_session_active(tx, account_id)?;
    let private = tx
        .execute(
            "UPDATE session_active
             SET scope_key = ?1 || substr(scope_key, 9)
             WHERE scope_key LIKE 'private:%'",
            params![private_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    let group = tx
        .execute(
            "UPDATE session_active
             SET scope_key = ?1 || substr(scope_key, 7)
             WHERE scope_key LIKE 'group:%'",
            params![group_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    Ok(private + group)
}

fn remove_duplicate_session_active(
    tx: &Transaction<'_>,
    account_id: &str,
) -> Result<(), DatabaseError> {
    tx.execute(
        "DELETE FROM session_active
         WHERE scope_key LIKE 'private:%'
           AND EXISTS (
               SELECT 1 FROM session_active newer
               WHERE newer.scope_key = ?1 || substr(session_active.scope_key, 9)
           )",
        params![private_prefix(account_id)],
    )
    .map_err(DatabaseError::from_sql)?;
    tx.execute(
        "DELETE FROM session_active
         WHERE scope_key LIKE 'group:%'
           AND EXISTS (
               SELECT 1 FROM session_active newer
               WHERE newer.scope_key = ?1 || substr(session_active.scope_key, 7)
           )",
        params![group_prefix(account_id)],
    )
    .map_err(DatabaseError::from_sql)?;
    Ok(())
}

fn rebaseline_memories(tx: &Transaction<'_>, account_id: &str) -> Result<usize, DatabaseError> {
    let personal = tx
        .execute(
            "UPDATE memories
             SET scope_id = ?1 || scope_id
             WHERE scope_type = 'personal'
               AND scope_id IS NOT NULL
               AND trim(scope_id) <> ''
               AND scope_id NOT LIKE 'platform:%'",
            params![private_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    let group = tx
        .execute(
            "UPDATE memories
             SET scope_id = ?1 || scope_id
             WHERE scope_type = 'group'
               AND scope_id IS NOT NULL
               AND trim(scope_id) <> ''
               AND scope_id NOT LIKE 'platform:%'",
            params![group_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    Ok(personal + group)
}

fn rebaseline_todos(tx: &Transaction<'_>, account_id: &str) -> Result<usize, DatabaseError> {
    let private = tx
        .execute(
            "UPDATE todos
             SET scope_key = ?1 || substr(scope_key, 9),
                 owner_key = CASE
                     WHEN user_id IS NOT NULL AND trim(user_id) <> ''
                     THEN (?1 || substr(scope_key, 9) || ':actor:' || trim(user_id))
                     ELSE (?1 || substr(scope_key, 9))
                 END
             WHERE scope_key LIKE 'private:%'",
            params![private_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    let group = tx
        .execute(
            "UPDATE todos
             SET scope_key = ?1 || substr(scope_key, 7),
                 owner_key = CASE
                     WHEN user_id IS NOT NULL AND trim(user_id) <> ''
                     THEN (?1 || substr(scope_key, 7) || ':actor:' || trim(user_id))
                     ELSE (?1 || substr(scope_key, 7))
                 END
             WHERE scope_key LIKE 'group:%'",
            params![group_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    Ok(private + group)
}

fn remove_duplicate_legacy_rss(
    tx: &Transaction<'_>,
    account_id: &str,
) -> Result<usize, DatabaseError> {
    let private = tx
        .execute(
            "DELETE FROM rss_subscriptions
             WHERE scope_key LIKE 'private:%'
               AND EXISTS (
                   SELECT 1 FROM rss_subscriptions newer
                   WHERE newer.scope_key = ?1 || rss_subscriptions.target_id
                     AND newer.url = rss_subscriptions.url
               )",
            params![private_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    let group = tx
        .execute(
            "DELETE FROM rss_subscriptions
             WHERE scope_key LIKE 'group:%'
               AND EXISTS (
                   SELECT 1 FROM rss_subscriptions newer
                   WHERE newer.scope_key = ?1 || rss_subscriptions.target_id
                     AND newer.url = rss_subscriptions.url
               )",
            params![group_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    Ok(private + group)
}

fn rebaseline_rss(tx: &Transaction<'_>, account_id: &str) -> Result<usize, DatabaseError> {
    let private = tx
        .execute(
            "UPDATE rss_subscriptions
             SET scope_key = ?1 || target_id
             WHERE target_type = 'private'
               AND scope_key LIKE 'private:%'",
            params![private_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    let group = tx
        .execute(
            "UPDATE rss_subscriptions
             SET scope_key = ?1 || target_id
             WHERE target_type = 'group'
               AND scope_key LIKE 'group:%'",
            params![group_prefix(account_id)],
        )
        .map_err(DatabaseError::from_sql)?;
    Ok(private + group)
}

fn private_prefix(account_id: &str) -> String {
    stable_prefix(account_id, "private")
}

fn group_prefix(account_id: &str) -> String {
    stable_prefix(account_id, "group")
}

fn stable_prefix(account_id: &str, target_type: &str) -> String {
    format!("platform:qq_official:account:{account_id}:{target_type}:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::SqliteDatabase;

    #[test]
    fn rebaseline_updates_business_scope_without_rewriting_rss_target() {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-identity-rebaseline-{}.db",
            uuid::Uuid::new_v4()
        ));
        let database = SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap();
        {
            let conn = database.connection().unwrap();
            conn.execute(
                "INSERT INTO memories (
                    id, created_at, updated_at, memory_type, scope, user_id, group_id,
                    content, source_text, scope_type, scope_id, created_by_user_id
                 ) VALUES ('m1', 'now', NULL, 'note', 'general', 'u1', NULL,
                    '记住这个', 'source', 'personal', 'u1', 'u1')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO todos (
                    owner_key, user_id, scope_key, title, time_precision, status, completed,
                    created_at, updated_at
                 ) VALUES ('u1', 'u1', 'private:u1', '提醒我', 'none', 'pending', 0, 'now', 'now')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO rss_subscriptions (
                    id, target_type, target_id, scope_key, url, title, enabled, created_at,
                    initialized, consecutive_failures
                 ) VALUES ('r1', 'group', 'g1', 'group:g1', 'https://example.test/feed.xml',
                    'Feed', 1, 'now', 1, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (
                    session_id, scope, scope_key, user_id, group_id, guild_id, channel_id,
                    platform, created_at, updated_at, title, state_json, summary,
                    pending_operation_json, last_todo_query_json, last_memory_query_json,
                    extra_json, last_todo_action_json
                 ) VALUES ('s1', 'private', 'private:u1', 'u1', NULL, NULL, NULL,
                    'qq_official', 'now', 'now', '旧会话', '{}', '',
                    '{\"legacy\":true}', '{\"legacy\":true}', '{\"legacy\":true}', '{}',
                    '{\"legacy\":true}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_active (scope_key, session_id, updated_at)
                 VALUES ('private:u1', 's1', 'now')",
                [],
            )
            .unwrap();
        }
        drop(database);

        let report = rebaseline_qq_official_identity(&path, "app-1").unwrap();

        assert!(report.changed());
        let database = SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap();
        let conn = database.connection().unwrap();
        let memory_scope: String = conn
            .query_row("SELECT scope_id FROM memories WHERE id = 'm1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            memory_scope,
            "platform:qq_official:account:app-1:private:u1"
        );
        let (todo_owner, todo_scope): (String, String) = conn
            .query_row("SELECT owner_key, scope_key FROM todos", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(
            todo_owner,
            "platform:qq_official:account:app-1:private:u1:actor:u1"
        );
        assert_eq!(todo_scope, "platform:qq_official:account:app-1:private:u1");
        let (rss_target, rss_scope): (String, String) = conn
            .query_row(
                "SELECT target_id, scope_key FROM rss_subscriptions WHERE id = 'r1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(rss_target, "g1");
        assert_eq!(rss_scope, "platform:qq_official:account:app-1:group:g1");
        let pending: Option<String> = conn
            .query_row(
                "SELECT pending_operation_json FROM sessions WHERE session_id = 's1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(pending.is_none());
    }
}
