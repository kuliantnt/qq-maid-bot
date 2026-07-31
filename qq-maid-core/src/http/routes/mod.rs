//! HTTP 路由和请求处理器。
//!
//! 定义进程级 `/healthz`、控制台和 Markdown 预览接口。
//!
//! Gateway 与 Core 之间的业务调用已经改为进程内 `CoreService`，这里不再公开
//! 内部 respond 或 SSE 传入口，避免同进程组件保留长期双轨。

use qq_maid_llm::provider::{DynLlmProvider, status::UpstreamStatus};
use std::{sync::Arc, time::Instant};

use crate::{
    config::{AppConfig, center::ConfigCenter},
    http::console::{
        ConsoleCoreSummary, ConsoleRestartController, ConsoleStatusSource, ConsoleToolMetadata,
        DynConsoleStatusSource, EmptyConsoleStatusSource,
    },
    management::AdminAuth,
    management::ConsoleUserDataService,
    runtime::tools::todo::TodoManagementService,
};

pub use super::router_builder::build_router;
/// 运维 HTTP 接口需要的最小配置。
#[derive(Clone)]
pub struct OpsHttpConfig {
    pub web_console_enabled: bool,
    pub web_console_allowed_origins: Vec<String>,
    pub web_console_trusted_proxy_ips: Vec<std::net::IpAddr>,
    pub web_console_secure_cookies: bool,
}

impl From<&AppConfig> for OpsHttpConfig {
    fn from(value: &AppConfig) -> Self {
        Self {
            web_console_enabled: value.web_console_enabled,
            web_console_allowed_origins: value.web_console_allowed_origins.clone(),
            web_console_trusted_proxy_ips: value.web_console_trusted_proxy_ips.clone(),
            web_console_secure_cookies: value.web_console_secure_cookies,
        }
    }
}

/// 运维 HTTP 全局状态，通过 Axum 的 State 注入到各处理器中。
#[derive(Clone)]
pub struct OpsHttpState {
    pub config: OpsHttpConfig,
    /// LLM 提供商（可为主备模式）。
    pub provider: Option<DynLlmProvider>,
    /// 最近一次真实上游调用的脱敏状态。
    pub upstream_status: UpstreamStatus,
    /// Core 自身的安全配置与启动时刻摘要。
    pub core_summary: ConsoleCoreSummary,
    /// Gateway 等接入层提供的只读运行态；不得在 snapshot 中执行外部探测。
    pub console_status_source: DynConsoleStatusSource,
    /// 配置中心领域能力；HTTP 读写都必须先通过部署管理员认证。
    pub config_center: Option<ConfigCenter>,
    /// 配置 WebUI 与后续 Memory WebUI 统一复用的部署管理员安全边界。
    pub admin_auth: Option<AdminAuth>,
    /// Todo 管理领域门面；Handler 不直接持有数据库或通知 Store。
    pub(crate) todo_management: Option<TodoManagementService>,
    /// 控制台用户私有偏好与通用文件领域门面。
    pub(crate) console_user_data: Option<ConsoleUserDataService>,
    /// 当前进程真实注册的 Tool 元数据，供 WebUI 动态展示白名单选项。
    pub registered_tools: Arc<Vec<ConsoleToolMetadata>>,
    /// 仅复用部署目录中的受控 botctl 脚本，不直接操作 systemd 或 Docker。
    pub restart_controller: ConsoleRestartController,
    /// 缺少 Provider 或平台入口时仍开放管理恢复入口，但不能伪报机器人已经就绪。
    pub setup_required: bool,
}

impl OpsHttpState {
    pub fn with_registered_tools(mut self, tools: Vec<ConsoleToolMetadata>) -> Self {
        self.registered_tools = Arc::new(tools);
        self
    }

    pub(crate) fn with_todo_management(mut self, service: TodoManagementService) -> Self {
        self.todo_management = Some(service);
        self
    }

    pub(crate) fn with_console_user_data(mut self, service: ConsoleUserDataService) -> Self {
        self.console_user_data = Some(service);
        self
    }

    pub fn from_parts(
        config: OpsHttpConfig,
        provider: DynLlmProvider,
        upstream_status: UpstreamStatus,
    ) -> Self {
        Self {
            config,
            provider: Some(provider),
            upstream_status,
            core_summary: ConsoleCoreSummary {
                application_version: "test-version".to_owned(),
                started_at: "unix:0".to_owned(),
                started_instant: Instant::now(),
                listen_summary: "127.0.0.1:8787".to_owned(),
                database_path: "data/storage/app.db".to_owned(),
                provider_configured: true,
                rss_enabled: true,
                tool_calling_enabled: true,
            },
            console_status_source: Arc::new(EmptyConsoleStatusSource),
            config_center: None,
            admin_auth: None,
            todo_management: None,
            console_user_data: None,
            registered_tools: Arc::new(Vec::new()),
            restart_controller: ConsoleRestartController::default(),
            setup_required: false,
        }
    }

    pub fn from_config(
        config: &AppConfig,
        provider: DynLlmProvider,
        upstream_status: UpstreamStatus,
        console_status_source: Arc<dyn ConsoleStatusSource>,
        application_version: &str,
    ) -> Self {
        Self::from_config_with_center(
            config,
            provider,
            upstream_status,
            console_status_source,
            application_version,
            None,
            None,
        )
    }

    pub fn from_config_with_center(
        config: &AppConfig,
        provider: DynLlmProvider,
        upstream_status: UpstreamStatus,
        console_status_source: Arc<dyn ConsoleStatusSource>,
        application_version: &str,
        config_center: Option<ConfigCenter>,
        admin_auth: Option<AdminAuth>,
    ) -> Self {
        Self {
            config: config.into(),
            provider: Some(provider),
            upstream_status,
            core_summary: ConsoleCoreSummary::from_config(config, application_version),
            console_status_source,
            config_center,
            admin_auth,
            todo_management: None,
            console_user_data: None,
            registered_tools: Arc::new(Vec::new()),
            restart_controller: ConsoleRestartController::from_current_dir(),
            setup_required: false,
        }
    }

    pub fn setup_required(
        config: OpsHttpConfig,
        core_summary: ConsoleCoreSummary,
        config_center: ConfigCenter,
        admin_auth: Option<AdminAuth>,
    ) -> Self {
        Self {
            config,
            provider: None,
            upstream_status: UpstreamStatus::default(),
            core_summary,
            console_status_source: Arc::new(EmptyConsoleStatusSource),
            config_center: Some(config_center),
            admin_auth,
            todo_management: None,
            console_user_data: None,
            registered_tools: Arc::new(Vec::new()),
            restart_controller: ConsoleRestartController::from_current_dir(),
            setup_required: true,
        }
    }
}

// 控制台静态资源、状态和安全响应头由 console_routes 模块负责。

#[cfg(test)]
mod tests;
