//! 应用启动模块。负责初始化日志、加载配置、构建各个运行时组件，
//! 并启动 Axum HTTP 服务。

use std::{future::Future, net::SocketAddr, sync::Arc};

use time::{UtcOffset, macros::format_description};
use tokio::sync::Semaphore;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::{
    config::AppConfig,
    http::routes::{AppState, build_router},
    provider::{
        build_provider,
        limiter::{LimitingLlmProvider, LimitingWebSearchExecutor},
        status::{UpstreamStatus, observe_provider},
    },
    runtime::{
        knowledge::KnowledgeIndex,
        memory::MemoryStore,
        notification::{NotificationWorker, NotificationWorkerConfig},
        prompt::PromptConfig,
        push::PushSink,
        query::build_query_executor,
        rss::{RssFetchConfig, RssFetcher, RssScheduler, RssSchedulerConfig, RssStore},
        session::SessionStore,
        todo::TodoStore,
        todo_reminder::{TodoReminderScheduler, TodoReminderSchedulerConfig},
        tools::build_radar_executor,
        train::build_train_executor,
        translation::TranslationService,
        weather::build_weather_executor,
    },
    storage::knowledge::KnowledgeStore,
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
};

/// 统一进程入口会先组装 Core 运行时，再决定何时开始监听和何时关停。
/// 这样既能把聊天调用交给进程内 CoreService，也能避免双入口重复初始化 dotenv 和 tracing。
pub struct LlmRuntime {
    addr: SocketAddr,
    state: AppState,
    rss_scheduler: Option<RssScheduler>,
    notification_worker: Option<NotificationWorker>,
    todo_reminder_scheduler: Option<TodoReminderScheduler>,
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
        let addr: SocketAddr = format!("{}:{}", config.server_host, config.server_port).parse()?;
        tracing::info!(
            agent_policy = %config.agent_config.diagnostic_summary()?,
            "agent policy loaded"
        );
        let upstream_status = UpstreamStatus::default();
        let llm_gate = (config.max_concurrent_responses > 0)
            .then(|| Arc::new(Semaphore::new(config.max_concurrent_responses as usize)));
        let provider = observe_provider(
            Arc::new(LimitingLlmProvider::new(
                build_provider(&config.llm_config())?,
                llm_gate.clone(),
            )),
            upstream_status.clone(),
        );
        let translation_service =
            TranslationService::new(provider.clone(), config.translation_model.clone());
        let query_executor = Arc::new(LimitingWebSearchExecutor::new(
            build_query_executor(&config)?,
            llm_gate.clone(),
        ));
        let weather_executor = build_weather_executor(&config)?;
        let train_executor = build_train_executor(&config)?;
        let radar_executor = build_radar_executor()?;
        // 通用数据库在应用启动阶段统一打开并执行项目级 migration；
        // RSS、Todo、Session 和 Memory 共用同一 SQLite 句柄，避免各业务模块重复打开数据库。
        let database = SqliteDatabase::open(config.app_db_file.clone(), APP_MIGRATIONS)?;
        let session_store = SessionStore::new(database.clone());
        let rss_store = RssStore::new(database.clone());
        let todo_store = TodoStore::new(database.clone());
        let memory_store = MemoryStore::new(database.clone());
        let notification_store =
            crate::storage::notification::NotificationOutboxStore::new(database.clone());
        let display_name_store =
            crate::runtime::display_name::DisplayNameStore::new(database.clone());
        let knowledge_index =
            KnowledgeIndex::new(KnowledgeStore::new(database), config.knowledge_dir.clone());
        // 知识目录不存在或为空会正常降级；数据库/FTS 错误必须阻止启动，
        // 否则会把索引损坏伪装成“没有知识命中”。
        knowledge_index.sync()?;
        let rss_fetcher = RssFetcher::new(RssFetchConfig {
            timeout_seconds: config.rss_http_timeout_seconds,
            max_body_bytes: config.rss_max_body_bytes as usize,
            user_agent: "qq-maid-rss/0.1 (+https://github.com/kuliantnt/qqbot)".to_owned(),
            allow_private_networks: config.rss_allow_private_urls,
        })?;
        let prompt_config = PromptConfig::new(config.prompt_dir.clone())
            .with_builtin_prompt_defaults(config.prompt_dir_uses_builtin_defaults);
        let push_sink = match (
            push_sink,
            config.rss_enabled || config.todo_daily_reminder_enabled,
        ) {
            (Some(push_sink), _) => Some(push_sink),
            (None, true) => {
                return Err(anyhow::anyhow!(
                    "RSS 或 Todo 每日提醒已启用，但未注入进程内 PushSink"
                ));
            }
            (None, false) => None,
        };
        let notification_worker = push_sink.clone().map(|push_sink| {
            NotificationWorker::new(
                notification_store.clone(),
                push_sink,
                NotificationWorkerConfig::default(),
            )
        });
        let rss_scheduler = if config.rss_enabled {
            Some(RssScheduler::new(
                rss_store.clone(),
                rss_fetcher.clone(),
                notification_store.clone(),
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
            ))
        } else {
            None
        };
        let todo_reminder_scheduler = if config.todo_daily_reminder_enabled {
            Some(TodoReminderScheduler::new(
                todo_store.clone(),
                push_sink
                    .clone()
                    .expect("push sink must exist when Todo reminder is enabled"),
                TodoReminderSchedulerConfig {
                    enabled: true,
                    reminder_time: config.todo_daily_reminder_time,
                },
            ))
        } else {
            None
        };
        let state = AppState {
            config,
            provider,
            upstream_status,
            query_executor,
            weather_executor,
            train_executor,
            radar_executor,
            memory_store,
            session_store,
            todo_store,
            notification_store,
            rss_store,
            rss_fetcher,
            knowledge_index,
            prompt_config,
            display_name_store,
        };

        Ok(Self {
            addr,
            state,
            rss_scheduler,
            notification_worker,
            todo_reminder_scheduler,
        })
    }

    pub fn core_handle(&self) -> crate::service::CoreHandle {
        crate::service::CoreHandle::new(self.state.clone())
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
        if let Some(scheduler) = self.rss_scheduler.clone() {
            scheduler.spawn();
        }
        if let Some(worker) = self.notification_worker.clone() {
            worker.spawn();
        }
        if let Some(scheduler) = self.todo_reminder_scheduler.clone() {
            scheduler.spawn();
        }
    }

    pub async fn serve_with_shutdown<F>(self, shutdown: F) -> anyhow::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Self {
            addr,
            state,
            rss_scheduler,
            notification_worker,
            todo_reminder_scheduler,
        } = self;
        let listener = tokio::net::TcpListener::bind(addr).await?;

        if let Some(scheduler) = rss_scheduler {
            scheduler.spawn();
        }
        if let Some(worker) = notification_worker {
            worker.spawn();
        }
        if let Some(scheduler) = todo_reminder_scheduler {
            scheduler.spawn();
        }

        tracing::info!(%addr, "qq-maid-core listening");
        axum::serve(listener, build_router(state))
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
