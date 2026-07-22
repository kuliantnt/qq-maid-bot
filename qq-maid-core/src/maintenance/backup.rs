use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, backup::Backup};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::storage::database::{SqliteMigration, is_compatible_historical_migration};

const MANIFEST_FILE: &str = "manifest.toml";
const DATABASE_FILE: &str = "database/app.db";
const CONFIG_DIRECTORY: &str = "config";

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct BackupError {
    code: &'static str,
    message: String,
}

impl BackupError {
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

    fn io(context: &str, error: impl std::fmt::Display) -> Self {
        Self::new("backup_io_error", format!("{context}: {error}"))
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_backup", message)
    }
}

#[derive(Debug, Clone)]
pub struct BackupOptions {
    pub database_file: PathBuf,
    pub config_directory: PathBuf,
    pub output_directory: PathBuf,
    pub include_secrets: bool,
    pub application_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub format_version: u32,
    pub application_version: String,
    pub created_at_unix: u64,
    pub includes_secret_material: bool,
    pub database_migrations: Vec<String>,
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct BackupReport {
    pub output_directory: PathBuf,
    pub file_count: usize,
    pub includes_secret_material: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub target_root: PathBuf,
    pub database_destination: PathBuf,
    pub config_destination: PathBuf,
    pub file_count: usize,
    pub includes_secret_material: bool,
    pub warnings: Vec<String>,
}

/// 使用 SQLite Online Backup API 取得 WAL 一致快照，并把配置目录复制到同一个受限目录。
/// 默认排除 `.env`、整个 `secrets/` 和一次性 Bootstrap token。
pub fn create_backup(
    options: &BackupOptions,
    known_migrations: &[SqliteMigration],
) -> Result<BackupReport, BackupError> {
    require_regular_file(&options.database_file, "SQLite source")?;
    require_regular_directory(&options.config_directory, "config source")?;
    if fs::symlink_metadata(&options.output_directory).is_ok() {
        return Err(BackupError::invalid(
            "backup output directory already exists; choose a new path",
        ));
    }
    let parent = options
        .output_directory
        .parent()
        .unwrap_or_else(|| Path::new("."));
    ensure_backup_output_parent(parent)?;
    let config_root = fs::canonicalize(&options.config_directory)
        .map_err(|error| BackupError::io("resolve config source", error))?;
    let output_parent = fs::canonicalize(parent)
        .map_err(|error| BackupError::io("resolve backup output parent", error))?;
    if output_parent.starts_with(&config_root) {
        return Err(BackupError::invalid(
            "backup output must not be inside the config source directory",
        ));
    }
    let partial = partial_path(&options.output_directory)?;
    create_private_directory(&partial)?;
    create_private_directory(&partial.join("database"))?;
    create_private_directory(&partial.join(CONFIG_DIRECTORY))?;

    let database_destination = partial.join(DATABASE_FILE);
    online_backup(&options.database_file, &database_destination)?;
    validate_database(&database_destination)?;
    copy_config_tree(
        &options.config_directory,
        &partial.join(CONFIG_DIRECTORY),
        options.include_secrets,
    )?;

    let mut files = hash_bundle_files(&partial)?;
    files.remove(MANIFEST_FILE);
    let applied = applied_migrations(&database_destination)?;
    let known = known_migrations
        .iter()
        .map(|migration| migration.name)
        .collect::<HashSet<_>>();
    let unknown = applied
        .iter()
        .filter(|name| !known.contains(name.as_str()) && !is_compatible_historical_migration(name))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(BackupError::invalid(format!(
            "database contains migrations unknown to this binary: {}",
            unknown.join(", ")
        )));
    }
    let manifest = BackupManifest {
        format_version: 1,
        application_version: options.application_version.clone(),
        created_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| BackupError::io("system clock is before Unix epoch", error))?
            .as_secs(),
        includes_secret_material: options.include_secrets,
        database_migrations: applied,
        files,
    };
    write_private_file(
        &partial.join(MANIFEST_FILE),
        toml::to_string_pretty(&manifest)
            .map_err(|error| BackupError::io("serialize backup manifest", error))?
            .as_bytes(),
    )?;
    fs::rename(&partial, &options.output_directory)
        .map_err(|error| BackupError::io("commit backup directory", error))?;

    let warnings = if options.include_secrets {
        vec![
            "恢复包允许包含配置目录内的 .env 与主密钥等敏感材料，必须加密离线保存并限制访问权限；它不是完整部署备份。"
                .to_owned(),
        ]
    } else {
        vec![
            "默认恢复包不包含 .env、secrets/ 或主密钥；灾备需另行安全保存同期主密钥、外部 secret 与部署文件。"
                .to_owned(),
        ]
    };
    Ok(BackupReport {
        output_directory: options.output_directory.clone(),
        file_count: manifest.files.len(),
        includes_secret_material: options.include_secrets,
        warnings,
    })
}

pub fn verify_backup(
    bundle: &Path,
    known_migrations: &[SqliteMigration],
) -> Result<BackupManifest, BackupError> {
    require_regular_directory(bundle, "backup bundle")?;
    let manifest_path = bundle.join(MANIFEST_FILE);
    let manifest_bytes = read_regular_file(&manifest_path, "backup manifest")?;
    let manifest = toml::from_str::<BackupManifest>(
        std::str::from_utf8(&manifest_bytes)
            .map_err(|_| BackupError::invalid("backup manifest must be UTF-8"))?,
    )
    .map_err(|error| BackupError::invalid(format!("invalid backup manifest: {error}")))?;
    if manifest.format_version != 1 {
        return Err(BackupError::invalid(format!(
            "unsupported backup format version {}",
            manifest.format_version
        )));
    }
    let actual = hash_bundle_files(bundle)?;
    let actual = actual
        .into_iter()
        .filter(|(path, _)| path != MANIFEST_FILE)
        .collect::<BTreeMap<_, _>>();
    if actual != manifest.files {
        return Err(BackupError::invalid(
            "backup file list or SHA-256 digest does not match manifest",
        ));
    }
    validate_database(&bundle.join(DATABASE_FILE))?;
    let applied = applied_migrations(&bundle.join(DATABASE_FILE))?;
    if applied != manifest.database_migrations {
        return Err(BackupError::invalid(
            "database migration list does not match manifest",
        ));
    }
    let known = known_migrations
        .iter()
        .map(|migration| migration.name)
        .collect::<HashSet<_>>();
    let unknown = applied
        .iter()
        .filter(|name| !known.contains(name.as_str()) && !is_compatible_historical_migration(name))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(BackupError::invalid(format!(
            "backup requires a newer binary; unknown migrations: {}",
            unknown.join(", ")
        )));
    }
    Ok(manifest)
}

pub fn plan_restore(
    bundle: &Path,
    target_root: &Path,
    known_migrations: &[SqliteMigration],
) -> Result<RestorePlan, BackupError> {
    let manifest = verify_backup(bundle, known_migrations)?;
    if target_overlaps_bundle(bundle, target_root)? {
        return Err(BackupError::invalid(
            "restore target must not be inside the backup bundle",
        ));
    }
    ensure_target_is_empty(target_root)?;
    let mut warnings =
        vec!["恢复目标必须是停止服务后的全新实例目录；本命令不会覆盖现有运行目录。".to_owned()];
    if !manifest.includes_secret_material {
        warnings.push(
            "该恢复包不含主密钥和外部 secret；数据库若有加密受管配置，启动前必须恢复同期主密钥，不能重新生成。"
                .to_owned(),
        );
    }
    Ok(RestorePlan {
        target_root: target_root.to_path_buf(),
        database_destination: target_root.join("data/storage/app.db"),
        config_destination: target_root.join(CONFIG_DIRECTORY),
        file_count: manifest.files.len(),
        includes_secret_material: manifest.includes_secret_material,
        warnings,
    })
}

fn target_overlaps_bundle(bundle: &Path, target: &Path) -> Result<bool, BackupError> {
    let bundle = fs::canonicalize(bundle)
        .map_err(|error| BackupError::io("resolve backup bundle", error))?;
    let target_parent = target.parent().unwrap_or_else(|| Path::new("."));
    let target_parent = fs::canonicalize(target_parent)
        .map_err(|error| BackupError::io("resolve restore target parent", error))?;
    Ok(target_parent.starts_with(&bundle))
}

/// 只恢复到不存在或为空的目录，避免在仍运行的进程旁替换 SQLite inode 或覆盖私有配置。
pub fn restore_backup(
    bundle: &Path,
    target_root: &Path,
    known_migrations: &[SqliteMigration],
) -> Result<RestorePlan, BackupError> {
    let plan = plan_restore(bundle, target_root, known_migrations)?;
    create_private_directory_if_missing(target_root)?;
    create_private_directory(&target_root.join("data"))?;
    create_private_directory(&target_root.join("data/storage"))?;
    copy_regular_file(&bundle.join(DATABASE_FILE), &plan.database_destination)?;
    copy_tree_all(&bundle.join(CONFIG_DIRECTORY), &plan.config_destination)?;
    validate_database(&plan.database_destination)?;
    Ok(plan)
}

fn online_backup(source: &Path, destination: &Path) -> Result<(), BackupError> {
    let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| BackupError::io("open SQLite source", error))?;
    let mut destination = Connection::open_with_flags(
        destination,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|error| BackupError::io("create SQLite backup", error))?;
    let backup = Backup::new(&source, &mut destination)
        .map_err(|error| BackupError::io("start SQLite online backup", error))?;
    backup
        .run_to_completion(128, Duration::from_millis(10), None)
        .map_err(|error| BackupError::io("copy SQLite online backup", error))
}

fn validate_database(path: &Path) -> Result<(), BackupError> {
    require_regular_file(path, "backup database")?;
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| BackupError::io("open backup database", error))?;
    let result = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map_err(|error| BackupError::io("run SQLite integrity_check", error))?;
    if result != "ok" {
        return Err(BackupError::invalid(format!(
            "SQLite integrity_check failed: {result}"
        )));
    }
    Ok(())
}

fn applied_migrations(path: &Path) -> Result<Vec<String>, BackupError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| BackupError::io("open database migration metadata", error))?;
    let table_exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='app_sqlite_migrations')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| BackupError::io("inspect database migration metadata", error))?;
    if !table_exists {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare("SELECT name FROM app_sqlite_migrations ORDER BY rowid")
        .map_err(|error| BackupError::io("read database migration metadata", error))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| BackupError::io("query database migration metadata", error))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| BackupError::io("decode database migration metadata", error))
}

fn copy_config_tree(
    source: &Path,
    destination: &Path,
    include_secrets: bool,
) -> Result<(), BackupError> {
    for entry in
        fs::read_dir(source).map_err(|error| BackupError::io("read config directory", error))?
    {
        let entry = entry.map_err(|error| BackupError::io("read config entry", error))?;
        let relative = PathBuf::from(entry.file_name());
        if should_skip_config_path(&relative, include_secrets) {
            continue;
        }
        copy_tree_entry(
            &entry.path(),
            &destination.join(&relative),
            &relative,
            include_secrets,
        )?;
    }
    Ok(())
}

fn copy_tree_entry(
    source: &Path,
    destination: &Path,
    relative: &Path,
    include_secrets: bool,
) -> Result<(), BackupError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| BackupError::io("inspect config entry", error))?;
    if metadata.file_type().is_symlink() {
        return Err(BackupError::invalid(format!(
            "config backup refuses symbolic link: {}",
            relative.display()
        )));
    }
    if metadata.is_dir() {
        create_private_directory(destination)?;
        for entry in fs::read_dir(source)
            .map_err(|error| BackupError::io("read config subdirectory", error))?
        {
            let entry = entry.map_err(|error| BackupError::io("read config entry", error))?;
            let child_relative = relative.join(entry.file_name());
            if should_skip_config_path(&child_relative, include_secrets) {
                continue;
            }
            copy_tree_entry(
                &entry.path(),
                &destination.join(entry.file_name()),
                &child_relative,
                include_secrets,
            )?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(BackupError::invalid(format!(
            "config backup only supports regular files and directories: {}",
            relative.display()
        )));
    }
    copy_regular_file(source, destination)
}

fn should_skip_config_path(relative: &Path, include_secrets: bool) -> bool {
    if relative
        .file_name()
        .is_some_and(|name| name == "bootstrap.token")
    {
        return true;
    }
    if include_secrets {
        return false;
    }
    relative == Path::new(".env")
        || relative.components().next().is_some_and(
            |component| matches!(component, Component::Normal(name) if name == "secrets"),
        )
}

fn copy_tree_all(source: &Path, destination: &Path) -> Result<(), BackupError> {
    require_regular_directory(source, "backup config directory")?;
    create_private_directory(destination)?;
    for entry in
        fs::read_dir(source).map_err(|error| BackupError::io("read backup config", error))?
    {
        let entry = entry.map_err(|error| BackupError::io("read backup config entry", error))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| BackupError::io("inspect backup config entry", error))?;
        if metadata.file_type().is_symlink() {
            return Err(BackupError::invalid(
                "backup bundle contains a symbolic link",
            ));
        }
        if metadata.is_dir() {
            copy_tree_all(&entry.path(), &destination.join(entry.file_name()))?;
        } else if metadata.is_file() {
            copy_regular_file(&entry.path(), &destination.join(entry.file_name()))?;
        } else {
            return Err(BackupError::invalid(
                "backup bundle contains a special file",
            ));
        }
    }
    Ok(())
}

fn hash_bundle_files(root: &Path) -> Result<BTreeMap<String, String>, BackupError> {
    let mut output = BTreeMap::new();
    hash_tree(root, root, &mut output)?;
    Ok(output)
}

fn hash_tree(
    root: &Path,
    current: &Path,
    output: &mut BTreeMap<String, String>,
) -> Result<(), BackupError> {
    for entry in
        fs::read_dir(current).map_err(|error| BackupError::io("read backup bundle", error))?
    {
        let entry = entry.map_err(|error| BackupError::io("read backup entry", error))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| BackupError::io("inspect backup entry", error))?;
        if metadata.file_type().is_symlink() {
            return Err(BackupError::invalid(
                "backup bundle must not contain symbolic links",
            ));
        }
        if metadata.is_dir() {
            hash_tree(root, &path, output)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| BackupError::invalid("backup path escaped bundle root"))?;
            let key = relative.to_string_lossy().replace('\\', "/");
            output.insert(key, sha256_file(&path)?);
        } else {
            return Err(BackupError::invalid(
                "backup bundle contains a special file",
            ));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, BackupError> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| BackupError::io("open backup file for hashing", error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| BackupError::io("hash backup file", error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let digest = digest.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    Ok(encoded)
}

fn partial_path(output: &Path) -> Result<PathBuf, BackupError> {
    let name = output
        .file_name()
        .ok_or_else(|| BackupError::invalid("backup output must have a final path component"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| BackupError::io("system clock is before Unix epoch", error))?
        .as_nanos();
    Ok(output.with_file_name(format!(
        ".{}.partial-{}-{stamp}",
        name.to_string_lossy(),
        std::process::id()
    )))
}

fn ensure_target_is_empty(target: &Path) -> Result<(), BackupError> {
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(BackupError::io("inspect restore target", error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(BackupError::invalid(
                "restore target must be a regular directory and not a symbolic link",
            ))
        }
        Ok(_) => {
            let mut entries = fs::read_dir(target)
                .map_err(|error| BackupError::io("read restore target", error))?;
            if entries
                .next()
                .transpose()
                .map_err(|error| BackupError::io("read restore target", error))?
                .is_some()
            {
                Err(BackupError::invalid(
                    "restore target is not empty; refusing to overwrite a running or existing instance",
                ))
            } else {
                Ok(())
            }
        }
    }
}

fn require_regular_file(path: &Path, label: &str) -> Result<(), BackupError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| BackupError::io(&format!("inspect {label}"), error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BackupError::invalid(format!(
            "{label} must be a regular file and not a symbolic link"
        )));
    }
    Ok(())
}

fn require_regular_directory(path: &Path, label: &str) -> Result<(), BackupError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| BackupError::io(&format!("inspect {label}"), error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(BackupError::invalid(format!(
            "{label} must be a directory and not a symbolic link"
        )));
    }
    Ok(())
}

fn ensure_backup_output_parent(parent: &Path) -> Result<(), BackupError> {
    match fs::symlink_metadata(parent) {
        Ok(_) => require_regular_directory(parent, "backup output parent"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let grandparent = parent.parent().unwrap_or_else(|| Path::new("."));
            require_regular_directory(grandparent, "backup output parent parent")?;
            // 只自动建立一级专用目录，避免拼写错误时悄悄创建整条任意路径。
            create_private_directory(parent)
        }
        Err(error) => Err(BackupError::io("inspect backup output parent", error)),
    }
}

fn create_private_directory(path: &Path) -> Result<(), BackupError> {
    fs::create_dir(path).map_err(|error| BackupError::io("create private directory", error))?;
    set_private_directory_permissions(path)
}

fn create_private_directory_if_missing(path: &Path) -> Result<(), BackupError> {
    match fs::create_dir(path) {
        Ok(()) => set_private_directory_permissions(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(BackupError::io("create restore target", error)),
    }
}

fn copy_regular_file(source: &Path, destination: &Path) -> Result<(), BackupError> {
    require_regular_file(source, "backup source file")?;
    let bytes = read_regular_file(source, "backup source file")?;
    write_private_file(destination, &bytes)
}

fn read_regular_file(path: &Path, label: &str) -> Result<Vec<u8>, BackupError> {
    require_regular_file(path, label)?;
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| BackupError::io(&format!("open {label}"), error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| BackupError::io(&format!("read {label}"), error))?;
    Ok(bytes)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| BackupError::io("create backup file", error))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| BackupError::io("persist backup file", error))
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), BackupError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| BackupError::io("restrict directory permissions", error))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), BackupError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::center::{
            ConfigCenter, ConfigCenterPaths, ManagedConfigApplyMode, ManagedConfigField,
            SECRET_MISSING_REVISION,
        },
        runtime::tools::{
            memory::{CreateMemoryRequest, ListMemoryQuery, MemoryStore},
            rss::{RssFeedItem, RssStore, RssTarget, RssTargetType},
            todo::{
                TodoItemDraft, TodoRecurrenceKind, TodoRecurrenceUnit, TodoStore, TodoTimePrecision,
            },
        },
        storage::{
            database::{SqliteDatabase, SqliteMigration},
            migrations::APP_MIGRATIONS,
            session::{SessionMeta, SessionStore},
        },
    };

    const TEST_MIGRATIONS: &[SqliteMigration] = &[SqliteMigration {
        name: "maintenance_backup_v1",
        sql: "CREATE TABLE backup_items (id INTEGER PRIMARY KEY, value TEXT NOT NULL);",
    }];

    fn test_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("qq-maid-{label}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn online_backup_excludes_secrets_and_restores_into_clean_instance() {
        let source = test_directory("backup-source");
        fs::create_dir_all(source.join("config/secrets")).unwrap();
        fs::write(source.join("config/runtime.toml"), "version = 1\n").unwrap();
        fs::write(source.join("config/.env"), "OPENAI_API_KEY=private\n").unwrap();
        fs::write(source.join("config/secrets/master.key"), "private-key").unwrap();
        let database = SqliteDatabase::open(source.join("app.db"), TEST_MIGRATIONS).unwrap();
        database
            .connection()
            .unwrap()
            .execute("INSERT INTO backup_items (value) VALUES ('preserved')", [])
            .unwrap();
        drop(database);

        let bundle = source.join("backups/bundle");
        let report = create_backup(
            &BackupOptions {
                database_file: source.join("app.db"),
                config_directory: source.join("config"),
                output_directory: bundle.clone(),
                include_secrets: false,
                application_version: "test".to_owned(),
            },
            TEST_MIGRATIONS,
        )
        .unwrap();
        assert!(!report.includes_secret_material);
        assert!(!bundle.join("config/.env").exists());
        assert!(!bundle.join("config/secrets/master.key").exists());
        verify_backup(&bundle, TEST_MIGRATIONS).unwrap();

        let target = source.join("restored");
        restore_backup(&bundle, &target, TEST_MIGRATIONS).unwrap();
        let restored = Connection::open(target.join("data/storage/app.db")).unwrap();
        let value: String = restored
            .query_row("SELECT value FROM backup_items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "preserved");
        assert!(target.join("config/runtime.toml").exists());
        let _ = fs::remove_dir_all(source);
    }

    #[test]
    fn encrypted_managed_config_restores_only_with_matching_master_key() {
        let source = test_directory("encrypted-config-backup-source");
        fs::create_dir_all(source.join("config")).unwrap();
        let database_file = source.join("app.db");
        let database = SqliteDatabase::open(&database_file, APP_MIGRATIONS).unwrap();
        let fields = || {
            vec![ManagedConfigField::secret(
                "provider.openai.api_key",
                "OPENAI_API_KEY",
                "core.provider",
                ManagedConfigApplyMode::Restart,
            )]
        };
        let paths = |root: &Path| ConfigCenterPaths {
            managed_config_file: root.join("config/runtime.toml"),
            master_key_file: root.join("config/secrets/master.key"),
        };
        let center = ConfigCenter::open(fields(), paths(&source), database.clone()).unwrap();
        center
            .replace_secret(
                "provider.openai.api_key",
                "restored-secret-value",
                SECRET_MISSING_REVISION,
            )
            .unwrap();
        drop(center);
        drop(database);

        let bundle = source.join("bundle");
        create_backup(
            &BackupOptions {
                database_file,
                config_directory: source.join("config"),
                output_directory: bundle.clone(),
                include_secrets: true,
                application_version: "test".to_owned(),
            },
            APP_MIGRATIONS,
        )
        .unwrap();

        let matching_target = source.join("restored-matching-key");
        restore_backup(&bundle, &matching_target, APP_MIGRATIONS).unwrap();
        let restored_database =
            SqliteDatabase::open(matching_target.join("data/storage/app.db"), APP_MIGRATIONS)
                .unwrap();
        let restored_center =
            ConfigCenter::open(fields(), paths(&matching_target), restored_database).unwrap();
        assert_eq!(
            restored_center
                .resolved_environment(&std::collections::HashMap::new())
                .unwrap()["OPENAI_API_KEY"],
            "restored-secret-value"
        );
        drop(restored_center);

        let missing_target = source.join("restored-missing-key");
        restore_backup(&bundle, &missing_target, APP_MIGRATIONS).unwrap();
        let missing_key = missing_target.join("config/secrets/master.key");
        fs::remove_file(&missing_key).unwrap();
        let missing_database =
            SqliteDatabase::open(missing_target.join("data/storage/app.db"), APP_MIGRATIONS)
                .unwrap();
        let missing_error =
            match ConfigCenter::open(fields(), paths(&missing_target), missing_database) {
                Ok(_) => panic!("encrypted config must reject a missing master key"),
                Err(error) => error,
            };
        assert_eq!(missing_error.code(), "secret_storage_error");
        assert!(
            missing_error
                .message()
                .contains("master key file is missing")
        );
        assert!(!missing_key.exists());

        let wrong_key_source = source.join("wrong-key-source");
        let wrong_key_database =
            SqliteDatabase::open(wrong_key_source.join("app.db"), APP_MIGRATIONS).unwrap();
        let wrong_key_center =
            ConfigCenter::open(fields(), paths(&wrong_key_source), wrong_key_database).unwrap();
        drop(wrong_key_center);
        let wrong_target = source.join("restored-wrong-key");
        restore_backup(&bundle, &wrong_target, APP_MIGRATIONS).unwrap();
        fs::copy(
            wrong_key_source.join("config/secrets/master.key"),
            wrong_target.join("config/secrets/master.key"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                wrong_target.join("config/secrets/master.key"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let wrong_database =
            SqliteDatabase::open(wrong_target.join("data/storage/app.db"), APP_MIGRATIONS).unwrap();
        let wrong_error = match ConfigCenter::open(fields(), paths(&wrong_target), wrong_database) {
            Ok(_) => panic!("encrypted config must reject a mismatched master key"),
            Err(error) => error,
        };
        assert_eq!(wrong_error.code(), "secret_storage_error");
        assert!(wrong_error.message().contains("failed authentication"));

        let _ = fs::remove_dir_all(source);
    }

    #[test]
    fn verification_rejects_modified_bundle() {
        let source = test_directory("backup-tamper");
        fs::create_dir_all(source.join("config")).unwrap();
        fs::write(source.join("config/runtime.toml"), "version = 1\n").unwrap();
        let database = SqliteDatabase::open(source.join("app.db"), TEST_MIGRATIONS).unwrap();
        drop(database);
        let bundle = source.join("bundle");
        create_backup(
            &BackupOptions {
                database_file: source.join("app.db"),
                config_directory: source.join("config"),
                output_directory: bundle.clone(),
                include_secrets: false,
                application_version: "test".to_owned(),
            },
            TEST_MIGRATIONS,
        )
        .unwrap();
        fs::write(bundle.join("config/runtime.toml"), "tampered = true\n").unwrap();

        let error = verify_backup(&bundle, TEST_MIGRATIONS).unwrap_err();
        assert_eq!(error.code(), "invalid_backup");
        let _ = fs::remove_dir_all(source);
    }

    #[test]
    fn backup_then_modify_and_restore_recovers_core_business_data() {
        let source = test_directory("business-backup-source");
        fs::create_dir_all(source.join("config")).unwrap();
        fs::write(source.join("config/runtime.toml"), "version = 1\n").unwrap();
        let database_file = source.join("app.db");
        let owner = TodoStore::owner(Some("backup-user"), "private:backup-user");
        let session_meta = SessionMeta::new(
            "private:backup-user",
            Some("backup-user".to_owned()),
            None,
            None,
            None,
            "qq_official",
        );
        let rss_target = RssTarget {
            target_type: RssTargetType::Private,
            target_id: "backup-user".to_owned(),
            scope_key: "private:backup-user".to_owned(),
        };

        create_business_snapshot(&database_file, &owner, &session_meta, &rss_target, "backup");
        let bundle = source.join("bundle");
        create_backup(
            &BackupOptions {
                database_file: database_file.clone(),
                config_directory: source.join("config"),
                output_directory: bundle.clone(),
                include_secrets: false,
                application_version: "test".to_owned(),
            },
            APP_MIGRATIONS,
        )
        .unwrap();

        // 备份完成后继续写入，恢复结果必须只包含备份时的一组业务数据。
        create_business_snapshot(
            &database_file,
            &owner,
            &session_meta,
            &rss_target,
            "after-backup",
        );
        assert_business_record_counts(&database_file, &owner, &session_meta, &rss_target, 2);

        let target = source.join("restored");
        restore_backup(&bundle, &target, APP_MIGRATIONS).unwrap();
        assert_business_record_counts(
            &target.join("data/storage/app.db"),
            &owner,
            &session_meta,
            &rss_target,
            1,
        );
        let _ = fs::remove_dir_all(source);
    }

    fn create_business_snapshot(
        database_file: &Path,
        owner: &crate::runtime::tools::todo::TodoOwner,
        session_meta: &SessionMeta,
        rss_target: &RssTarget,
        suffix: &str,
    ) {
        let todo_store =
            TodoStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
        todo_store
            .create(
                owner,
                TodoItemDraft {
                    title: format!("backup todo {suffix}"),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                    recurrence_kind: TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();

        let session_store =
            SessionStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
        session_store
            .create(session_meta, format!("backup session {suffix}"), true)
            .unwrap();

        let memory_store =
            MemoryStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
        memory_store
            .create(CreateMemoryRequest {
                user_id: Some("backup-user".to_owned()),
                group_id: None,
                content: format!("backup memory {suffix}"),
                source_text: format!("backup memory source {suffix}"),
                memory_type: "note".to_owned(),
                scope: "general".to_owned(),
            })
            .unwrap();

        let rss_store = RssStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
        rss_store
            .create_subscription(
                rss_target,
                &format!("https://example.test/{suffix}.xml"),
                &format!("backup feed {suffix}"),
                &[RssFeedItem {
                    item_key: format!("item-{suffix}"),
                    revision_hash: format!("revision-{suffix}"),
                    title: format!("backup item {suffix}"),
                    link: None,
                    published_at: None,
                    updated_at: None,
                    summary: None,
                    source_order: 0,
                }],
                50,
            )
            .unwrap();
    }

    fn assert_business_record_counts(
        database_file: &Path,
        owner: &crate::runtime::tools::todo::TodoOwner,
        session_meta: &SessionMeta,
        rss_target: &RssTarget,
        expected: usize,
    ) {
        let todo_store =
            TodoStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
        assert_eq!(todo_store.list_pending(owner).unwrap().len(), expected);

        let session_store =
            SessionStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
        assert_eq!(
            session_store
                .list_for_scope(&session_meta.scope_key, None)
                .unwrap()
                .len(),
            expected
        );

        let memory_store =
            MemoryStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
        assert_eq!(
            memory_store.list(ListMemoryQuery::default()).unwrap().len(),
            expected
        );

        let rss_store = RssStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
        assert_eq!(
            rss_store
                .list_by_scope(&rss_target.scope_key)
                .unwrap()
                .len(),
            expected
        );
    }
}
