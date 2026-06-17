//! 应用启动模块。负责加载环境变量、初始化日志、构建配置，并委托 gateway 主循环运行。

use time::{UtcOffset, macros::format_description};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{config::AppConfig, gateway, logging::mask_url};

/// 应用入口：加载本地配置、初始化 tracing，并启动 QQ gateway 主循环。
pub async fn run() -> anyhow::Result<()> {
    load_dotenv_files();
    init_tracing();

    let config = AppConfig::from_env()?;
    info!(
        api_base = %config.api_base,
        respond_url = %mask_url(&config.respond_url),
        sandbox = config.sandbox,
        enable_markdown = config.enable_markdown,
        enable_image = config.enable_image,
        verbose_log = config.verbose_log,
        push_enabled = config.push_enabled,
        push_addr = %format!("{}:{}", config.push_host, config.push_port),
        push_token_configured = config.push_token.is_some(),
        "starting qq-maid Rust C2C gateway"
    );

    gateway::run(config).await
}

/// 依次尝试加载当前工作目录下的 `config/.env` 和 `.env` 文件。
///
/// `dotenvy` 默认不覆盖已经存在的环境变量：进程环境变量优先，
/// 且先加载的 dotenv 文件会保留同名变量，后续文件只补充缺失项。
fn load_dotenv_files() {
    dotenvy::from_path("config/.env").ok();
    dotenvy::dotenv().ok();
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,qq_maid_gateway_rs=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_timer(shanghai_log_timer()))
        .init();
}

/// 日志时间固定使用上海时区，避免部署机器本地时区导致时间线错位。
fn shanghai_log_timer() -> impl tracing_subscriber::fmt::time::FormatTime {
    fmt::time::OffsetTime::new(
        UtcOffset::from_hms(8, 0, 0).expect("valid Asia/Shanghai UTC offset"),
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
    )
}
