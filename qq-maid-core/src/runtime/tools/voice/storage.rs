use rusqlite::{OptionalExtension, params};

use crate::storage::{
    database::{DatabaseError, SqliteDatabase, SqliteMigration},
    session::now_iso_cn,
};

use super::VoicePreferenceKey;

pub const VOICE_PREFERENCE_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "voice_preference_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS voice_preferences (
            platform TEXT NOT NULL,
            account_id TEXT NOT NULL,
            target_type TEXT NOT NULL CHECK (target_type IN ('private', 'group')),
            target_id TEXT NOT NULL,
            enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
            updated_at TEXT NOT NULL,
            PRIMARY KEY (platform, account_id, target_type, target_id)
          );",
};

#[derive(Clone)]
pub struct VoicePreferenceStore {
    database: SqliteDatabase,
}

impl VoicePreferenceStore {
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    pub fn is_enabled(&self, key: &VoicePreferenceKey) -> Result<bool, VoiceStorageError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT enabled FROM voice_preferences
                 WHERE platform = ?1 AND account_id = ?2
                   AND target_type = ?3 AND target_id = ?4",
                params![key.platform, key.account_id, key.target_type, key.target_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|value| value.unwrap_or_default() != 0)
            .map_err(VoiceStorageError::from_sql)
    }

    pub fn set_enabled(
        &self,
        key: &VoicePreferenceKey,
        enabled: bool,
    ) -> Result<(), VoiceStorageError> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO voice_preferences (
                    platform, account_id, target_type, target_id, enabled, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(platform, account_id, target_type, target_id)
                 DO UPDATE SET enabled = excluded.enabled, updated_at = excluded.updated_at",
                params![
                    key.platform,
                    key.account_id,
                    key.target_type,
                    key.target_id,
                    i64::from(enabled),
                    now_iso_cn(),
                ],
            )
            .map_err(VoiceStorageError::from_sql)?;
        Ok(())
    }

    fn connection(
        &self,
    ) -> Result<crate::storage::database::PooledSqliteConnection, VoiceStorageError> {
        self.database
            .connection()
            .map_err(VoiceStorageError::from_database)
    }
}

#[derive(Debug, Clone)]
pub struct VoiceStorageError {
    code: &'static str,
    message: String,
}

impl VoiceStorageError {
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

impl std::fmt::Display for VoiceStorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for VoiceStorageError {}
