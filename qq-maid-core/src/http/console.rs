//! Web 控制台只读状态契约。
//!
//! HTTP 层只消费安全摘要；Gateway 可实现 [`ConsoleStatusSource`] 提供进程内观测，
//! 但不得把平台凭据或协议对象反向暴露给 Core。

use std::{
    fs::{self, File},
    io::ErrorKind,
    path::Path,
    sync::Arc,
    time::Instant,
};

use qq_maid_common::time_context::now_unix_seconds_marker;
use serde::Serialize;

use crate::{config::AppConfig, storage::APP_MIGRATIONS};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleValueState {
    Supported,
    Disabled,
    Unsupported,
    Unknown,
    NotAvailable,
    NotConfigured,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleRuntimeState {
    Online,
    Offline,
    Available,
    Unknown,
    NotAvailable,
    NotConfigured,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsoleCapabilities {
    pub text: ConsoleValueState,
    pub markdown: ConsoleValueState,
    pub image: ConsoleValueState,
    pub file: ConsoleValueState,
    pub mixed_message: ConsoleValueState,
    pub streaming: ConsoleValueState,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsoleDirectionalCapabilities {
    pub inbound: ConsoleCapabilities,
    pub outbound: ConsoleCapabilities,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsoleCapabilityScope {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub capabilities: ConsoleDirectionalCapabilities,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsolePlatformStatus {
    pub id: String,
    pub label: String,
    pub configured: bool,
    pub enabled: bool,
    pub state: ConsoleRuntimeState,
    pub last_event_at: Option<String>,
    pub last_error_summary: Option<String>,
    pub ready_at: Option<String>,
    pub resumed_at: Option<String>,
    pub capability_scopes: Vec<ConsoleCapabilityScope>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsoleStorageStatus {
    pub id: String,
    pub label: String,
    pub path_summary: String,
    pub state: ConsoleRuntimeState,
    pub exists: Option<bool>,
    pub readable: Option<bool>,
    pub writable: Option<bool>,
    pub error_summary: Option<String>,
    pub schema_summary: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ConsoleExternalSnapshot {
    pub platforms: Vec<ConsolePlatformStatus>,
    pub storage: Vec<ConsoleStorageStatus>,
}

/// 平台接入层提供的只读、安全状态源；调用必须只读且不得执行网络探测。
pub trait ConsoleStatusSource: Send + Sync {
    fn snapshot(&self) -> ConsoleExternalSnapshot;
}

#[derive(Default)]
pub struct EmptyConsoleStatusSource;

impl ConsoleStatusSource for EmptyConsoleStatusSource {
    fn snapshot(&self) -> ConsoleExternalSnapshot {
        ConsoleExternalSnapshot::default()
    }
}

pub type DynConsoleStatusSource = Arc<dyn ConsoleStatusSource>;

#[derive(Clone)]
pub struct ConsoleCoreSummary {
    pub application_version: String,
    pub started_at: String,
    pub started_instant: Instant,
    pub listen_summary: String,
    pub database_path: String,
    pub provider_configured: bool,
    pub rss_enabled: bool,
    pub tool_calling_enabled: bool,
}

impl ConsoleCoreSummary {
    pub fn from_config(config: &AppConfig, application_version: &str) -> Self {
        Self {
            application_version: application_version.to_owned(),
            started_at: now_unix_seconds_marker(),
            started_instant: Instant::now(),
            listen_summary: safe_listen_summary(&config.server_host, config.server_port),
            database_path: config.app_db_file.clone(),
            // Provider 已在 OpsHttpState 创建前完成构造；这里只表达配置是否已通过启动校验。
            provider_configured: true,
            rss_enabled: config.rss_enabled,
            tool_calling_enabled: config
                .agent_config
                .resolve(crate::config::ChatScene::Private)
                .expect("agent config is validated during startup")
                .tool_calling_enabled,
        }
    }

    pub fn core_storage(&self) -> Vec<ConsoleStorageStatus> {
        vec![
            path_storage(
                "database",
                "SQLite 数据库",
                Path::new(&self.database_path),
                Some(format!(
                    "已加载 {} 项 migration，最新：{}",
                    APP_MIGRATIONS.len(),
                    APP_MIGRATIONS
                        .last()
                        .map(|migration| migration.name)
                        .unwrap_or("not_available")
                )),
            ),
            ConsoleStorageStatus {
                id: "cache".to_owned(),
                label: "缓存目录".to_owned(),
                path_summary: "当前无统一磁盘缓存目录".to_owned(),
                state: ConsoleRuntimeState::NotAvailable,
                exists: None,
                readable: None,
                writable: None,
                error_summary: None,
                schema_summary: None,
            },
        ]
    }
}

pub fn path_storage(
    id: &str,
    label: &str,
    path: &Path,
    schema_summary: Option<String>,
) -> ConsoleStorageStatus {
    let probe = probe_path(path);
    ConsoleStorageStatus {
        id: id.to_owned(),
        label: label.to_owned(),
        path_summary: safe_path_summary(path),
        state: probe.state,
        exists: probe.exists,
        readable: probe.readable,
        // 只读控制台不执行写入测试；权限位不能证明当前进程真实可写。
        writable: None,
        error_summary: probe.error_summary,
        schema_summary,
    }
}

struct PathProbe {
    state: ConsoleRuntimeState,
    exists: Option<bool>,
    readable: Option<bool>,
    error_summary: Option<String>,
}

fn probe_path(path: &Path) -> PathProbe {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return PathProbe {
                state: ConsoleRuntimeState::NotAvailable,
                exists: Some(false),
                readable: Some(false),
                error_summary: Some("not_found".to_owned()),
            };
        }
        Err(error) => {
            return PathProbe {
                state: ConsoleRuntimeState::Unknown,
                exists: None,
                readable: None,
                error_summary: Some(safe_io_error_summary(&error)),
            };
        }
    };

    let readable_result = if metadata.is_file() {
        File::open(path).map(|_| ())
    } else if metadata.is_dir() {
        fs::read_dir(path).map(|_| ())
    } else {
        return PathProbe {
            state: ConsoleRuntimeState::Unknown,
            exists: Some(true),
            readable: None,
            error_summary: Some("unsupported_path_type".to_owned()),
        };
    };

    match readable_result {
        Ok(()) => PathProbe {
            state: ConsoleRuntimeState::Available,
            exists: Some(true),
            readable: Some(true),
            error_summary: None,
        },
        Err(error) => PathProbe {
            state: ConsoleRuntimeState::Unknown,
            exists: Some(true),
            readable: Some(false),
            error_summary: Some(safe_io_error_summary(&error)),
        },
    }
}

fn safe_io_error_summary(error: &std::io::Error) -> String {
    match error.kind() {
        ErrorKind::NotFound => "not_found",
        ErrorKind::PermissionDenied => "permission_denied",
        ErrorKind::NotADirectory => "invalid_path",
        ErrorKind::IsADirectory => "invalid_path_type",
        _ => "io_error",
    }
    .to_owned()
}

pub fn safe_path_summary(path: &Path) -> String {
    if path.is_absolute() {
        return path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("…/{name}"))
            .unwrap_or_else(|| "absolute_path".to_owned());
    }
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn safe_listen_summary(host: &str, port: u16) -> String {
    match host.trim() {
        "0.0.0.0" | "::" => format!("all_interfaces:{port}"),
        host => format!("{host}:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_path(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("qq-maid-console-{name}-{nonce}"))
    }

    #[test]
    fn absolute_storage_path_only_exposes_filename() {
        assert_eq!(
            safe_path_summary(Path::new("/home/private/app.db")),
            "…/app.db"
        );
    }

    #[test]
    fn unavailable_states_have_stable_wire_values() {
        assert_eq!(
            serde_json::to_string(&ConsoleValueState::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(
            serde_json::to_string(&ConsoleValueState::NotAvailable).unwrap(),
            "\"not_available\""
        );
        assert_eq!(
            serde_json::to_string(&ConsoleValueState::NotConfigured).unwrap(),
            "\"not_configured\""
        );
    }

    #[test]
    fn existing_readable_file_is_available_without_claiming_writable() {
        let path = test_path("readable-file");
        fs::write(&path, b"test").unwrap();

        let status = path_storage("test", "测试文件", &path, None);

        assert_eq!(status.state, ConsoleRuntimeState::Available);
        assert_eq!(status.exists, Some(true));
        assert_eq!(status.readable, Some(true));
        assert_eq!(status.writable, None);
        assert_eq!(status.error_summary, None);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn existing_readable_directory_is_available_without_claiming_writable() {
        let path = test_path("readable-directory");
        fs::create_dir(&path).unwrap();

        let status = path_storage("test", "测试目录", &path, None);

        assert_eq!(status.state, ConsoleRuntimeState::Available);
        assert_eq!(status.exists, Some(true));
        assert_eq!(status.readable, Some(true));
        assert_eq!(status.writable, None);
        assert_eq!(status.error_summary, None);
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn missing_path_is_not_available_and_not_readable() {
        let path = test_path("missing");

        let status = path_storage("test", "缺失路径", &path, None);

        assert_eq!(status.state, ConsoleRuntimeState::NotAvailable);
        assert_eq!(status.exists, Some(false));
        assert_eq!(status.readable, Some(false));
        assert_eq!(status.writable, None);
        assert_eq!(status.error_summary.as_deref(), Some("not_found"));
    }

    #[test]
    fn metadata_error_does_not_masquerade_as_missing_or_readable() {
        let file = test_path("not-directory");
        fs::write(&file, b"test").unwrap();
        let invalid = file.join("child");

        let status = path_storage("test", "无效路径", &invalid, None);

        assert_eq!(status.state, ConsoleRuntimeState::Unknown);
        assert_eq!(status.exists, None);
        assert_eq!(status.readable, None);
        assert_eq!(status.writable, None);
        assert!(matches!(
            status.error_summary.as_deref(),
            Some("invalid_path" | "io_error")
        ));
        assert!(
            !status
                .error_summary
                .unwrap()
                .contains(file.to_string_lossy().as_ref())
        );
        fs::remove_file(file).unwrap();
    }
}
