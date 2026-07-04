//! 统一程序入口。
//!
//! 该入口一次性完成 dotenv / tracing 初始化，组装 CoreHandle、Gateway 和主动推送
//! sink。Core 与 Gateway 之间只走进程内强类型调用，不再通过 localhost HTTP 探活或通信。

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::anyhow;
use qq_maid_core::{
    app::LlmRuntime as CoreRuntime, config::AppConfig as CoreConfig,
    storage::identity_rebaseline::rebaseline_qq_official_identity,
};
use qq_maid_gateway_rs::{
    config::AppConfig as GatewayConfig, gateway::push::GatewayPushSink, respond::RespondClient,
};
use time::{UtcOffset, macros::format_description};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

const OPS_HTTP_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    qq_maid_core::app::load_dotenv_files();
    init_tracing()?;

    let core_config = CoreConfig::from_env()?;
    let gateway_env = std::env::vars().collect::<HashMap<_, _>>();
    let gateway_config = GatewayConfig::from_map(&gateway_env)?;
    let rebaseline_report =
        rebaseline_qq_official_identity(&core_config.app_db_file, &gateway_config.app_id)?;
    if rebaseline_report.changed() {
        info!(
            sessions = rebaseline_report.sessions,
            session_active = rebaseline_report.session_active,
            memories = rebaseline_report.memories,
            todos = rebaseline_report.todos,
            rss_subscriptions = rebaseline_report.rss_subscriptions,
            rss_duplicates_removed = rebaseline_report.rss_duplicates_removed,
            "已完成旧 QQ 业务归属键归一"
        );
    }

    let push_sink = GatewayPushSink::unbound();
    let core_runtime =
        CoreRuntime::from_config_with_push_sink(core_config, Some(Arc::new(push_sink.clone())))?;
    let core_handle = core_runtime.core_handle();
    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel::<()>();
    let mut core_http_handle = tokio::spawn(async move {
        core_runtime
            .serve_with_shutdown(async move {
                let _ = core_shutdown_rx.await;
            })
            .await
    });
    let respond = RespondClient::new(Arc::new(core_handle));
    info!("Core 已完成进程内初始化，开始启动 Gateway");
    let shutdown_token = CancellationToken::new();
    let gateway_shutdown = shutdown_token.clone();
    let mut gateway_handle = tokio::spawn(async move {
        qq_maid_gateway_rs::app::run_with_config_with_shutdown(
            gateway_config,
            respond,
            push_sink,
            gateway_shutdown,
        )
        .await
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("收到 Ctrl+C，准备停止统一进程");
            shutdown_token.cancel();
            let _ = core_shutdown_tx.send(());
            let _ = tokio::time::timeout(OPS_HTTP_SHUTDOWN_WAIT, &mut gateway_handle).await;
            let _ = tokio::time::timeout(OPS_HTTP_SHUTDOWN_WAIT, &mut core_http_handle).await;
            Ok(())
        }
        result = &mut core_http_handle => {
            shutdown_token.cancel();
            let _ = gateway_handle.await;
            Err(task_exit_error("qq-maid-core-ops-http", result))
        }
        result = &mut gateway_handle => {
            shutdown_token.cancel();
            let _ = core_shutdown_tx.send(());
            let _ = tokio::time::timeout(OPS_HTTP_SHUTDOWN_WAIT, &mut core_http_handle).await;
            Err(task_exit_error("qq-maid-gateway-rs", result))
        }
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
