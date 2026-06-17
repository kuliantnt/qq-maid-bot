//! 通用 SQLite 数据库基础设施。
//!
//! 该模块只负责数据库文件、连接生命周期、通用 PRAGMA 和 migration 执行。
//! 业务表结构由各业务模块提供 migration 定义，避免通用层反向依赖 RSS/Todo 等语义。

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use rusqlite::Connection;
use thiserror::Error;

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
    connection: Mutex<Connection>,
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
        let db_path = db_path.into();
        ensure_parent_dir(&db_path)?;
        let mut connection = Connection::open(&db_path).map_err(DatabaseError::from_sql)?;
        configure_connection(&connection)?;
        run_migrations(&mut connection, migrations)?;
        Ok(Self {
            inner: Arc::new(SqliteDatabaseInner {
                path: db_path,
                connection: Mutex::new(connection),
            }),
        })
    }

    /// 获取共享 SQLite 连接。
    ///
    /// 当前机器人是单实例低并发场景，使用单连接加互斥锁可以保持 RSS 命令和后台轮询
    /// 的写入顺序，同时避免业务模块重复打开数据库或自行配置 PRAGMA。
    pub fn connection(&self) -> Result<MutexGuard<'_, Connection>, DatabaseError> {
        self.inner
            .connection
            .lock()
            .map_err(|_| DatabaseError::io("sqlite connection lock poisoned"))
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    #[cfg(test)]
    pub fn open_temp(prefix: &str, migrations: &[SqliteMigration]) -> Result<Self, DatabaseError> {
        Self::open(
            std::env::temp_dir().join(format!("{prefix}-{}.db", uuid::Uuid::new_v4())),
            migrations,
        )
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

    fn from_sql(err: rusqlite::Error) -> Self {
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

fn configure_connection(conn: &Connection) -> Result<(), DatabaseError> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 3000;",
    )
    .map_err(DatabaseError::from_sql)
}

fn run_migrations(
    conn: &mut Connection,
    migrations: &[SqliteMigration],
) -> Result<(), DatabaseError> {
    for migration in migrations {
        conn.execute_batch(migration.sql)
            .map_err(|err| DatabaseError::migration(migration.name, err))?;
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
}
