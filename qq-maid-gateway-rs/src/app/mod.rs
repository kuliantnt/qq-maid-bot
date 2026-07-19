//! 应用启动模块。负责加载环境变量、初始化日志、构建配置，并委托 gateway 主循环运行。

use time::{UtcOffset, macros::format_description};
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    config::AppConfig,
    gateway::{self, ping::GatewayRuntimeStatus, push::GatewayPushSink},
    respond::RespondClient,
};

/// 应用入口：加载本地配置、初始化 tracing，并启动已启用的渠道。
pub async fn run() -> anyhow::Result<()> {
    anyhow::bail!("qq-maid-gateway-rs 不再支持独立 HTTP Core 模式，请通过统一 qq-maid-bot 入口启动")
}

/// 统一进程入口会在完成全局初始化后直接复用这里的 gateway 启动逻辑，
/// 避免把 QQ 接入层初始化细节复制到新的聚合程序里。
pub async fn run_with_config(
    config: AppConfig,
    respond: RespondClient,
    push_sink: GatewayPushSink,
) -> anyhow::Result<()> {
    run_with_config_with_shutdown(config, respond, push_sink, CancellationToken::new()).await
}

/// 统一进程入口会通过 CancellationToken 驱动 Gateway 和 Dispatcher 优雅退出。
pub async fn run_with_config_with_shutdown(
    config: AppConfig,
    respond: RespondClient,
    push_sink: GatewayPushSink,
    shutdown_token: CancellationToken,
) -> anyhow::Result<()> {
    run_with_config_with_shutdown_and_status(
        config,
        respond,
        push_sink,
        GatewayRuntimeStatus::new(),
        shutdown_token,
    )
    .await
}

/// 统一二进制注入共享 Gateway 状态，供 Core 只读控制台复用。
pub async fn run_with_config_with_shutdown_and_status(
    config: AppConfig,
    respond: RespondClient,
    push_sink: GatewayPushSink,
    runtime: GatewayRuntimeStatus,
    shutdown_token: CancellationToken,
) -> anyhow::Result<()> {
    log_startup(&config);
    gateway::run(config, respond, push_sink, runtime, shutdown_token).await
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
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,qq_maid_gateway_rs=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_timer(shanghai_log_timer()))
        .try_init()?;
    Ok(())
}

fn log_startup(config: &AppConfig) {
    info!(
        qq_official_state = ?config.qq_official_binding_state(),
        api_base = %config.api_base,
        sandbox = config.sandbox,
        enable_markdown = config.enable_markdown,
        enable_group_messages = config.enable_group_messages,
        verbose_log = config.verbose_log,
        conversation_queue_capacity = config.conversation_queue_capacity,
        max_active_conversation_workers = config.max_active_conversation_workers,
        conversation_worker_idle_timeout_secs = config.conversation_worker_idle_timeout.as_secs(),
        wechat_service_enabled = config.wechat_service.enabled,
        wechat_service_bind = %format!(
            "{}:{}",
            config.wechat_service.bind_host, config.wechat_service.bind_port
        ),
        wechat_service_callback_path = %config.wechat_service.callback_path,
        "starting qq-maid Rust gateway"
    );
}

/// 日志时间固定使用上海时区，避免部署机器本地时区导致时间线错位。
fn shanghai_log_timer() -> impl tracing_subscriber::fmt::time::FormatTime {
    fmt::time::OffsetTime::new(
        UtcOffset::from_hms(8, 0, 0).expect("valid Asia/Shanghai UTC offset"),
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
    )
}
