//! 骰子规则偏好持久化。
//!
//! 默认骰面数是 conversation 级规则状态：群聊成员共享同一设置，私聊则自然隔离到
//! 当前会话目标。Roll domain 只保存稳定的规则系统标识，不从 scope_key 反解析平台地址。

use rusqlite::{OptionalExtension, params};

use crate::storage::{
    database::{SqliteDatabase, SqliteMigration},
    session::now_iso_cn,
};

use super::{RollPreferenceError, RollRuleSystem};

/// 骰子规则偏好表，由应用统一 migration 流程在启动时创建。
pub const ROLL_PREFERENCE_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "roll_preference_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS roll_preferences (
            scope_key TEXT PRIMARY KEY,
            rule_system TEXT NOT NULL CHECK (rule_system IN ('dnd', 'coc')),
            updated_at TEXT NOT NULL
          );",
};

#[derive(Debug, Clone)]
pub(super) struct RollPreferenceStore {
    database: SqliteDatabase,
}

impl RollPreferenceStore {
    pub(super) fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    /// 读取当前 conversation 的规则设置；未设置时保持兼容默认值 DND/D20。
    pub(super) fn get(&self, scope_key: &str) -> Result<RollRuleSystem, RollPreferenceError> {
        let scope_key = validate_scope_key(scope_key)?;
        let connection = self.connection()?;
        let stored = connection
            .query_row(
                "SELECT rule_system FROM roll_preferences WHERE scope_key = ?1",
                [scope_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(RollPreferenceError::from_sql)?;
        stored.map_or(Ok(RollRuleSystem::default()), |value| {
            RollRuleSystem::parse(&value)
                .ok_or_else(|| RollPreferenceError::invalid_data("数据库中的骰子规则系统无效"))
        })
    }

    /// 设置 conversation 级规则系统；骰面数和判定方向由规则系统确定性派生。
    pub(super) fn set(
        &self,
        scope_key: &str,
        rule_system: RollRuleSystem,
    ) -> Result<(), RollPreferenceError> {
        let scope_key = validate_scope_key(scope_key)?;
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO roll_preferences (scope_key, rule_system, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(scope_key) DO UPDATE SET
                    rule_system = excluded.rule_system,
                    updated_at = excluded.updated_at",
                params![scope_key, rule_system.key(), now_iso_cn()],
            )
            .map_err(RollPreferenceError::from_sql)?;
        Ok(())
    }

    fn connection(
        &self,
    ) -> Result<crate::storage::database::PooledSqliteConnection, RollPreferenceError> {
        self.database
            .connection()
            .map_err(RollPreferenceError::from_database)
    }
}

fn validate_scope_key(scope_key: &str) -> Result<&str, RollPreferenceError> {
    let scope_key = scope_key.trim();
    if scope_key.is_empty() {
        return Err(RollPreferenceError::bad_request(
            "缺少有效的会话作用域，无法设置骰子规则",
        ));
    }
    Ok(scope_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> RollPreferenceStore {
        let database = SqliteDatabase::open_temp("roll-preference", &[ROLL_PREFERENCE_SCHEMA_V1])
            .expect("roll preference database should open");
        RollPreferenceStore::new(database)
    }

    #[test]
    fn defaults_to_dnd_and_persists_each_scope_independently() {
        let store = store();
        let group = "platform:qq_official:account:a1:group:g1";
        let private = "platform:qq_official:account:a1:private:u1";

        assert_eq!(store.get(group).unwrap(), RollRuleSystem::Dnd);
        store.set(group, RollRuleSystem::Coc).unwrap();
        assert_eq!(store.get(group).unwrap(), RollRuleSystem::Coc);
        assert_eq!(store.get(private).unwrap(), RollRuleSystem::Dnd);

        store.set(group, RollRuleSystem::Dnd).unwrap();
        assert_eq!(store.get(group).unwrap(), RollRuleSystem::Dnd);
    }

    #[test]
    fn rejects_an_empty_scope_key() {
        let store = store();
        assert_eq!(store.get(" ").unwrap_err().code(), "bad_request");
        assert_eq!(
            store.set("", RollRuleSystem::Coc).unwrap_err().code(),
            "bad_request"
        );
    }
}
