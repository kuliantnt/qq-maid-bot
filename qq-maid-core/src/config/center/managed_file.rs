use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use toml::Value;

use super::{ConfigCenterError, ConfigRegistry};

const MANAGED_CONFIG_VERSION: u32 = 1;
const MAX_MANAGED_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum ManagedConfigChange {
    Set { key: String, value: Value },
    Remove { key: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ManagedConfigSnapshot {
    pub revision: String,
    pub exists: bool,
    pub values: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedConfigDocument {
    version: u32,
    #[serde(default)]
    values: BTreeMap<String, Value>,
}

#[derive(Clone)]
pub struct ManagedConfigFile {
    path: PathBuf,
    registry: ConfigRegistry,
}

impl ManagedConfigFile {
    pub fn new(path: PathBuf, registry: ConfigRegistry) -> Self {
        Self { path, registry }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<ManagedConfigSnapshot, ConfigCenterError> {
        let Some(bytes) = read_regular_file(&self.path)? else {
            return Ok(ManagedConfigSnapshot {
                revision: "missing".to_owned(),
                exists: false,
                values: BTreeMap::new(),
            });
        };
        let document = parse_document(&bytes)?;
        self.validate_document(&document)?;
        Ok(ManagedConfigSnapshot {
            revision: revision(&bytes),
            exists: true,
            values: document.values,
        })
    }

    pub fn update(
        &self,
        expected_revision: &str,
        changes: &[ManagedConfigChange],
    ) -> Result<ManagedConfigSnapshot, ConfigCenterError> {
        let current = self.load()?;
        if current.revision != expected_revision {
            return Err(ConfigCenterError::conflict(format!(
                "managed config changed since revision `{expected_revision}`"
            )));
        }

        let mut document = ManagedConfigDocument {
            version: MANAGED_CONFIG_VERSION,
            values: current.values,
        };
        for change in changes {
            match change {
                ManagedConfigChange::Set { key, value } => {
                    let field = self.registry.require(key)?;
                    self.registry.validate_managed_value(field, value)?;
                    document.values.insert(key.clone(), value.clone());
                }
                ManagedConfigChange::Remove { key } => {
                    let field = self.registry.require(key)?;
                    if !field.web_editable
                        || field.sensitivity
                            != qq_maid_common::managed_config::ManagedConfigSensitivity::Public
                    {
                        return Err(ConfigCenterError::invalid(format!(
                            "field `{key}` cannot be removed from managed TOML"
                        )));
                    }
                    document.values.remove(key);
                }
            }
        }
        self.validate_document(&document)?;
        let bytes = toml::to_string_pretty(&document)
            .map_err(|err| {
                ConfigCenterError::invalid(format!("failed to serialize managed config: {err}"))
            })?
            .into_bytes();
        atomic_write(&self.path, &bytes)?;
        Ok(ManagedConfigSnapshot {
            revision: revision(&bytes),
            exists: true,
            values: document.values,
        })
    }

    fn validate_document(&self, document: &ManagedConfigDocument) -> Result<(), ConfigCenterError> {
        if document.version != MANAGED_CONFIG_VERSION {
            return Err(ConfigCenterError::invalid(format!(
                "unsupported managed config version {}",
                document.version
            )));
        }
        for (key, value) in &document.values {
            let field = self.registry.require(key)?;
            self.registry.validate_managed_value(field, value)?;
        }
        Ok(())
    }
}

fn read_regular_file(path: &Path) -> Result<Option<Vec<u8>>, ConfigCenterError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ConfigCenterError::io(format!(
                "failed to inspect managed config file: {err}"
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ConfigCenterError::io(
            "managed config path must be a regular file and must not be a symbolic link",
        ));
    }
    if metadata.len() > MAX_MANAGED_CONFIG_BYTES {
        return Err(ConfigCenterError::invalid(format!(
            "managed config exceeds {MAX_MANAGED_CONFIG_BYTES} bytes"
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|err| {
        ConfigCenterError::io(format!("failed to open managed config file: {err}"))
    })?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes).map_err(|err| {
        ConfigCenterError::io(format!("failed to read managed config file: {err}"))
    })?;
    Ok(Some(bytes))
}

fn parse_document(bytes: &[u8]) -> Result<ManagedConfigDocument, ConfigCenterError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ConfigCenterError::invalid("managed config must be valid UTF-8"))?;
    toml::from_str(text)
        .map_err(|err| ConfigCenterError::invalid(format!("invalid managed config TOML: {err}")))
}

fn revision(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    format!("sha256:{encoded}")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ConfigCenterError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_regular_directory(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ConfigCenterError::io(
                "managed config path must be a regular file and must not be a symbolic link",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode();
            if mode & 0o200 == 0 || mode & 0o022 != 0 {
                return Err(ConfigCenterError::io(
                    "managed config must be owner-writable and must not be group/other-writable",
                ));
            }
        }
        #[cfg(not(unix))]
        if metadata.permissions().readonly() {
            return Err(ConfigCenterError::io(
                "managed config is read-only and cannot be replaced",
            ));
        }
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("runtime.toml");
    let temp_path = parent.join(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path).map_err(|err| {
            ConfigCenterError::io(format!("failed to create managed config temp file: {err}"))
        })?;
        file.write_all(bytes).map_err(|err| {
            ConfigCenterError::io(format!("failed to write managed config temp file: {err}"))
        })?;
        file.sync_all().map_err(|err| {
            ConfigCenterError::io(format!("failed to sync managed config temp file: {err}"))
        })?;
        fs::rename(&temp_path, path).map_err(|err| {
            ConfigCenterError::io(format!(
                "failed to replace managed config atomically: {err}"
            ))
        })?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn ensure_regular_directory(path: &Path) -> Result<(), ConfigCenterError> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|err| {
            ConfigCenterError::io(format!("failed to create managed config directory: {err}"))
        })?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        ConfigCenterError::io(format!("failed to inspect managed config directory: {err}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ConfigCenterError::io(
            "managed config parent must be a directory and must not be a symbolic link",
        ));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), ConfigCenterError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|err| {
                ConfigCenterError::io(format!("failed to sync managed config directory: {err}"))
            })?;
    }
    Ok(())
}
