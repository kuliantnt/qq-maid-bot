//! 应用启动模块。负责初始化日志、加载配置、构建各个运行时组件，
//! 并启动 Axum HTTP 服务。

use std::{future::Future, net::SocketAddr, sync::Arc};

use time::{UtcOffset, macros::format_description};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::{
    config::{AppConfig, ManagementBootstrapConfig, center::ConfigCenter},
    http::console::ConsoleCoreSummary,
    http::console::{DynConsoleStatusSource, EmptyConsoleStatusSource},
    http::routes::{OpsHttpState, build_router},
    management::AdminAuth,
    runtime::push::PushSink,
    storage::database::SqliteDatabase,
};

mod runtime;
mod workers;

pub use runtime::{CoreExecutors, CoreRuntimeState, CoreStores};
use workers::CoreWorkers;

/// 统一进程入口会先组装 Core 运行时，再决定何时开始监听和何时关停。
/// 这样既能把聊天调用交给进程内 CoreService，也能避免双入口重复初始化 dotenv 和 tracing。
pub struct LlmRuntime {
    addr: SocketAddr,
    core_state: CoreRuntimeState,
    http_state: OpsHttpState,
    workers: CoreWorkers,
}

/// Provider 或平台尚未配置时的降级运行态。它只启动健康检查和受保护管理面，
/// 不构造 Provider、业务 Store、后台 worker 或 Gateway，因此不会伪装机器人已可用。
pub struct ManagementRuntime {
    addr: SocketAddr,
    http_state: OpsHttpState,
}

impl ManagementRuntime {
    pub fn new(
        config: ManagementBootstrapConfig,
        config_center: ConfigCenter,
        admin_auth: AdminAuth,
        application_version: &str,
    ) -> anyhow::Result<Self> {
        let addr: SocketAddr = format!("{}:{}", config.server_host, config.server_port).parse()?;
        let summary = ConsoleCoreSummary::setup_required(
            application_version,
            &config.server_host,
            config.server_port,
            &config.app_db_file,
        );
        let http_state = OpsHttpState::setup_required(
            crate::http::routes::OpsHttpConfig {
                web_console_enabled: config.web_console_enabled,
                web_console_allowed_origins: config.web_console_allowed_origins,
            },
            summary,
            config_center,
            admin_auth,
        );
        Ok(Self { addr, http_state })
    }

    pub async fn serve_with_shutdown<F>(self, shutdown: F) -> anyhow::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        tracing::info!(addr = %self.addr, state = "setup_required", "qq-maid management runtime listening");
        axum::serve(listener, build_router(self.http_state))
            .with_graceful_shutdown(shutdown)
            .await?;
        Ok(())
    }
}

/// 应用入口：加载环境变量、初始化日志、构建配置与运行时、启动 HTTP 服务。
pub async fn run() -> anyhow::Result<()> {
    load_dotenv_files();
    init_tracing()?;
    run_with_config(AppConfig::from_env()?).await
}

/// 统一入口复用当前配置解析与组件装配，但把真正 `serve` 的时机交给调用方控制。
pub async fn run_with_config(config: AppConfig) -> anyhow::Result<()> {
    LlmRuntime::from_config(config)?.serve().await
}

impl LlmRuntime {
    pub fn from_config(config: AppConfig) -> anyhow::Result<Self> {
        Self::from_config_with_push_sink(config, None)
    }

    pub fn from_config_with_push_sink(
        config: AppConfig,
        push_sink: Option<Arc<dyn PushSink>>,
    ) -> anyhow::Result<Self> {
        Self::from_config_with_push_sink_and_console_source(
            config,
            push_sink,
            Arc::new(EmptyConsoleStatusSource),
            env!("CARGO_PKG_VERSION"),
        )
    }

    /// 统一入口注入 Gateway 的只读状态源；独立 Core 调用保持空状态源。
    pub fn from_config_with_push_sink_and_console_source(
        config: AppConfig,
        push_sink: Option<Arc<dyn PushSink>>,
        console_status_source: DynConsoleStatusSource,
        application_version: &'static str,
    ) -> anyhow::Result<Self> {
        let database = SqliteDatabase::open_with_pool_size(
            config.app_db_file.clone(),
            crate::storage::APP_MIGRATIONS,
            config.sqlite_pool_size,
        )?;
        Self::from_config_with_database_push_sink_and_console_source(
            config,
            database,
            None,
            None,
            push_sink,
            console_status_source,
            application_version,
        )
    }

    /// 统一程序入口注入已经用于解析加密配置的数据库和配置中心。
    pub fn from_config_with_database_push_sink_and_console_source(
        config: AppConfig,
        database: SqliteDatabase,
        config_center: Option<ConfigCenter>,
        admin_auth: Option<AdminAuth>,
        push_sink: Option<Arc<dyn PushSink>>,
        console_status_source: DynConsoleStatusSource,
        application_version: &'static str,
    ) -> anyhow::Result<Self> {
        let addr: SocketAddr = format!("{}:{}", config.server_host, config.server_port).parse()?;
        let core_state = CoreRuntimeState::from_config_with_database(config, database)?;
        let http_state = OpsHttpState::from_config_with_center(
            &core_state.config,
            core_state.provider.clone(),
            core_state.upstream_status.clone(),
            console_status_source,
            application_version,
            config_center,
            admin_auth,
        );
        let workers = CoreWorkers::from_runtime_state(&core_state, push_sink)?;

        Ok(Self {
            addr,
            core_state,
            http_state,
            workers,
        })
    }

    pub fn core_handle(&self) -> crate::service::CoreHandle {
        crate::service::CoreHandle::new(self.core_state.clone())
    }

    /// 返回 Core HTTP 健康检查 URL。
    ///
    /// 当 bind 地址为通配地址（0.0.0.0 / ::）时，自动映射为本地回环地址，
    /// 避免统一进程入口在存在 HTTP_PROXY 的环境中把探测请求发给代理导致超时。
    pub fn healthz_url(&self) -> String {
        let host = match self.addr.ip().to_string().as_str() {
            "0.0.0.0" => "127.0.0.1".to_string(),
            "::" => "[::1]".to_string(),
            _ => self.addr.ip().to_string(),
        };
        format!("http://{}:{}/healthz", host, self.addr.port())
    }

    pub async fn serve(self) -> anyhow::Result<()> {
        self.serve_with_shutdown(std::future::pending::<()>()).await
    }

    pub fn spawn_schedulers(&self) {
        self.workers.spawn();
    }

    pub async fn serve_with_shutdown<F>(self, shutdown: F) -> anyhow::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Self {
            addr,
            http_state,
            workers,
            ..
        } = self;
        let listener = tokio::net::TcpListener::bind(addr).await?;

        workers.spawn();

        tracing::info!(%addr, "qq-maid-core listening");
        axum::serve(listener, build_router(http_state))
            .with_graceful_shutdown(shutdown)
            .await?;
        Ok(())
    }
}

/// 依次尝试加载当前工作目录下的 `config/.env` 和 `.env` 文件。
/// 本地 make 目标和部署控制脚本都会先切到 `runtime/`，因此默认对应
/// `runtime/config/.env` 和 `runtime/.env`，避免继续读取仓库根配置。
///
/// `dotenvy` 默认不覆盖已经存在的环境变量：进程环境变量优先，
/// 且先加载的 dotenv 文件会保留同名变量，后续文件只补充缺失项。
pub fn load_dotenv_files() {
    dotenvy::from_path("config/.env").ok();
    dotenvy::dotenv().ok();
}

pub fn init_tracing() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_target(false)
                .with_timer(shanghai_log_timer()),
        )
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("qq_maid_core=info,tower_http=info")),
        )
        .try_init()?;
    Ok(())
}

/// 日志时间固定使用上海时区，避免宿主机本地时区影响排障。
fn shanghai_log_timer() -> impl tracing_subscriber::fmt::time::FormatTime {
    fmt::time::OffsetTime::new(
        UtcOffset::from_hms(8, 0, 0).expect("valid Asia/Shanghai UTC offset"),
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
    )
}
