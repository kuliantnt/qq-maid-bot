//! 受管运行配置中心。
//!
//! 普通字段写入专用 `runtime.toml`，敏感值认证加密后写入 SQLite；主密钥只保存在
//! 数据库外的独立文件。这里提供领域模型和安全写入能力，管理员认证与页面由后续任务接入。

mod managed_file;
mod registry;
mod secret;

#[cfg(test)]
mod tests;

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use qq_maid_common::managed_config::{
    ManagedConfigApplyMode, ManagedConfigSensitivity, ManagedConfigValueType,
};
use serde::Serialize;
use toml::Value;

use crate::storage::database::SqliteDatabase;

pub use managed_file::{ManagedConfigChange, ManagedConfigFile, ManagedConfigSnapshot};
pub use registry::ConfigRegistry;
pub use secret::{CONFIG_SECRET_SCHEMA_V1, SecretStore};

pub const RUNTIME_CONFIG_FILE_ENV: &str = "RUNTIME_CONFIG_FILE";
pub const MASTER_KEY_FILE_ENV: &str = "MASTER_KEY_FILE";
pub const DEFAULT_RUNTIME_CONFIG_PATH: &str = "config/runtime.toml";
pub const DEFAULT_MASTER_KEY_RELATIVE_PATH: &str = "secrets/master.key";

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ConfigCenterError {
    code: &'static str,
    message: String,
}

impl ConfigCenterError {
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

    fn io(message: impl Into<String>) -> Self {
        Self::new("config_io_error", message)
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_config", message)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new("config_conflict", message)
    }

    fn secret(message: impl Into<String>) -> Self {
        Self::new("secret_storage_error", message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigCenterPaths {
    pub managed_config_file: PathBuf,
    pub master_key_file: PathBuf,
}

impl ConfigCenterPaths {
    pub fn from_environment(environment: &HashMap<String, String>) -> Self {
        let managed_config_file = environment
            .get(RUNTIME_CONFIG_FILE_ENV)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_RUNTIME_CONFIG_PATH));
        let master_key_file = environment
            .get(MASTER_KEY_FILE_ENV)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                managed_config_file
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("config"))
                    .join(DEFAULT_MASTER_KEY_RELATIVE_PATH)
            });
        Self {
            managed_config_file,
            master_key_file,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigValueSource {
    Environment,
    ManagedToml,
    EncryptedSecret,
    Default,
    NotConfigured,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ConfigFieldSnapshot {
    pub key: String,
    pub module: String,
    pub value_type: ManagedConfigValueType,
    pub source: ConfigValueSource,
    pub overridden: bool,
    pub editable: bool,
    pub configured: bool,
    pub valid: bool,
    pub sensitivity: ManagedConfigSensitivity,
    pub apply_mode: qq_maid_common::managed_config::ManagedConfigApplyMode,
    /// 受管文件中已保存的普通值。敏感字段始终为 `None`。
    pub saved_value: Option<Value>,
    /// 按当前文件与外部覆盖计算出的有效普通值。敏感字段始终为 `None`。
    pub effective_value: Option<Value>,
    /// 本进程启动时实际加载的普通值。敏感字段始终为 `None`。
    pub running_value: Option<Value>,
    pub pending_restart: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ConfigCenterSnapshot {
    pub revision: String,
    pub file_exists: bool,
    pub fields: Vec<ConfigFieldSnapshot>,
}

#[derive(Clone)]
pub struct ConfigCenter {
    registry: ConfigRegistry,
    managed_file: ManagedConfigFile,
    secret_store: SecretStore,
    external_environment: Arc<HashMap<String, String>>,
    running_managed: Arc<ManagedConfigSnapshot>,
    running_secret_revisions: Arc<HashMap<String, String>>,
}

impl ConfigCenter {
    pub fn open(
        fields: Vec<qq_maid_common::managed_config::ManagedConfigField>,
        paths: ConfigCenterPaths,
        database: SqliteDatabase,
    ) -> Result<Self, ConfigCenterError> {
        let registry = ConfigRegistry::new(fields)?;
        let managed_file = ManagedConfigFile::new(paths.managed_config_file, registry.clone());
        let secret_store = SecretStore::open(database, &paths.master_key_file)?;
        let running_managed = managed_file.load()?;
        let running_secret_revisions = secret_store.envelope_revisions()?;
        Ok(Self {
            registry,
            managed_file,
            secret_store,
            external_environment: Arc::new(HashMap::new()),
            running_managed: Arc::new(running_managed),
            running_secret_revisions: Arc::new(running_secret_revisions),
        })
    }

    /// 绑定启动时读取到的外部覆盖快照，供只读管理 API 展示真实来源。
    pub fn with_external_environment(mut self, environment: HashMap<String, String>) -> Self {
        self.external_environment = Arc::new(environment);
        self
    }

    pub fn registry(&self) -> &ConfigRegistry {
        &self.registry
    }

    pub fn snapshot(
        &self,
        external_environment: &HashMap<String, String>,
    ) -> Result<ConfigCenterSnapshot, ConfigCenterError> {
        let managed = self.managed_file.load()?;
        let secret_revisions = self.secret_store.envelope_revisions()?;
        let mut fields = Vec::with_capacity(self.registry.fields().len());

        for field in self.registry.fields() {
            let external = external_value(external_environment, field);
            let managed_value = managed.values.get(field.key);
            let has_secret = secret_revisions.contains_key(field.key);
            let overridden = external.is_some() && (managed_value.is_some() || has_secret);

            let (source, configured, value) = if let Some(raw) = external {
                let value = if field.sensitivity == ManagedConfigSensitivity::Public {
                    Some(self.registry.parse_environment_value(field, raw)?)
                } else {
                    None
                };
                (
                    ConfigValueSource::Environment,
                    !raw.trim().is_empty(),
                    value,
                )
            } else if field.sensitivity == ManagedConfigSensitivity::Secret && has_secret {
                (ConfigValueSource::EncryptedSecret, true, None)
            } else if let Some(value) = managed_value {
                (ConfigValueSource::ManagedToml, true, Some(value.clone()))
            } else if let Some(default) = field.default_value {
                (
                    ConfigValueSource::Default,
                    true,
                    (field.sensitivity == ManagedConfigSensitivity::Public)
                        .then(|| self.registry.parse_environment_value(field, default))
                        .transpose()?,
                )
            } else {
                (ConfigValueSource::NotConfigured, false, None)
            };

            let saved_value = (field.sensitivity == ManagedConfigSensitivity::Public)
                .then(|| managed_value.cloned())
                .flatten();
            let running_value = if field.sensitivity == ManagedConfigSensitivity::Public {
                if let Some(raw) = external {
                    Some(self.registry.parse_environment_value(field, raw)?)
                } else if let Some(value) = self.running_managed.values.get(field.key) {
                    Some(value.clone())
                } else {
                    field
                        .default_value
                        .map(|default| self.registry.parse_environment_value(field, default))
                        .transpose()?
                }
            } else {
                None
            };
            let pending_restart =
                if field.apply_mode != ManagedConfigApplyMode::Restart || external.is_some() {
                    false
                } else if field.sensitivity == ManagedConfigSensitivity::Secret {
                    secret_revisions.get(field.key) != self.running_secret_revisions.get(field.key)
                } else if field.sensitivity == ManagedConfigSensitivity::Public {
                    value != running_value
                } else {
                    false
                };

            fields.push(ConfigFieldSnapshot {
                key: field.key.to_owned(),
                module: field.module.to_owned(),
                value_type: field.value_type,
                source,
                overridden,
                editable: field.web_editable && external.is_none(),
                configured,
                valid: true,
                sensitivity: field.sensitivity,
                apply_mode: field.apply_mode,
                saved_value,
                effective_value: value,
                running_value,
                pending_restart,
            });
        }

        Ok(ConfigCenterSnapshot {
            revision: managed.revision,
            file_exists: managed.exists,
            fields,
        })
    }

    pub fn current_snapshot(&self) -> Result<ConfigCenterSnapshot, ConfigCenterError> {
        self.snapshot(&self.external_environment)
    }

    pub fn update_managed(
        &self,
        expected_revision: &str,
        changes: &[ManagedConfigChange],
    ) -> Result<ManagedConfigSnapshot, ConfigCenterError> {
        self.managed_file.update(expected_revision, changes)
    }

    pub fn replace_secret(&self, key: &str, value: &str) -> Result<(), ConfigCenterError> {
        let field = self.registry.require(key)?;
        if field.sensitivity != ManagedConfigSensitivity::Secret || !field.web_editable {
            return Err(ConfigCenterError::invalid(format!(
                "field `{key}` is not a Web-writable secret"
            )));
        }
        if value.trim().is_empty() {
            return Err(ConfigCenterError::invalid(format!(
                "secret field `{key}` must not be empty; use clear explicitly"
            )));
        }
        if matches!(
            value.trim(),
            "********" | "••••••••" | "<redacted>" | "[redacted]" | "__UNCHANGED__"
        ) {
            return Err(ConfigCenterError::invalid(format!(
                "secret field `{key}` contains a masked placeholder; use replace or no-change explicitly"
            )));
        }
        self.secret_store.replace(key, value.as_bytes())
    }

    pub fn clear_secret(&self, key: &str) -> Result<bool, ConfigCenterError> {
        let field = self.registry.require(key)?;
        if field.sensitivity != ManagedConfigSensitivity::Secret || !field.web_editable {
            return Err(ConfigCenterError::invalid(format!(
                "field `{key}` is not a Web-writable secret"
            )));
        }
        self.secret_store.clear(key)
    }

    /// 生成现有 Core / Gateway resolver 可消费的环境映射。
    ///
    /// 外部进程环境始终最后覆盖；敏感值只在内存中解密，不写回 TOML、日志或 API。
    pub fn resolved_environment(
        &self,
        external_environment: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>, ConfigCenterError> {
        let managed = self.managed_file.load()?;
        let mut resolved = HashMap::new();
        for field in self.registry.fields() {
            if external_value(external_environment, field).is_some() {
                continue;
            }
            match field.sensitivity {
                ManagedConfigSensitivity::Public => {
                    if let Some(value) = managed.values.get(field.key) {
                        resolved.insert(
                            field.env_name.to_owned(),
                            self.registry.environment_string(field, value)?,
                        );
                    }
                }
                ManagedConfigSensitivity::Secret => {
                    if let Some(value) = self.secret_store.read(field.key)? {
                        let value = String::from_utf8(value).map_err(|_| {
                            ConfigCenterError::secret(format!(
                                "stored secret `{}` is not valid UTF-8",
                                field.key
                            ))
                        })?;
                        resolved.insert(field.env_name.to_owned(), value);
                    }
                }
                ManagedConfigSensitivity::Restricted => {}
            }
        }
        resolved.extend(external_environment.clone());
        Ok(resolved)
    }
}

fn external_value<'a>(
    environment: &'a HashMap<String, String>,
    field: &qq_maid_common::managed_config::ManagedConfigField,
) -> Option<&'a String> {
    environment.get(field.env_name).or_else(|| {
        field
            .env_aliases
            .iter()
            .find_map(|alias| environment.get(*alias))
    })
}
