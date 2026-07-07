//! 通用 SQLite 数据库基础设施。
//!
//! 该模块只负责数据库文件、连接生命周期、通用 PRAGMA 和 migration 执行。
//! 业务表结构由各业务模块提供 migration 定义，避免通用层反向依赖 RSS/Todo 等语义。

use std::{
    fmt, fs,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
};

use rusqlite::{
    Connection,
    functions::{Context, FunctionFlags},
};
use serde_json::Value;
use thiserror::Error;

/// SQLite 连接池默认大小。
///
/// 这是本地 SQLite 连接数，独立于 LLM / Web Search 的上游并发限制；
/// 运行时可通过 `QQ_MAID_DB_POOL_MAX_SIZE` 覆盖。
pub const DEFAULT_SQLITE_POOL_SIZE: usize = 8;
pub const MIN_SQLITE_POOL_SIZE: usize = 1;
pub const MAX_SQLITE_POOL_SIZE: usize = 32;

/// 单个 SQLite migration。
///
/// 当前 migration 约定为幂等 SQL；通用初始化流程会在每次启动时统一执行，
/// 因此业务模块不得在运行时方法里自行建表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteMigration {
    pub name: &'static str,
    pub sql: &'static str,
}

#[derive(Debug, Clone)]
pub struct SqliteDatabase {
    inner: Arc<SqliteDatabaseInner>,
}

#[derive(Debug)]
struct SqliteDatabaseInner {
    path: PathBuf,
    pool_size: usize,
    connections: Mutex<Vec<Connection>>,
    available: Condvar,
}

pub struct PooledSqliteConnection {
    connection: Option<Connection>,
    database: Arc<SqliteDatabaseInner>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct DatabaseError {
    code: &'static str,
    message: String,
}

impl SqliteDatabase {
    /// 打开数据库文件并执行通用初始化。
    pub fn open(
        db_path: impl Into<PathBuf>,
        migrations: &[SqliteMigration],
    ) -> Result<Self, DatabaseError> {
        Self::open_with_pool_size(db_path, migrations, DEFAULT_SQLITE_POOL_SIZE)
    }

    /// 打开数据库文件，串行执行 migration 后创建固定大小的连接池。
    pub fn open_with_pool_size(
        db_path: impl Into<PathBuf>,
        migrations: &[SqliteMigration],
        pool_size: usize,
    ) -> Result<Self, DatabaseError> {
        if !(MIN_SQLITE_POOL_SIZE..=MAX_SQLITE_POOL_SIZE).contains(&pool_size) {
            return Err(DatabaseError::io(format!(
                "sqlite pool size must be between {MIN_SQLITE_POOL_SIZE} and {MAX_SQLITE_POOL_SIZE}"
            )));
        }

        let db_path = db_path.into();
        ensure_parent_dir(&db_path)?;
        let mut migration_connection = open_configured_connection(&db_path)?;
        run_migrations(&mut migration_connection, migrations)?;
        drop(migration_connection);

        let mut connections = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            connections.push(open_configured_connection(&db_path)?);
        }

        Ok(Self {
            inner: Arc::new(SqliteDatabaseInner {
                path: db_path,
                pool_size,
                connections: Mutex::new(connections),
                available: Condvar::new(),
            }),
        })
    }

    /// 从连接池借出 SQLite 连接。
    ///
    /// guard 释放时连接会归还池中；调用方不得把它跨 `.await` 保存。
    pub fn connection(&self) -> Result<PooledSqliteConnection, DatabaseError> {
        let mut connections = self
            .inner
            .connections
            .lock()
            .map_err(|_| DatabaseError::io("sqlite connection pool lock poisoned"))?;
        loop {
            if let Some(connection) = connections.pop() {
                return Ok(PooledSqliteConnection {
                    connection: Some(connection),
                    database: Arc::clone(&self.inner),
                });
            }
            connections = self
                .inner
                .available
                .wait(connections)
                .map_err(|_| DatabaseError::io("sqlite connection pool lock poisoned"))?;
        }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn pool_size(&self) -> usize {
        self.inner.pool_size
    }

    #[cfg(test)]
    fn idle_connection_count(&self) -> usize {
        self.inner.connections.lock().unwrap().len()
    }

    #[cfg(test)]
    pub fn open_temp(prefix: &str, migrations: &[SqliteMigration]) -> Result<Self, DatabaseError> {
        Self::open(
            std::env::temp_dir().join(format!("{prefix}-{}.db", uuid::Uuid::new_v4())),
            migrations,
        )
    }
}

impl fmt::Debug for PooledSqliteConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PooledSqliteConnection")
            .field("path", &self.database.path)
            .finish_non_exhaustive()
    }
}

impl Deref for PooledSqliteConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("pooled sqlite connection missing before drop")
    }
}

impl DerefMut for PooledSqliteConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection
            .as_mut()
            .expect("pooled sqlite connection missing before drop")
    }
}

impl Drop for PooledSqliteConnection {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        let mut connections = match self.database.connections.lock() {
            Ok(connections) => connections,
            Err(poisoned) => poisoned.into_inner(),
        };
        connections.push(connection);
        self.database.available.notify_one();
    }
}

impl DatabaseError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            code: "io_error",
            message: message.into(),
        }
    }

    fn migration(name: &str, err: rusqlite::Error) -> Self {
        Self {
            code: "migration_error",
            message: format!("sqlite migration `{name}` failed: {err}"),
        }
    }

    pub(crate) fn from_sql(err: rusqlite::Error) -> Self {
        Self::io(format!("sqlite failed: {err}"))
    }
}

fn ensure_parent_dir(path: &Path) -> Result<(), DatabaseError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| DatabaseError::io(format!("failed to create sqlite db dir: {err}")))?;
    }
    Ok(())
}

fn open_configured_connection(path: &Path) -> Result<Connection, DatabaseError> {
    let connection = Connection::open(path).map_err(DatabaseError::from_sql)?;
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(conn: &Connection) -> Result<(), DatabaseError> {
    register_json_remove_object_keys_function(conn)?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 3000;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(DatabaseError::from_sql)
}

fn register_json_remove_object_keys_function(conn: &Connection) -> Result<(), DatabaseError> {
    conn.create_scalar_function(
        "qq_maid_json_remove_object_keys",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        json_remove_object_keys,
    )
    .map_err(DatabaseError::from_sql)
}

fn json_remove_object_keys(ctx: &Context<'_>) -> rusqlite::Result<String> {
    let raw_json = ctx.get::<String>(0)?;
    let keys = ctx.get::<String>(1)?;
    Ok(remove_json_object_keys(&raw_json, &keys))
}

fn remove_json_object_keys(raw_json: &str, keys: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(raw_json) else {
        return raw_json.to_owned();
    };
    let Some(object) = value.as_object_mut() else {
        return raw_json.to_owned();
    };
    for key in keys.lines().map(str::trim).filter(|key| !key.is_empty()) {
        object.remove(key);
    }
    serde_json::to_string(&value).unwrap_or_else(|_| raw_json.to_owned())
}

fn run_migrations(
    conn: &mut Connection,
    migrations: &[SqliteMigration],
) -> Result<(), DatabaseError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_sqlite_migrations (
            name TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )
    .map_err(DatabaseError::from_sql)?;
    for migration in migrations {
        let tx = conn.transaction().map_err(DatabaseError::from_sql)?;
        let already_applied = tx
            .query_row(
                "SELECT 1 FROM app_sqlite_migrations WHERE name = ?1",
                [migration.name],
                |_| Ok(()),
            )
            .is_ok();
        if !already_applied {
            tx.execute_batch(migration.sql)
                .map_err(|err| DatabaseError::migration(migration.name, err))?;
            tx.execute(
                "INSERT INTO app_sqlite_migrations (name) VALUES (?1)",
                [migration.name],
            )
            .map_err(DatabaseError::from_sql)?;
        }
        tx.commit().map_err(DatabaseError::from_sql)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MIGRATIONS: &[SqliteMigration] = &[SqliteMigration {
        name: "test_schema",
        sql: "CREATE TABLE IF NOT EXISTS test_items (id TEXT PRIMARY KEY, value TEXT NOT NULL);",
    }];

    #[test]
    fn opens_database_and_replays_idempotent_migrations() {
        let path =
            std::env::temp_dir().join(format!("qq-maid-sqlite-test-{}.db", uuid::Uuid::new_v4()));
        let db = SqliteDatabase::open(&path, TEST_MIGRATIONS).unwrap();
        db.connection()
            .unwrap()
            .execute(
                "INSERT INTO test_items (id, value) VALUES (?1, ?2)",
                rusqlite::params!["a", "first"],
            )
            .unwrap();
        drop(db);

        let reopened = SqliteDatabase::open(&path, TEST_MIGRATIONS).unwrap();
        let value: String = reopened
            .connection()
            .unwrap()
            .query_row("SELECT value FROM test_items WHERE id = 'a'", [], |row| {
                row.get(0)
            })
            .unwrap();

        assert_eq!(value, "first");
    }

    #[test]
    fn creates_configured_connection_pool_after_migration() {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-sqlite-pool-test-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = SqliteDatabase::open_with_pool_size(&path, TEST_MIGRATIONS, 2).unwrap();

        assert_eq!(db.pool_size(), 2);
        assert_eq!(db.idle_connection_count(), 2);

        let first = db.connection().unwrap();
        let second = db.connection().unwrap();
        assert_eq!(db.idle_connection_count(), 0);

        first
            .execute(
                "INSERT INTO test_items (id, value) VALUES (?1, ?2)",
                rusqlite::params!["pooled", "ok"],
            )
            .unwrap();
        let value: String = second
            .query_row(
                "SELECT value FROM test_items WHERE id = 'pooled'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "ok");

        drop(first);
        drop(second);
        assert_eq!(db.idle_connection_count(), 2);
    }

    #[test]
    fn every_pooled_connection_has_common_initialization() {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-sqlite-config-test-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = SqliteDatabase::open_with_pool_size(&path, TEST_MIGRATIONS, 2).unwrap();
        let first = db.connection().unwrap();
        let second = db.connection().unwrap();

        for conn in [&first, &second] {
            let foreign_keys: i64 = conn
                .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                .unwrap();
            let busy_timeout: i64 = conn
                .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
                .unwrap();
            let synchronous: i64 = conn
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            let cleaned: String = conn
                .query_row(
                    "SELECT qq_maid_json_remove_object_keys(?1, ?2)",
                    rusqlite::params![r#"{"keep":1,"remove":2}"#, "remove"],
                    |row| row.get(0),
                )
                .unwrap();

            assert_eq!(foreign_keys, 1);
            assert_eq!(busy_timeout, 3000);
            assert_eq!(synchronous, 1);
            assert_eq!(cleaned, r#"{"keep":1}"#);
        }
    }

    #[test]
    fn rejects_pool_size_outside_supported_range() {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-sqlite-zero-pool-{}.db",
            uuid::Uuid::new_v4()
        ));
        let err = SqliteDatabase::open_with_pool_size(&path, TEST_MIGRATIONS, 0).unwrap_err();

        assert_eq!(err.code(), "io_error");
        assert!(err.message().contains("between 1 and 32"));

        let err = SqliteDatabase::open_with_pool_size(&path, TEST_MIGRATIONS, 33).unwrap_err();
        assert_eq!(err.code(), "io_error");
        assert!(err.message().contains("between 1 and 32"));
    }

    #[test]
    fn reports_migration_failure_with_name() {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-sqlite-bad-migration-{}.db",
            uuid::Uuid::new_v4()
        ));
        let err = SqliteDatabase::open(
            &path,
            &[SqliteMigration {
                name: "broken_schema",
                sql: "CREATE TABLE broken (",
            }],
        )
        .unwrap_err();

        assert_eq!(err.code(), "migration_error");
        assert!(err.message().contains("broken_schema"));
    }

    #[test]
    fn json_remove_object_keys_preserves_other_keys_without_json1() {
        let cleaned = remove_json_object_keys(
            r#"{"current_speaker_hint":"旧","current_topic":"保留","custom":{"x":1}}"#,
            "current_speaker_hint\nmissing",
        );
        let value = serde_json::from_str::<Value>(&cleaned).unwrap();

        assert!(value.get("current_speaker_hint").is_none());
        assert_eq!(value["current_topic"], "保留");
        assert_eq!(value["custom"]["x"], 1);
    }

    #[test]
    fn json_remove_object_keys_leaves_invalid_or_non_object_json_unchanged() {
        assert_eq!(remove_json_object_keys("{bad", "a"), "{bad");
        assert_eq!(remove_json_object_keys("[]", "a"), "[]");
    }
}
