//! Core 运行时依赖装配。
//!
//! 本模块只负责构建进程内 CoreService 需要的 provider、executor、store 和提示词等依赖，
//! 不承载 HTTP 路由 state，也不启动后台 worker。

use std::sync::Arc;

use tokio::sync::Semaphore;

use qq_maid_llm::{
    provider::{
        DynLlmProvider, build_provider,
        limiter::{LimitingLlmProvider, LimitingWebSearchExecutor},
        status::{UpstreamStatus, observe_provider},
    },
    web_search::{DynWebSearchExecutor, build_web_search_executor},
};

use crate::{
    config::AppConfig,
    runtime::{
        display_name::DisplayNameStore,
        knowledge::KnowledgeIndex,
        prompt::PromptConfig,
        session::SessionStore,
        tools::{
            DynRadarExecutor, build_radar_executor,
            memory::MemoryStore,
            rss::{RssFetchConfig, RssFetcher, RssStore},
            todo::TodoStore,
            train::{DynTrainExecutor, build_train_executor},
            weather::{DynWeatherExecutor, build_weather_executor},
        },
    },
    storage::notification::NotificationOutboxStore,
    storage::{APP_MIGRATIONS, database::SqliteDatabase, knowledge::KnowledgeStore},
};

/// Core 业务 flow 使用的持久化存储集合。
#[derive(Clone)]
pub struct CoreStores {
    pub memory_store: MemoryStore,
    pub session_store: SessionStore,
    pub todo_store: TodoStore,
    pub notification_store: NotificationOutboxStore,
    pub rss_store: RssStore,
    /// 手动展示名存储，用于本地昵称兜底（#326）。
    pub display_name_store: DisplayNameStore,
}

/// Core 业务 flow 需要的外部执行器集合。
#[derive(Clone)]
pub struct CoreExecutors {
    pub query_executor: DynWebSearchExecutor,
    pub weather_executor: DynWeatherExecutor,
    pub train_executor: DynTrainExecutor,
    pub radar_executor: DynRadarExecutor,
}

/// 进程内 CoreService 的运行时依赖容器。
#[derive(Clone)]
pub struct CoreRuntimeState {
    pub config: AppConfig,
    pub provider: DynLlmProvider,
    /// 最近一次真实上游调用的脱敏状态。
    pub upstream_status: UpstreamStatus,
    pub executors: CoreExecutors,
    pub stores: CoreStores,
    pub rss_fetcher: RssFetcher,
    pub knowledge_index: KnowledgeIndex,
    pub prompt_config: PromptConfig,
}

impl CoreRuntimeState {
    pub fn from_config(config: AppConfig) -> anyhow::Result<Self> {
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
        let query_executor = Arc::new(LimitingWebSearchExecutor::new(
            build_web_search_executor(&config.llm_config())?,
            llm_gate.clone(),
        ));
        let weather_executor = build_weather_executor(&config)?;
        let train_executor = build_train_executor(&config)?;
        let radar_executor = build_radar_executor()?;

        // 通用数据库在应用启动阶段统一打开并执行项目级 migration；
        // SQLite pool size 独立于 LLM / Web Search 上游并发限制。
        let database = SqliteDatabase::open_with_pool_size(
            config.app_db_file.clone(),
            APP_MIGRATIONS,
            config.sqlite_pool_size,
        )?;
        let stores = CoreStores {
            memory_store: MemoryStore::new(database.clone()),
            session_store: SessionStore::new(database.clone()),
            todo_store: TodoStore::new(database.clone()),
            notification_store: NotificationOutboxStore::new(database.clone()),
            rss_store: RssStore::new(database.clone()),
            display_name_store: DisplayNameStore::new(database.clone()),
        };
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

        Ok(Self {
            config,
            provider,
            upstream_status,
            executors: CoreExecutors {
                query_executor,
                weather_executor,
                train_executor,
                radar_executor,
            },
            stores,
            rss_fetcher,
            knowledge_index,
            prompt_config,
        })
    }
}
