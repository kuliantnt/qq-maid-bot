//! Ops 入站执行领取存储。
//!
//! 该表只保存稳定入站键、非敏感短任务 ID 和执行状态，不保存命令参数、Codex
//! prompt、平台用户/群 ID 或输出正文。唯一约束提供跨并发请求和进程重启的原子领取。

use rusqlite::{OptionalExtension, params};

use crate::storage::{
    database::{DatabaseError, SqliteDatabase, SqliteMigration},
    session::now_iso_cn,
};

pub const OPS_EXECUTION_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "ops_execution_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS ops_executions (
            inbound_key TEXT PRIMARY KEY,
            task_id TEXT NOT NULL UNIQUE,
            command_name TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
          );
          CREATE INDEX IF NOT EXISTS idx_ops_executions_updated
              ON ops_executions(updated_at);",
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsExecutionClaim {
    pub task_id: String,
    pub command_name: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Existing(OpsExecutionClaim),
    TaskIdCollision,
}

#[derive(Clone)]
pub struct OpsExecutionStore {
    database: SqliteDatabase,
}

impl OpsExecutionStore {
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    pub fn claim(
        &self,
        inbound_key: &str,
        task_id: &str,
        command_name: &str,
    ) -> Result<ClaimOutcome, OpsStorageError> {
        let conn = self.connection()?;
        let now = now_iso_cn();
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO ops_executions (
                    inbound_key, task_id, command_name, status, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, 'accepted', ?4, ?4)",
                params![inbound_key, task_id, command_name, now],
            )
            .map_err(OpsStorageError::from_sql)?;
        if changed == 1 {
            return Ok(ClaimOutcome::Claimed);
        }
        if let Some(existing) = self.get_by_inbound_key(inbound_key)? {
            return Ok(ClaimOutcome::Existing(existing));
        }
        Ok(ClaimOutcome::TaskIdCollision)
    }

    pub fn get_by_inbound_key(
        &self,
        inbound_key: &str,
    ) -> Result<Option<OpsExecutionClaim>, OpsStorageError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT task_id, command_name, status FROM ops_executions WHERE inbound_key = ?1",
            [inbound_key],
            |row| {
                Ok(OpsExecutionClaim {
                    task_id: row.get(0)?,
                    command_name: row.get(1)?,
                    status: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(OpsStorageError::from_sql)
    }

    pub fn mark_status(&self, task_id: &str, status: &str) -> Result<(), OpsStorageError> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE ops_executions SET status = ?2, updated_at = ?3 WHERE task_id = ?1",
            params![task_id, status, now_iso_cn()],
        )
        .map_err(OpsStorageError::from_sql)?;
        Ok(())
    }

    pub fn release_unstarted(
        &self,
        inbound_key: &str,
        task_id: &str,
    ) -> Result<(), OpsStorageError> {
        let conn = self.connection()?;
        conn.execute(
            "DELETE FROM ops_executions
             WHERE inbound_key = ?1 AND task_id = ?2 AND status = 'accepted'",
            params![inbound_key, task_id],
        )
        .map_err(OpsStorageError::from_sql)?;
        Ok(())
    }

    fn connection(
        &self,
    ) -> Result<crate::storage::database::PooledSqliteConnection, OpsStorageError> {
        self.database
            .connection()
            .map_err(OpsStorageError::from_database)
    }
}

#[derive(Debug, Clone)]
pub struct OpsStorageError {
    code: &'static str,
    message: String,
}

impl OpsStorageError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn from_database(error: DatabaseError) -> Self {
        Self {
            code: error.code(),
            message: error.message().to_owned(),
        }
    }

    fn from_sql(error: rusqlite::Error) -> Self {
        Self {
            code: "io_error",
            message: format!("sqlite failed: {error}"),
        }
    }
}

impl std::fmt::Display for OpsStorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for OpsStorageError {}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    #[test]
    fn concurrent_claims_for_same_inbound_key_have_one_winner() {
        let database = SqliteDatabase::open_temp("ops-claim", &[OPS_EXECUTION_SCHEMA_V1]).unwrap();
        let store = OpsExecutionStore::new(database);
        let barrier = Arc::new(Barrier::new(2));
        let handles = ["ops-first", "ops-second"].map(|task_id| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.claim("same-inbound", task_id, "codex").unwrap()
            })
        });
        let outcomes = handles.map(|handle| handle.join().unwrap());

        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ClaimOutcome::Claimed))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ClaimOutcome::Existing(_)))
                .count(),
            1
        );
    }
}
