//! conversation 级骰子规则偏好门面。
//!
//! Respond 只消费这里返回的领域快照和设置结果，不直接解析规则标识或访问持久化 Store。

use crate::storage::database::{DatabaseError, SqliteDatabase};

use super::{RollRuleSystem, storage::RollPreferenceStore};

/// 当前 conversation 的骰子规则投影。
///
/// 用户可见的规则名称、默认骰和判定说明在领域内成组维护，避免 Respond 重复枚举规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RollPreferenceSnapshot {
    rule_system: RollRuleSystem,
}

impl RollPreferenceSnapshot {
    fn new(rule_system: RollRuleSystem) -> Self {
        Self { rule_system }
    }

    pub(crate) const fn rule_system(self) -> RollRuleSystem {
        self.rule_system
    }

    pub(crate) const fn display_name(self) -> &'static str {
        self.rule_system.display_name()
    }

    pub(crate) const fn default_die_sides(self) -> u8 {
        self.rule_system.default_die_sides()
    }

    pub(crate) const fn comparison_summary(self) -> &'static str {
        match self.rule_system {
            RollRuleSystem::Dnd => "点数 ≥ DC 时成功",
            RollRuleSystem::Coc => "点数 ≤ 目标值时成功",
        }
    }

    pub(crate) const fn setting_effect(self) -> &'static str {
        match self.rule_system {
            RollRuleSystem::Dnd => "裸 d 使用 D20；Entertainment DM 中点数达到或超过 DC 时成功。",
            RollRuleSystem::Coc => "裸 d 使用 D100；Entertainment DM 中点数不高于目标值时成功。",
        }
    }
}

/// 设置规则偏好的领域结果；无效规则不会触发持久化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RollPreferenceSetOutcome {
    Updated(RollPreferenceSnapshot),
    UnsupportedRuleSystem,
}

/// Roll 领域偏好门面，统一规则校验、查询和持久化。
#[derive(Debug, Clone)]
pub struct RollPreferenceService {
    store: RollPreferenceStore,
}

impl RollPreferenceService {
    pub fn new(database: SqliteDatabase) -> Self {
        Self {
            store: RollPreferenceStore::new(database),
        }
    }

    /// 查询当前 conversation 的规则；未设置时由存储层返回兼容默认值 DND/D20。
    pub(crate) fn query(
        &self,
        scope_key: &str,
    ) -> Result<RollPreferenceSnapshot, RollPreferenceError> {
        self.store.get(scope_key).map(RollPreferenceSnapshot::new)
    }

    /// 校验并设置 conversation 级规则系统。
    pub(crate) fn set_rule_system(
        &self,
        scope_key: &str,
        value: &str,
    ) -> Result<RollPreferenceSetOutcome, RollPreferenceError> {
        let Some(rule_system) = RollRuleSystem::parse(value) else {
            return Ok(RollPreferenceSetOutcome::UnsupportedRuleSystem);
        };
        self.store.set(scope_key, rule_system)?;
        Ok(RollPreferenceSetOutcome::Updated(
            RollPreferenceSnapshot::new(rule_system),
        ))
    }
}

/// 识别 SealDice 的动作式 `.set dnd|coc`，并返回稳定规则 key 供通用设置命令规范化。
pub(crate) fn normalize_rule_system_setting(value: &str) -> Option<&'static str> {
    RollRuleSystem::parse(value).map(RollRuleSystem::key)
}

#[derive(Debug, Clone)]
pub(crate) struct RollPreferenceError {
    code: &'static str,
    message: String,
}

impl RollPreferenceError {
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(super) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }

    pub(super) fn invalid_data(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_data",
            message: message.into(),
        }
    }

    pub(super) fn from_database(error: DatabaseError) -> Self {
        Self {
            code: error.code(),
            message: error.message().to_owned(),
        }
    }

    pub(super) fn from_sql(error: rusqlite::Error) -> Self {
        Self {
            code: "io_error",
            message: format!("sqlite failed: {error}"),
        }
    }
}

impl std::fmt::Display for RollPreferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RollPreferenceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tools::roll::ROLL_PREFERENCE_SCHEMA_V1;

    fn service() -> RollPreferenceService {
        let database =
            SqliteDatabase::open_temp("roll-preference-service", &[ROLL_PREFERENCE_SCHEMA_V1])
                .expect("roll preference database should open");
        RollPreferenceService::new(database)
    }

    #[test]
    fn validates_and_projects_rule_system_settings() {
        let service = service();
        let scope_key = "platform:qq_official:account:a1:group:g1";

        assert_eq!(
            service.set_rule_system(scope_key, "other").unwrap(),
            RollPreferenceSetOutcome::UnsupportedRuleSystem
        );
        let RollPreferenceSetOutcome::Updated(snapshot) =
            service.set_rule_system(scope_key, " COC ").unwrap()
        else {
            panic!("supported rule system should be updated");
        };
        assert_eq!(snapshot.display_name(), "CoC（D100）");
        assert_eq!(snapshot.comparison_summary(), "点数 ≤ 目标值时成功");
        assert_eq!(service.query(scope_key).unwrap(), snapshot);
    }
}
