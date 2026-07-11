//! Web 控制台只读状态契约。
//!
//! HTTP 层只消费安全摘要；Gateway 可实现 [`ConsoleStatusSource`] 提供进程内观测，
//! 但不得把平台凭据或协议对象反向暴露给 Core。

use std::{fs, path::Path, sync::Arc, time::Instant};

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
    pub capabilities: ConsoleDirectionalCapabilities,
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
            tool_calling_enabled: config.tool_calling_enabled,
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
    let metadata = fs::metadata(path).ok();
    let exists = path.exists();
    let readable = metadata
        .as_ref()
        .map(|metadata| metadata.is_file() || metadata.is_dir());
    // 这里只读取权限位摘要，不尝试创建或写入文件。
    let writable = metadata
        .as_ref()
        .map(|metadata| !metadata.permissions().readonly());
    ConsoleStorageStatus {
        id: id.to_owned(),
        label: label.to_owned(),
        path_summary: safe_path_summary(path),
        state: if exists {
            ConsoleRuntimeState::Online
        } else {
            ConsoleRuntimeState::NotAvailable
        },
        exists: Some(exists),
        readable,
        writable,
        schema_summary,
    }
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
}
