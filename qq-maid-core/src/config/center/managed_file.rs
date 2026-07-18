use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
    update_lock: Arc<Mutex<()>>,
}

impl ManagedConfigFile {
    pub fn new(path: PathBuf, registry: ConfigRegistry) -> Self {
        Self {
            path,
            registry,
            update_lock: Arc::new(Mutex::new(())),
        }
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

    /// 首次启动即建立最小受管文件，避免控制台长期停留在“尚未建立”的半初始化状态。
    ///
    /// 空 `values` 仍通过注册表默认值和外部兜底解析，不会把 secret 或部署环境复制进文件；
    /// 已有文件只读取不覆盖。并发实例抢先创建时重新加载获胜文件。
    pub fn load_or_initialize(&self) -> Result<ManagedConfigSnapshot, ConfigCenterError> {
        let current = self.load()?;
        if current.exists {
            return Ok(current);
        }
        match self.update("missing", &[]) {
            Ok(created) => Ok(created),
            Err(error) if error.code() == "config_conflict" => self.load(),
            Err(error) => Err(error),
        }
    }

    pub fn update(
        &self,
        expected_revision: &str,
        changes: &[ManagedConfigChange],
    ) -> Result<ManagedConfigSnapshot, ConfigCenterError> {
        self.update_with_validator(expected_revision, changes, |_| Ok(()))
    }

    pub(super) fn update_with_validator(
        &self,
        expected_revision: &str,
        changes: &[ManagedConfigChange],
        validate: impl FnOnce(&BTreeMap<String, Value>) -> Result<(), ConfigCenterError>,
    ) -> Result<ManagedConfigSnapshot, ConfigCenterError> {
        // 同一进程内的请求必须串行完成“检查—构造—替换”，尤其避免两个首次创建
        // 请求都看到 missing 后依次覆盖。跨进程或人工编辑仍由替换前 revision 检查兜底。
        let _guard = self
            .update_lock
            .lock()
            .map_err(|_| ConfigCenterError::io("managed config update lock is poisoned"))?;
        ensure_expected_revision(&self.path, expected_revision, "managed config")?;
        let current = self.load()?;

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
                        || field.sensitivity != super::ManagedConfigSensitivity::Public
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
        validate(&document.values)?;
        let bytes = toml::to_string_pretty(&document)
            .map_err(|err| {
                ConfigCenterError::invalid(format!("failed to serialize managed config: {err}"))
            })?
            .into_bytes();

        atomic_write_if_revision(&self.path, &bytes, expected_revision, "managed config")?;
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

/// 对受管文件执行统一的 opaque revision 核对。
///
/// 只读取原始字节而不解析内容，因此即使并发人工修改写入了非法 TOML，也会稳定返回
/// `config_conflict`，不会因解析错误改变冲突语义。
pub(super) fn ensure_expected_revision(
    path: &Path,
    expected_revision: &str,
    label: &str,
) -> Result<(), ConfigCenterError> {
    let actual = read_regular_file(path)?
        .as_deref()
        .map(revision)
        .unwrap_or_else(|| "missing".to_owned());
    if actual == expected_revision {
        Ok(())
    } else {
        Err(ConfigCenterError::conflict(format!(
            "{label} changed since the supplied revision"
        )))
    }
}

pub(super) fn read_regular_file(path: &Path) -> Result<Option<Vec<u8>>, ConfigCenterError> {
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

pub(super) fn revision(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    format!("sha256:{encoded}")
}

pub(super) fn atomic_write_if_revision(
    path: &Path,
    bytes: &[u8],
    expected_revision: &str,
    label: &str,
) -> Result<(), ConfigCenterError> {
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
        // 临时文件完整落盘之后、正式 replace 之前再读一次目标，尽量缩短非协作式
        // 人工/跨进程写入的 TOCTOU 窗口。
        ensure_expected_revision(path, expected_revision, label)?;
        replace_file(&temp_path, path).map_err(|err| {
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

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
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
