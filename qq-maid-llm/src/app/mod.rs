//! 应用启动模块。负责初始化日志、加载配置、构建各个运行时组件，
//! 并启动 Axum HTTP 服务。

use std::{net::SocketAddr, path::PathBuf};

use time::{UtcOffset, macros::format_description};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::{
    config::AppConfig,
    http::routes::{AppState, build_router},
    provider::{
        build_provider,
        status::{UpstreamStatus, observe_provider},
    },
    runtime::{
        memory::MemoryStore,
        prompt::PromptConfig,
        push::GatewayPushClient,
        query::build_query_executor,
        rss::{RssFetchConfig, RssFetcher, RssScheduler, RssSchedulerConfig, RssStore},
        session::SessionStore,
        todo::TodoStore,
        todo_reminder::{TodoReminderScheduler, TodoReminderSchedulerConfig},
        train::build_train_executor,
        translation::TranslationService,
        weather::build_weather_executor,
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
};

/// 应用入口：加载环境变量、初始化日志、构建配置与运行时、启动 HTTP 服务。
pub async fn run() -> anyhow::Result<()> {
    load_dotenv_files();
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_target(false)
                .with_timer(shanghai_log_timer()),
        )
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("qq_maid_llm=info,tower_http=info")),
        )
        .init();

    let config = AppConfig::from_env()?;
    let addr: SocketAddr = format!("{}:{}", config.server_host, config.server_port).parse()?;
    let upstream_status = UpstreamStatus::default();
    let provider = observe_provider(build_provider(&config)?, upstream_status.clone());
    let translation_service =
        TranslationService::new(provider.clone(), config.translation_model.clone());
    let query_executor = build_query_executor(&config)?;
    let weather_executor = build_weather_executor(&config)?;
    let train_executor = build_train_executor(&config)?;
    // 通用数据库在应用启动阶段统一打开并执行项目级 migration；
    // RSS、Todo、Session 和 Memory 共用同一 SQLite 句柄，避免各业务模块重复打开数据库。
    let database = SqliteDatabase::open(config.app_db_file.clone(), APP_MIGRATIONS)?;
    let session_store = SessionStore::new(database.clone());
    let rss_store = RssStore::new(database.clone());
    let todo_store = TodoStore::new(database.clone());
    let memory_store = MemoryStore::new(database);
    let rss_fetcher = RssFetcher::new(RssFetchConfig {
        timeout_seconds: config.rss_http_timeout_seconds,
        max_body_bytes: config.rss_max_body_bytes as usize,
        user_agent: "qq-maid-rss/0.1 (+https://github.com/kuliantnt/qqbot)".to_owned(),
        allow_private_networks: config.rss_allow_private_urls,
    })?;
    let prompt_config = PromptConfig::new(
        config.prompt_dir.clone(),
        config.member_id_mapping_file.clone(),
    )
    .with_builtin_prompt_defaults(config.prompt_dir_uses_builtin_defaults)
    .with_world_file(config.world_file.clone().map(PathBuf::from));
    let push_client = if config.rss_enabled || config.todo_daily_reminder_enabled {
        Some(GatewayPushClient::new(
            config.rss_push_url.clone(),
            config.rss_push_token.clone(),
            config.rss_http_timeout_seconds,
        )?)
    } else {
        None
    };
    if config.rss_enabled {
        RssScheduler::new(
            rss_store.clone(),
            rss_fetcher.clone(),
            push_client
                .clone()
                .expect("push client must exist when RSS scheduler is enabled"),
            translation_service.clone(),
            RssSchedulerConfig {
                enabled: config.rss_enabled,
                interval_seconds: config.rss_poll_interval_seconds,
                max_push_per_subscription: config.rss_max_push_per_feed as usize,
                summary_max_chars: config.rss_summary_max_chars as usize,
                seen_retention: config.rss_seen_retention as usize,
                push_max_failures: config.rss_push_max_failures as u32,
                push_message_type: config.rss_push_message_type.clone(),
            },
        )
        .spawn();
    }
    if config.todo_daily_reminder_enabled {
        TodoReminderScheduler::new(
            todo_store.clone(),
            push_client
                .clone()
                .expect("push client must exist when Todo reminder is enabled"),
            TodoReminderSchedulerConfig {
                enabled: true,
                reminder_time: config.todo_daily_reminder_time,
            },
        )
        .spawn();
    }
    let state = AppState {
        config,
        provider,
        upstream_status,
        query_executor,
        weather_executor,
        train_executor,
        memory_store,
        session_store,
        todo_store,
        rss_store,
        rss_fetcher,
        prompt_config,
    };
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!(%addr, "qq-maid-llm listening");
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}

/// 依次尝试加载当前工作目录下的 `config/.env` 和 `.env` 文件。
/// 本地 make 目标和部署控制脚本都会先切到 `runtime/`，因此默认对应
/// `runtime/config/.env` 和 `runtime/.env`，避免继续读取仓库根配置。
///
/// `dotenvy` 默认不覆盖已经存在的环境变量：进程环境变量优先，
/// 且先加载的 dotenv 文件会保留同名变量，后续文件只补充缺失项。
fn load_dotenv_files() {
    dotenvy::from_path("config/.env").ok();
    dotenvy::dotenv().ok();
}

/// 日志时间固定使用上海时区，避免宿主机本地时区影响排障。
fn shanghai_log_timer() -> impl tracing_subscriber::fmt::time::FormatTime {
    fmt::time::OffsetTime::new(
        UtcOffset::from_hms(8, 0, 0).expect("valid Asia/Shanghai UTC offset"),
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
    )
}
