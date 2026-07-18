//! 受管运行配置中心。
//!
//! 普通字段写入专用 `runtime.toml`，敏感值认证加密后写入 SQLite；主密钥只保存在
//! 数据库外的独立文件。这里提供领域模型和安全写入能力，管理员认证与页面由后续任务接入。

mod agent_file;
mod field;
mod managed_file;
mod registry;
mod secret;

#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::Serialize;
use toml::Value;

use crate::{config::AgentRuntimeConfig, storage::database::SqliteDatabase};

pub use agent_file::{AgentConfigChange, AgentConfigFile, AgentConfigSnapshot};
pub use field::{
    ManagedConfigApplyMode, ManagedConfigField, ManagedConfigSensitivity, ManagedConfigValueType,
};
pub use managed_file::{ManagedConfigChange, ManagedConfigFile, ManagedConfigSnapshot};
pub use registry::ConfigRegistry;
pub use secret::{
    CONFIG_SECRET_SCHEMA_V1, SECRET_MISSING_REVISION, SecretConfigChange, SecretStore,
};

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
    AgentToml,
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
    /// 仅 secret 字段返回 opaque revision；不存在时明确返回 `missing`。
    pub revision: Option<String>,
    pub sensitivity: ManagedConfigSensitivity,
    pub apply_mode: ManagedConfigApplyMode,
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
    /// Agent 策略使用独立 revision，不能与 runtime.toml 的 revision 混用。
    pub agent: Option<AgentConfigSnapshot>,
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
    agent_file: Option<AgentConfigFile>,
    candidate_validator: Option<CandidateValidator>,
    startup_preflight: Option<StartupPreflight>,
    mutation_lock: Arc<Mutex<()>>,
}

type CandidateValidator =
    Arc<dyn Fn(&HashMap<String, String>) -> Result<(), String> + Send + Sync + 'static>;
type StartupPreflight = Arc<
    dyn Fn(&HashMap<String, String>, Option<&AgentRuntimeConfig>) -> Result<(), String>
        + Send
        + Sync
        + 'static,
>;

impl ConfigCenter {
    pub fn open(
        fields: Vec<ManagedConfigField>,
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
            agent_file: None,
            candidate_validator: None,
            startup_preflight: None,
            mutation_lock: Arc::new(Mutex::new(())),
        })
    }

    /// 绑定启动时读取到的外部覆盖快照，供只读管理 API 展示真实来源。
    pub fn with_external_environment(mut self, environment: HashMap<String, String>) -> Self {
        self.external_environment = Arc::new(environment);
        self
    }

    /// 由统一根程序注入 Core 与 Gateway 的真实配置解析器，保持 crate 依赖方向不反转。
    pub fn with_candidate_validator(
        mut self,
        validator: impl Fn(&HashMap<String, String>) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.candidate_validator = Some(Arc::new(validator));
        self
    }

    /// 注入统一根程序的完整启动预检。Agent 写入会把尚未落盘的候选配置传给该钩子；
    /// runtime/secret 写入则从 `AGENT_CONFIG_FILE` 加载当前文件，行为与真实重启一致。
    pub fn with_startup_preflight(
        mut self,
        preflight: impl Fn(&HashMap<String, String>, Option<&AgentRuntimeConfig>) -> Result<(), String>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.startup_preflight = Some(Arc::new(preflight));
        self
    }

    /// 绑定启动时真正加载的 Agent 配置。文件路径只取自 Bootstrap resolver，写接口不接收路径。
    pub fn with_running_agent_config(
        mut self,
        running: AgentRuntimeConfig,
    ) -> Result<Self, ConfigCenterError> {
        self.agent_file = Some(AgentConfigFile::new(running)?);
        Ok(self)
    }

    pub fn registry(&self) -> &ConfigRegistry {
        &self.registry
    }

    pub fn snapshot(
        &self,
        external_environment: &HashMap<String, String>,
    ) -> Result<ConfigCenterSnapshot, ConfigCenterError> {
        let managed = self.managed_file.load()?;
        let secret_snapshot = self.secret_store.snapshot()?;
        let secret_revisions = secret_snapshot.revisions;
        let secret_values = secret_snapshot.plaintexts;
        let candidate_environment =
            self.resolve_environment_from(&managed.values, &secret_values, external_environment)?;
        let candidate_valid = self
            .validate_candidate(&candidate_environment, None)
            .is_ok();
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
                valid: candidate_valid,
                revision: (field.sensitivity == ManagedConfigSensitivity::Secret).then(|| {
                    secret_revisions
                        .get(field.key)
                        .cloned()
                        .unwrap_or_else(|| SECRET_MISSING_REVISION.to_owned())
                }),
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
            agent: self
                .agent_file
                .as_ref()
                .map(AgentConfigFile::snapshot)
                .transpose()?,
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
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| ConfigCenterError::io("configuration mutation lock is poisoned"))?;
        self.reject_external_overrides(changes.iter().map(ManagedConfigChange::key))?;
        let secret_values = self.secret_store.plaintexts()?;
        self.managed_file
            .update_with_validator(expected_revision, changes, |managed_values| {
                let environment = self.resolve_environment_from(
                    managed_values,
                    &secret_values,
                    &self.external_environment,
                )?;
                self.validate_candidate(&environment, None)
            })
    }

    pub fn update_agent(
        &self,
        expected_revision: &str,
        changes: &[AgentConfigChange],
    ) -> Result<AgentConfigSnapshot, ConfigCenterError> {
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| ConfigCenterError::io("configuration mutation lock is poisoned"))?;
        let managed = self.managed_file.load()?;
        let secret_values = self.secret_store.plaintexts()?;
        let environment = self.resolve_environment_from(
            &managed.values,
            &secret_values,
            &self.external_environment,
        )?;
        self.agent_file
            .as_ref()
            .ok_or_else(|| ConfigCenterError::invalid("agent config domain is not initialized"))?
            .update_with_validator(expected_revision, changes, |candidate_agent| {
                self.validate_candidate(&environment, Some(candidate_agent))
            })
    }

    pub fn replace_secret(
        &self,
        key: &str,
        value: &str,
        expected_revision: &str,
    ) -> Result<String, ConfigCenterError> {
        let revisions = self.update_secrets(&[SecretConfigChange::Replace {
            key: key.to_owned(),
            value: value.to_owned(),
            expected_revision: expected_revision.to_owned(),
        }])?;
        Ok(revisions[key].clone())
    }

    pub fn clear_secret(
        &self,
        key: &str,
        expected_revision: &str,
    ) -> Result<String, ConfigCenterError> {
        let revisions = self.update_secrets(&[SecretConfigChange::Clear {
            key: key.to_owned(),
            expected_revision: expected_revision.to_owned(),
        }])?;
        Ok(revisions[key].clone())
    }

    /// 批量修改关联 secret；revision 比较、最终候选校验和全部写入处于同一 SQLite 事务。
    pub fn update_secrets(
        &self,
        changes: &[SecretConfigChange],
    ) -> Result<HashMap<String, String>, ConfigCenterError> {
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| ConfigCenterError::io("configuration mutation lock is poisoned"))?;
        let mut keys = HashSet::with_capacity(changes.len());
        for change in changes {
            let key = change.key();
            let field = self.registry.require(key)?;
            if field.sensitivity != ManagedConfigSensitivity::Secret || !field.web_editable {
                return Err(ConfigCenterError::invalid(format!(
                    "field `{key}` is not a Web-writable secret"
                )));
            }
            if !keys.insert(key) {
                return Err(ConfigCenterError::invalid(format!(
                    "secret field `{key}` appears more than once in one mutation"
                )));
            }
            if external_value(&self.external_environment, field).is_some() {
                return Err(ConfigCenterError::invalid(format!(
                    "field `{key}` is controlled by the process environment and cannot be modified"
                )));
            }
            if let SecretConfigChange::Replace { value, .. } = change {
                validate_secret_replacement(key, value)?;
            }
        }
        let managed = self.managed_file.load()?;
        self.secret_store.mutate(changes, |secret_values| {
            let environment = self.resolve_environment_from(
                &managed.values,
                secret_values,
                &self.external_environment,
            )?;
            self.validate_candidate(&environment, None)
        })
    }

    /// 生成现有 Core / Gateway resolver 可消费的环境映射。
    ///
    /// 外部进程环境始终最后覆盖；敏感值只在内存中解密，不写回 TOML、日志或 API。
    pub fn resolved_environment(
        &self,
        external_environment: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>, ConfigCenterError> {
        let managed = self.managed_file.load()?;
        let secret_values = self.secret_store.plaintexts()?;
        self.resolve_environment_from(&managed.values, &secret_values, external_environment)
    }

    fn resolve_environment_from(
        &self,
        managed_values: &std::collections::BTreeMap<String, Value>,
        secret_values: &HashMap<String, Vec<u8>>,
        external_environment: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>, ConfigCenterError> {
        let mut resolved = HashMap::new();
        for field in self.registry.fields() {
            if external_value(external_environment, field).is_some() {
                continue;
            }
            match field.sensitivity {
                ManagedConfigSensitivity::Public => {
                    if let Some(value) = managed_values.get(field.key) {
                        resolved.insert(
                            field.env_name.to_owned(),
                            self.registry.environment_string(field, value)?,
                        );
                    }
                }
                ManagedConfigSensitivity::Secret => {
                    if let Some(value) = secret_values.get(field.key) {
                        let value = String::from_utf8(value.clone()).map_err(|_| {
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

    fn validate_candidate(
        &self,
        environment: &HashMap<String, String>,
        candidate_agent: Option<&AgentRuntimeConfig>,
    ) -> Result<(), ConfigCenterError> {
        if let Some(validator) = &self.candidate_validator {
            validator(environment).map_err(ConfigCenterError::invalid)?;
        }
        if let Some(preflight) = &self.startup_preflight {
            preflight(environment, candidate_agent).map_err(ConfigCenterError::invalid)?;
        }
        Ok(())
    }

    fn reject_external_overrides<'a>(
        &self,
        keys: impl Iterator<Item = &'a str>,
    ) -> Result<(), ConfigCenterError> {
        for key in keys {
            let field = self.registry.require(key)?;
            if external_value(&self.external_environment, field).is_some() {
                return Err(ConfigCenterError::invalid(format!(
                    "field `{key}` is controlled by the process environment and cannot be modified"
                )));
            }
        }
        Ok(())
    }
}

fn validate_secret_replacement(key: &str, value: &str) -> Result<(), ConfigCenterError> {
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
    Ok(())
}

fn external_value<'a>(
    environment: &'a HashMap<String, String>,
    field: &ManagedConfigField,
) -> Option<&'a String> {
    environment.get(field.env_name).or_else(|| {
        field
            .env_aliases
            .iter()
            .find_map(|alias| environment.get(*alias))
    })
}
