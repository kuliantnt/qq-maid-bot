//! 控制台用户偏好与通用文件领域门面。
//!
//! 本模块只使用认证系统提供的管理员 ID 做资源归属，不接受客户端提交其他用户 ID。
//! 文件元数据写入通用 SQLite，文件内容写入数据库同级的持久化目录。

use std::{path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::storage::database::{SqliteDatabase, SqliteMigration};

mod files;
mod preferences;

pub(crate) use files::StagedFileDeletion;

pub const MAX_CONSOLE_FILE_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const MAX_CUSTOM_COLORS: usize = 32;
pub(crate) const MAX_CUSTOM_COLOR_CHARS: usize = 64;
pub(crate) const MAX_BACKGROUND_FILES: usize = 64;
pub(crate) const MAX_ORIGINAL_FILENAME_CHARS: usize = 255;
pub(crate) const MAX_CONTENT_TYPE_CHARS: usize = 255;
pub(crate) const SUPPORTED_BACKGROUND_CONTENT_TYPES: &[&str] = &[
    "image/avif",
    "image/bmp",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/svg+xml",
    "image/webp",
];

pub const CONSOLE_USER_DATA_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "console_user_data_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS console_user_files (
            file_id TEXT PRIMARY KEY,
            admin_id INTEGER NOT NULL,
            original_filename TEXT NOT NULL,
            content_type TEXT NOT NULL,
            size INTEGER NOT NULL CHECK(size >= 0),
            storage_filename TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            FOREIGN KEY(admin_id) REFERENCES console_admins(id) ON DELETE CASCADE
          );
          CREATE INDEX IF NOT EXISTS idx_console_user_files_owner_created
            ON console_user_files(admin_id, created_at DESC, file_id DESC);
          CREATE TABLE IF NOT EXISTS console_user_preferences (
            admin_id INTEGER PRIMARY KEY,
            custom_colors_json TEXT NOT NULL,
            background_file_ids_json TEXT NOT NULL,
            active_background_file_id TEXT,
            kuliantnt INTEGER NOT NULL DEFAULT 0 CHECK(kuliantnt IN (0, 1)),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(admin_id) REFERENCES console_admins(id) ON DELETE CASCADE
          );",
};

/// 背景模式字段独立于自定义背景文件与解锁状态：`default` 表示无背景或由
/// `active_background_file_id` 指定的自定义背景；`special` 表示特殊九宫格（不引用文件）。
/// 通过独立 migration 在旧库上补列，保证已有 `APP_DB_FILE` 历史数据兼容。
pub const CONSOLE_USER_DATA_SCHEMA_V2: SqliteMigration = SqliteMigration {
    name: "console_user_data_background_mode_v2",
    sql: "ALTER TABLE console_user_preferences
            ADD COLUMN background_mode TEXT NOT NULL DEFAULT 'default'
            CHECK(background_mode IN ('default', 'special'));",
};

/// 文件用途隔离：旧表中没有用途的文件默认作为背景文件；当前 PR 已创建知识托管关联的
/// 文件在同一 migration 中恢复为 `knowledge`。该 migration 放在知识库 schema 之后，
/// 这样新库和已有 PR 中间态数据库都能安全执行同一条 SQL。
pub const CONSOLE_USER_DATA_SCHEMA_V3: SqliteMigration = SqliteMigration {
    name: "console_user_data_file_module_v3",
    sql: "ALTER TABLE console_user_files
            ADD COLUMN module TEXT NOT NULL DEFAULT 'background'
            CHECK(module IN ('background', 'knowledge'));
          CREATE INDEX IF NOT EXISTS idx_console_user_files_owner_module_created
            ON console_user_files(admin_id, module, created_at DESC, file_id DESC);
          UPDATE console_user_files
             SET module = 'knowledge'
           WHERE file_id IN (SELECT file_id FROM knowledge_managed_files);",
};

/// 背景模式：`default` 表示无背景或由 `active_background_file_id` 指定的自定义背景；
/// `special` 表示特殊九宫格（不引用文件）。该字段与 `kuliantnt`（仅表示是否解锁）语义分离，
/// 避免用单个布尔值同时承担“解锁”和“当前选择”两个状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundMode {
    #[default]
    Default,
    Special,
}

/// 通用托管文件的业务用途。用途只由后端领域入口决定，不能由客户端上传参数覆盖。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserFileModule {
    Background,
    Knowledge,
}

impl UserFileModule {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Knowledge => "knowledge",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "background" => Some(Self::Background),
            "knowledge" => Some(Self::Knowledge),
            _ => None,
        }
    }
}

impl BackgroundMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Special => "special",
        }
    }
}

impl std::str::FromStr for BackgroundMode {
    type Err = ConsoleUserDataError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "default" => Ok(Self::Default),
            "special" => Ok(Self::Special),
            _ => Err(ConsoleUserDataError::invalid(
                "background_mode must be one of: default, special",
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UserPreferences {
    pub custom_colors: Vec<String>,
    pub background_file_ids: Vec<String>,
    pub active_background_file_id: Option<String>,
    pub background_mode: BackgroundMode,
    pub kuliantnt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PreferenceValuePatch<T> {
    #[default]
    Unchanged,
    Clear,
    Set(T),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserPreferencesPatch {
    pub custom_colors: Option<Vec<String>>,
    pub background_file_ids: Option<Vec<String>>,
    pub active_background_file_id: PreferenceValuePatch<String>,
    pub background_mode: Option<BackgroundMode>,
    pub kuliantnt: Option<bool>,
}

impl UserPreferencesPatch {
    pub fn is_empty(&self) -> bool {
        self.custom_colors.is_none()
            && self.background_file_ids.is_none()
            && self.active_background_file_id == PreferenceValuePatch::Unchanged
            && self.background_mode.is_none()
            && self.kuliantnt.is_none()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UserFile {
    pub file_id: String,
    pub filename: String,
    pub content_type: String,
    pub module: UserFileModule,
    pub size: u64,
    pub created_at: String,
    #[serde(skip)]
    pub(crate) storage_filename: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFileContent {
    pub metadata: UserFile,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFilePage {
    pub items: Vec<UserFile>,
    pub total_count: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ConsoleUserDataError {
    code: &'static str,
    message: String,
}

impl ConsoleUserDataError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found",
            message: message.into(),
        }
    }

    pub(crate) fn storage(message: impl Into<String>) -> Self {
        Self {
            code: "storage_error",
            message: message.into(),
        }
    }
}

#[derive(Clone)]
pub struct ConsoleUserDataService {
    pub(crate) database: SqliteDatabase,
    pub(crate) file_root: Arc<PathBuf>,
}

impl ConsoleUserDataService {
    pub fn new(database: SqliteDatabase) -> Self {
        let parent = database
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        Self {
            database,
            file_root: Arc::new(parent.join("console-files")),
        }
    }
}

pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub(crate) fn validate_file_id(file_id: &str) -> Result<(), ConsoleUserDataError> {
    let parsed = uuid::Uuid::parse_str(file_id)
        .map_err(|_| ConsoleUserDataError::invalid("file_id must be a canonical UUID"))?;
    if parsed.hyphenated().to_string() != file_id {
        return Err(ConsoleUserDataError::invalid(
            "file_id must be a canonical UUID",
        ));
    }
    Ok(())
}
