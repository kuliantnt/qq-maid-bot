use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::Serialize;
use toml::Value;

use crate::config::agent::{
    AgentConfigDocument, AgentConfigSource, AgentProfileConfig, AgentRuntimeConfig,
    AgentSceneConfig, ChatScene, KnowledgeEmbeddingConfig, KnowledgeRetrievalMode, RouteFile,
    SearchRouteFile,
};

use super::{
    ConfigCenterError, ConfigValueSource, ManagedConfigApplyMode,
    managed_file::{
        atomic_write_if_revision, ensure_expected_revision, read_regular_file, revision,
    },
};

#[derive(Debug, Clone, PartialEq)]
pub enum AgentConfigChange {
    SetKnowledge {
        mode: KnowledgeRetrievalMode,
        embedding: KnowledgeEmbeddingConfig,
    },
    SetModelRoute {
        name: String,
        candidates: Vec<String>,
    },
    RemoveModelRoute {
        name: String,
    },
    SetSearchRoute {
        name: String,
        model: String,
    },
    RemoveSearchRoute {
        name: String,
    },
    SetProfile {
        name: String,
        profile: AgentProfileConfig,
    },
    RemoveProfile {
        name: String,
    },
    SetScene {
        scene: ChatScene,
        config: AgentSceneConfig,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentConfigSnapshot {
    pub revision: String,
    pub file_exists: bool,
    pub source: ConfigValueSource,
    pub editable: bool,
    pub read_only: bool,
    pub apply_mode: ManagedConfigApplyMode,
    /// `agent.toml` 当前保存的完整结构，不包含任何 secret 原文。
    pub saved_value: Option<Value>,
    /// 本进程启动时实际读取的 `agent.toml` 结构；保存不会篡改运行中策略。
    pub running_value: Option<Value>,
    pub pending_restart: bool,
}

#[derive(Clone)]
pub struct AgentConfigFile {
    path: PathBuf,
    running_value: Option<Value>,
    update_lock: Arc<Mutex<()>>,
}

impl AgentConfigFile {
    pub fn new(running: AgentRuntimeConfig) -> Result<Self, ConfigCenterError> {
        let running_value = Some(document_value(running.document().ok_or_else(|| {
            ConfigCenterError::invalid(
                "configuration center requires the explicit agent.toml loaded at process startup",
            )
        })?)?);
        Ok(Self {
            path: running.managed_path(),
            running_value,
            update_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn snapshot(&self) -> Result<AgentConfigSnapshot, ConfigCenterError> {
        let loaded = self.load()?;
        self.snapshot_from_loaded(loaded)
    }

    pub fn update(
        &self,
        expected_revision: &str,
        changes: &[AgentConfigChange],
    ) -> Result<AgentConfigSnapshot, ConfigCenterError> {
        self.update_with_validator(expected_revision, changes, |_| Ok(()))
    }

    pub(super) fn update_with_validator(
        &self,
        expected_revision: &str,
        changes: &[AgentConfigChange],
        validate: impl FnOnce(&AgentRuntimeConfig) -> Result<(), ConfigCenterError>,
    ) -> Result<AgentConfigSnapshot, ConfigCenterError> {
        let _guard = self
            .update_lock
            .lock()
            .map_err(|_| ConfigCenterError::io("agent config update lock is poisoned"))?;
        ensure_expected_revision(&self.path, expected_revision, "agent config")?;
        let current = self.load()?;
        if !current.exists {
            return Err(ConfigCenterError::invalid(
                "agent config file does not exist; install a complete agent.toml before using managed edits",
            ));
        }
        if !safe_to_replace(&self.path)? {
            return Err(ConfigCenterError::io(
                "agent config is read-only or has unsafe write permissions",
            ));
        }

        let mut document = current.document.expect("existing file has a document");
        for change in changes {
            apply_change(&mut document, change)?;
        }
        let candidate_runtime = AgentRuntimeConfig::from_document(
            document.clone(),
            AgentConfigSource::File(self.path.to_string_lossy().into_owned()),
        )
        .map_err(|err| ConfigCenterError::invalid(err.to_string()))?;
        validate(&candidate_runtime)?;

        // 规范化写回会丢弃注释和排版，但由同一 schema 序列化全部合法语义。
        let bytes = toml::to_string_pretty(&document)
            .map_err(|err| {
                ConfigCenterError::invalid(format!("failed to serialize agent config: {err}"))
            })?
            .into_bytes();

        atomic_write_if_revision(&self.path, &bytes, expected_revision, "agent config")?;

        self.snapshot_from_loaded(LoadedAgentConfig {
            revision: revision(&bytes),
            exists: true,
            document: Some(document),
            read_only: false,
        })
    }

    fn load(&self) -> Result<LoadedAgentConfig, ConfigCenterError> {
        let Some(bytes) = read_regular_file(&self.path)? else {
            return Ok(LoadedAgentConfig {
                revision: "missing".to_owned(),
                exists: false,
                document: None,
                read_only: true,
            });
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ConfigCenterError::invalid("agent config must be valid UTF-8"))?;
        let document: AgentConfigDocument = toml::from_str(text).map_err(|err| {
            ConfigCenterError::invalid(format!("invalid agent config TOML: {err}"))
        })?;
        AgentRuntimeConfig::from_document(
            document.clone(),
            AgentConfigSource::File(self.path.to_string_lossy().into_owned()),
        )
        .map_err(|err| ConfigCenterError::invalid(err.to_string()))?;
        Ok(LoadedAgentConfig {
            revision: revision(&bytes),
            exists: true,
            document: Some(document),
            read_only: !safe_to_replace(&self.path)?,
        })
    }

    fn snapshot_from_loaded(
        &self,
        loaded: LoadedAgentConfig,
    ) -> Result<AgentConfigSnapshot, ConfigCenterError> {
        let saved_value = loaded.document.as_ref().map(document_value).transpose()?;
        Ok(AgentConfigSnapshot {
            revision: loaded.revision,
            file_exists: loaded.exists,
            source: if loaded.exists {
                ConfigValueSource::AgentToml
            } else {
                ConfigValueSource::NotConfigured
            },
            editable: loaded.exists && !loaded.read_only,
            read_only: loaded.read_only,
            apply_mode: ManagedConfigApplyMode::Restart,
            pending_restart: saved_value != self.running_value,
            saved_value,
            running_value: self.running_value.clone(),
        })
    }
}

struct LoadedAgentConfig {
    revision: String,
    exists: bool,
    document: Option<AgentConfigDocument>,
    read_only: bool,
}

fn apply_change(
    document: &mut AgentConfigDocument,
    change: &AgentConfigChange,
) -> Result<(), ConfigCenterError> {
    match change {
        AgentConfigChange::SetKnowledge { mode, embedding } => {
            document.knowledge.mode = *mode;
            document.knowledge.embedding = embedding.clone();
        }
        AgentConfigChange::SetModelRoute { name, candidates } => {
            let name = entry_name(name)?;
            document.model_routes.insert(
                name,
                RouteFile {
                    candidates: candidates.clone(),
                },
            );
        }
        AgentConfigChange::RemoveModelRoute { name } => {
            document.model_routes.remove(entry_name(name)?.as_str());
        }
        AgentConfigChange::SetSearchRoute { name, model } => {
            let name = entry_name(name)?;
            document.search_routes.insert(
                name,
                SearchRouteFile {
                    model: model.clone(),
                },
            );
        }
        AgentConfigChange::RemoveSearchRoute { name } => {
            document.search_routes.remove(entry_name(name)?.as_str());
        }
        AgentConfigChange::SetProfile { name, profile } => {
            document.profiles.insert(entry_name(name)?, profile.clone());
        }
        AgentConfigChange::RemoveProfile { name } => {
            document.profiles.remove(entry_name(name)?.as_str());
        }
        AgentConfigChange::SetScene { scene, config } => match scene {
            ChatScene::Private => document.scenes.private = config.clone(),
            ChatScene::Group => document.scenes.group = config.clone(),
        },
    }
    Ok(())
}

fn entry_name(name: &str) -> Result<String, ConfigCenterError> {
    let name = name.trim();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(ConfigCenterError::invalid(
            "agent config entry name must not be empty or contain control characters",
        ));
    }
    Ok(name.to_owned())
}

fn document_value(document: &AgentConfigDocument) -> Result<Value, ConfigCenterError> {
    Value::try_from(document.clone()).map_err(|err| {
        ConfigCenterError::invalid(format!("failed to encode agent config snapshot: {err}"))
    })
}

fn safe_to_replace(path: &std::path::Path) -> Result<bool, ConfigCenterError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        ConfigCenterError::io(format!("failed to inspect agent config file: {err}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ConfigCenterError::io(
            "agent config path must be a regular file and must not be a symbolic link",
        ));
    }
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let parent_metadata = fs::symlink_metadata(parent).map_err(|err| {
        ConfigCenterError::io(format!("failed to inspect agent config directory: {err}"))
    })?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(ConfigCenterError::io(
            "agent config parent must be a directory and must not be a symbolic link",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        let parent_mode = parent_metadata.permissions().mode();
        Ok(mode & 0o200 != 0
            && mode & 0o022 == 0
            && parent_mode & 0o300 == 0o300
            && parent_mode & 0o022 == 0)
    }
    #[cfg(not(unix))]
    {
        Ok(!metadata.permissions().readonly() && !parent_metadata.permissions().readonly())
    }
}
