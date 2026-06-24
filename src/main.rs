//! 统一程序入口。
//!
//! 该入口只负责一次性完成 dotenv / tracing 初始化，并按“先 Core HTTP、后 Gateway”的顺序
//! 启动现有两个 Rust 模块。Gateway 仍通过本机 HTTP 调用 `/v1/respond`，本次不改业务边界。

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, anyhow};
use qq_maid_core::{app::LlmRuntime as CoreRuntime, config::AppConfig as CoreConfig};
use qq_maid_gateway_rs::config::AppConfig as GatewayConfig;
use time::{UtcOffset, macros::format_description};
use tokio::{sync::oneshot, task::JoinHandle};
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

const CORE_READY_TIMEOUT: Duration = Duration::from_secs(15);
const CORE_READY_RETRY_DELAY: Duration = Duration::from_millis(250);
const CORE_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    qq_maid_core::app::load_dotenv_files();
    init_tracing()?;

    let core_config = CoreConfig::from_env()?;
    let gateway_env = std::env::vars().collect::<HashMap<_, _>>();
    let gateway_config = GatewayConfig::from_map(&gateway_env)?;

    let core_runtime = CoreRuntime::from_config(core_config)?;
    let core_healthz_url = core_runtime.healthz_url();
    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel::<()>();
    let mut core_handle = tokio::spawn(async move {
        core_runtime
            .serve_with_shutdown(async move {
                let _ = core_shutdown_rx.await;
            })
            .await
    });

    if let Err(err) = wait_for_core_ready(&core_healthz_url, &mut core_handle).await {
        let final_err = if core_handle.is_finished() {
            task_exit_error("qq-maid-core", core_handle.await).context(err.to_string())
        } else {
            let _ = core_shutdown_tx.send(());
            let _ = tokio::time::timeout(CORE_SHUTDOWN_WAIT, &mut core_handle).await;
            err
        };
        return Err(final_err);
    }

    info!(healthz = %core_healthz_url, "Core HTTP 已就绪，开始启动 Gateway");
    let mut gateway_handle =
        tokio::spawn(async move { qq_maid_gateway_rs::app::run_with_config(gateway_config).await });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("收到 Ctrl+C，准备停止统一进程");
            shutdown_on_signal(core_shutdown_tx, &mut core_handle, &mut gateway_handle).await;
            Ok(())
        }
        result = &mut core_handle => {
            gateway_handle.abort();
            let _ = gateway_handle.await;
            Err(task_exit_error("qq-maid-core", result))
        }
        result = &mut gateway_handle => {
            let _ = core_shutdown_tx.send(());
            let _ = tokio::time::timeout(CORE_SHUTDOWN_WAIT, &mut core_handle).await;
            Err(task_exit_error("qq-maid-gateway-rs", result))
        }
    }
}

async fn wait_for_core_ready(
    healthz_url: &str,
    core_handle: &mut JoinHandle<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .context("build Core readiness client")?;
    let started_at = tokio::time::Instant::now();

    loop {
        if core_handle.is_finished() {
            return Err(anyhow!("qq-maid-core 在 /healthz 就绪前已经退出"));
        }

        match client.get(healthz_url).send().await {
            Ok(response) if response.status().is_success() => {
                info!(healthz = %healthz_url, "Core /healthz 就绪");
                return Ok(());
            }
            Ok(response) => {
                warn!(
                    healthz = %healthz_url,
                    status = %response.status(),
                    "Core /healthz 尚未就绪"
                );
            }
            Err(err) => {
                warn!(healthz = %healthz_url, error = %err, "等待 Core /healthz 就绪");
            }
        }

        if started_at.elapsed() >= CORE_READY_TIMEOUT {
            return Err(anyhow!(
                "等待 Core /healthz 超时（{} 秒）：{}",
                CORE_READY_TIMEOUT.as_secs(),
                healthz_url
            ));
        }

        tokio::time::sleep(CORE_READY_RETRY_DELAY).await;
    }
}

async fn shutdown_on_signal(
    core_shutdown_tx: oneshot::Sender<()>,
    core_handle: &mut JoinHandle<anyhow::Result<()>>,
    gateway_handle: &mut JoinHandle<anyhow::Result<()>>,
) {
    let _ = core_shutdown_tx.send(());
    gateway_handle.abort();

    if !gateway_handle.is_finished() {
        let _ = gateway_handle.await;
    }
    if !core_handle.is_finished() {
        let _ = tokio::time::timeout(CORE_SHUTDOWN_WAIT, core_handle).await;
    }
}

fn task_exit_error(
    task_name: &str,
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
) -> anyhow::Error {
    match result {
        Ok(Ok(())) => anyhow!("{task_name} 意外退出"),
        Ok(Err(err)) => err.context(format!("{task_name} 运行失败")),
        Err(err) => anyhow!("{task_name} 任务结束异常: {err}"),
    }
}

fn init_tracing() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_target(false)
                .with_timer(shanghai_log_timer()),
        )
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,qq_maid_gateway_rs=debug,qq_maid_core=info,tower_http=info")
        }))
        .try_init()?;
    Ok(())
}

fn shanghai_log_timer() -> impl tracing_subscriber::fmt::time::FormatTime {
    fmt::time::OffsetTime::new(
        UtcOffset::from_hms(8, 0, 0).expect("valid Asia/Shanghai UTC offset"),
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
    )
}
