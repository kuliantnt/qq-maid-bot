use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags};

use crate::{
    config::center::{
        ConfigCenter, ConfigCenterPaths, ConfigRegistry, ManagedConfigChange, ManagedConfigField,
        ManagedConfigFile, ManagedConfigSensitivity, SECRET_MISSING_REVISION, SecretConfigChange,
    },
    storage::database::SqliteDatabase,
};

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ConfigMigrationError {
    code: &'static str,
    message: String,
}

impl ConfigMigrationError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_migration_input", message)
    }

    fn io(context: &str, error: impl std::fmt::Display) -> Self {
        Self::new("migration_io_error", format!("{context}: {error}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMigrationKind {
    Public,
    Secret,
    Restricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMigrationAction {
    ImportManaged,
    ImportSecret,
    AlreadyManaged,
    KeepExternal,
    InvalidValue,
    NotPresent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMigrationEntry {
    pub key: String,
    pub env_name: String,
    pub source_file: Option<PathBuf>,
    pub source_name: Option<String>,
    pub kind: ConfigMigrationKind,
    pub action: ConfigMigrationAction,
}

#[derive(Debug, Clone)]
pub struct ConfigMigrationPlan {
    pub dotenv_files: Vec<PathBuf>,
    pub managed_revision: String,
    pub entries: Vec<ConfigMigrationEntry>,
}

#[derive(Debug, Clone)]
pub struct ConfigMigrationReport {
    pub managed_imported: usize,
    pub secrets_imported: usize,
    pub unchanged: usize,
    pub source_files_unchanged: Vec<PathBuf>,
}

#[derive(Clone)]
struct DotenvValue {
    value: String,
    source_file: PathBuf,
}

/// 盘点 dotenv 中已登记字段。报告只包含 key、来源文件和动作，不包含任何配置值。
pub fn plan_config_migration(
    fields: Vec<ManagedConfigField>,
    managed_config_file: &Path,
    database_file: &Path,
    dotenv_files: &[PathBuf],
) -> Result<ConfigMigrationPlan, ConfigMigrationError> {
    let registry = ConfigRegistry::new(fields)
        .map_err(|error| ConfigMigrationError::invalid(error.to_string()))?;
    let managed = ManagedConfigFile::new(managed_config_file.to_path_buf(), registry.clone())
        .load()
        .map_err(|error| ConfigMigrationError::invalid(error.to_string()))?;
    let dotenv = load_dotenv_files(dotenv_files)?;
    let secret_keys = configured_secret_keys(database_file)?;
    let mut entries = Vec::with_capacity(registry.fields().len());

    for field in registry.fields() {
        let source = std::iter::once(field.env_name)
            .chain(field.env_aliases.iter().copied())
            .find_map(|name| dotenv.get(name).map(|value| (name, value)));
        let kind = match field.sensitivity {
            ManagedConfigSensitivity::Public => ConfigMigrationKind::Public,
            ManagedConfigSensitivity::Secret => ConfigMigrationKind::Secret,
            ManagedConfigSensitivity::Restricted => ConfigMigrationKind::Restricted,
        };
        let action = match source {
            None => ConfigMigrationAction::NotPresent,
            Some(_) if field.sensitivity == ManagedConfigSensitivity::Restricted => {
                ConfigMigrationAction::KeepExternal
            }
            Some(_)
                if managed.values.contains_key(field.key) || secret_keys.contains(field.key) =>
            {
                ConfigMigrationAction::AlreadyManaged
            }
            Some((_, source)) if field.sensitivity == ManagedConfigSensitivity::Public => {
                match registry
                    .parse_environment_value(field, &source.value)
                    .and_then(|value| registry.validate_managed_value(field, &value))
                {
                    Ok(()) => ConfigMigrationAction::ImportManaged,
                    Err(_) => ConfigMigrationAction::InvalidValue,
                }
            }
            Some((_, source)) if source.value.trim().is_empty() => {
                ConfigMigrationAction::InvalidValue
            }
            Some(_) => ConfigMigrationAction::ImportSecret,
        };
        entries.push(ConfigMigrationEntry {
            key: field.key.to_owned(),
            env_name: field.env_name.to_owned(),
            source_file: source.map(|(_, value)| value.source_file.clone()),
            source_name: source.map(|(name, _)| name.to_owned()),
            kind,
            action,
        });
    }

    Ok(ConfigMigrationPlan {
        dotenv_files: dotenv_files.to_vec(),
        managed_revision: managed.revision,
        entries,
    })
}

/// 导入只填补受管配置中的空缺；已有 Web/TOML 值和密文永远不会被静默覆盖。
/// 原 dotenv 文件不修改、不删除，重复执行会稳定变为 `AlreadyManaged`。
pub fn apply_config_migration(
    fields: Vec<ManagedConfigField>,
    paths: ConfigCenterPaths,
    database: SqliteDatabase,
    database_file: &Path,
    dotenv_files: &[PathBuf],
) -> Result<ConfigMigrationReport, ConfigMigrationError> {
    let plan = plan_config_migration(
        fields.clone(),
        &paths.managed_config_file,
        database_file,
        dotenv_files,
    )?;
    if plan
        .entries
        .iter()
        .any(|entry| entry.action == ConfigMigrationAction::InvalidValue)
    {
        return Err(ConfigMigrationError::invalid(
            "dotenv contains invalid registered values; fix them before applying migration",
        ));
    }
    let registry = ConfigRegistry::new(fields.clone())
        .map_err(|error| ConfigMigrationError::invalid(error.to_string()))?;
    let managed_config_file = paths.managed_config_file.clone();
    let center = ConfigCenter::open(fields, paths, database)
        .map_err(|error| ConfigMigrationError::invalid(error.to_string()))?
        .with_incomplete_setup_writes();
    // ConfigCenter 会在缺失时创建空 runtime.toml；重新盘点一次可取得新 revision，
    // 也能避免盘点与提交之间的并发写把已有受管值当作空缺覆盖。
    let plan = plan_config_migration(
        registry.fields().to_vec(),
        &managed_config_file,
        database_file,
        dotenv_files,
    )?;
    let dotenv = load_dotenv_files(dotenv_files)?;
    let mut managed_changes = Vec::new();
    let mut secret_changes = Vec::new();
    for entry in &plan.entries {
        let Some(source_name) = entry.source_name.as_deref() else {
            continue;
        };
        let source = dotenv.get(source_name).ok_or_else(|| {
            ConfigMigrationError::invalid("dotenv changed while migration was being prepared")
        })?;
        match entry.action {
            ConfigMigrationAction::ImportManaged => {
                let field = registry
                    .require(&entry.key)
                    .map_err(|error| ConfigMigrationError::invalid(error.to_string()))?;
                let value = registry
                    .parse_environment_value(field, &source.value)
                    .map_err(|error| ConfigMigrationError::invalid(error.to_string()))?;
                managed_changes.push(ManagedConfigChange::Set {
                    key: entry.key.clone(),
                    value,
                });
            }
            ConfigMigrationAction::ImportSecret => {
                secret_changes.push(SecretConfigChange::Replace {
                    key: entry.key.clone(),
                    value: source.value.clone(),
                    expected_revision: SECRET_MISSING_REVISION.to_owned(),
                });
            }
            _ => {}
        }
    }

    // 文件和 SQLite 无法共享事务；先提交普通文件，再批量事务化密文。任一步失败都明确报错，
    // 重跑只会继续填补剩余空缺，不会覆盖已经成功的部分或原 dotenv。
    if !managed_changes.is_empty() {
        center
            .update_managed(&plan.managed_revision, &managed_changes)
            .map_err(|error| ConfigMigrationError::invalid(error.to_string()))?;
    }
    if !secret_changes.is_empty() {
        center
            .update_secrets(&secret_changes)
            .map_err(|error| ConfigMigrationError::invalid(error.to_string()))?;
    }
    Ok(ConfigMigrationReport {
        managed_imported: managed_changes.len(),
        secrets_imported: secret_changes.len(),
        unchanged: plan.entries.len() - managed_changes.len() - secret_changes.len(),
        source_files_unchanged: dotenv_files.to_vec(),
    })
}

fn load_dotenv_files(
    dotenv_files: &[PathBuf],
) -> Result<HashMap<String, DotenvValue>, ConfigMigrationError> {
    let mut values = HashMap::new();
    for path in dotenv_files {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(ConfigMigrationError::io("inspect dotenv file", error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ConfigMigrationError::invalid(format!(
                "dotenv source must be a regular file and not a symbolic link: {}",
                path.display()
            )));
        }
        let iterator = dotenvy::from_path_iter(path).map_err(|error| {
            ConfigMigrationError::invalid(format!("invalid dotenv file: {error}"))
        })?;
        for item in iterator {
            let (name, value) = item.map_err(|error| {
                ConfigMigrationError::invalid(format!("invalid dotenv entry: {error}"))
            })?;
            values.entry(name).or_insert_with(|| DotenvValue {
                value,
                source_file: path.clone(),
            });
        }
    }
    Ok(values)
}

fn configured_secret_keys(database_file: &Path) -> Result<HashSet<String>, ConfigMigrationError> {
    if !database_file.exists() {
        return Ok(HashSet::new());
    }
    let metadata = fs::symlink_metadata(database_file)
        .map_err(|error| ConfigMigrationError::io("inspect SQLite file", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ConfigMigrationError::invalid(
            "SQLite path must be a regular file and not a symbolic link",
        ));
    }
    let connection = Connection::open_with_flags(database_file, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| {
        ConfigMigrationError::io("open SQLite for migration inventory", error)
    })?;
    let table_exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='config_secrets')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| ConfigMigrationError::io("inspect secret schema", error))?;
    if !table_exists {
        return Ok(HashSet::new());
    }
    let mut statement = connection
        .prepare("SELECT key FROM config_secrets")
        .map_err(|error| ConfigMigrationError::io("read configured secret keys", error))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| ConfigMigrationError::io("query configured secret keys", error))?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|error| ConfigMigrationError::io("decode configured secret keys", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::center::{ManagedConfigApplyMode, ManagedConfigValueType},
        storage::APP_MIGRATIONS,
    };

    fn fields() -> Vec<ManagedConfigField> {
        vec![
            ManagedConfigField::public(
                "feature.enabled",
                "FEATURE_ENABLED",
                "test",
                ManagedConfigValueType::Boolean,
                ManagedConfigApplyMode::Restart,
                Some("false"),
            ),
            ManagedConfigField::secret(
                "provider.key",
                "PROVIDER_KEY",
                "test",
                ManagedConfigApplyMode::Restart,
            )
            .with_env_aliases(&["OLD_PROVIDER_KEY"]),
            ManagedConfigField::restricted(
                "bootstrap.database",
                "APP_DB_FILE",
                "test",
                ManagedConfigValueType::String,
                ManagedConfigApplyMode::Restart,
                None,
            ),
        ]
    }

    #[test]
    fn dry_run_and_apply_are_redacted_idempotent_and_keep_source_file() {
        let root =
            std::env::temp_dir().join(format!("qq-maid-config-migration-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("config/secrets")).unwrap();
        let dotenv = root.join("config/.env");
        fs::write(
            &dotenv,
            "FEATURE_ENABLED=true\nOLD_PROVIDER_KEY=do-not-print\nAPP_DB_FILE=data/app.db\n",
        )
        .unwrap();
        let database_file = root.join("data/app.db");
        let database = SqliteDatabase::open(&database_file, APP_MIGRATIONS).unwrap();
        let paths = ConfigCenterPaths {
            managed_config_file: root.join("config/runtime.toml"),
            master_key_file: root.join("config/secrets/master.key"),
        };

        let plan = plan_config_migration(
            fields(),
            &paths.managed_config_file,
            &database_file,
            std::slice::from_ref(&dotenv),
        )
        .unwrap();
        assert!(plan.entries.iter().any(|entry| {
            entry.key == "provider.key"
                && entry.action == ConfigMigrationAction::ImportSecret
                && entry.source_name.as_deref() == Some("OLD_PROVIDER_KEY")
        }));
        assert!(plan.entries.iter().any(|entry| {
            entry.key == "bootstrap.database" && entry.action == ConfigMigrationAction::KeepExternal
        }));

        let report = apply_config_migration(
            fields(),
            paths.clone(),
            database.clone(),
            &database_file,
            std::slice::from_ref(&dotenv),
        )
        .unwrap();
        assert_eq!(report.managed_imported, 1);
        assert_eq!(report.secrets_imported, 1);
        assert!(dotenv.exists());
        assert!(
            fs::read_to_string(&dotenv)
                .unwrap()
                .contains("do-not-print")
        );

        let repeated = plan_config_migration(
            fields(),
            &paths.managed_config_file,
            &database_file,
            std::slice::from_ref(&dotenv),
        )
        .unwrap();
        assert!(repeated.entries.iter().all(|entry| {
            !matches!(
                entry.action,
                ConfigMigrationAction::ImportManaged | ConfigMigrationAction::ImportSecret
            )
        }));
        let _ = fs::remove_dir_all(root);
    }
}
