//! Core 后台 worker 和 scheduler 装配。
//!
//! 这里只根据已经构建好的 CoreRuntimeState 创建后台任务对象；真正 spawn 仍由 LlmRuntime 控制，
//! 以保持统一入口的启动和 shutdown 顺序稳定。

use std::sync::Arc;

use crate::runtime::{
    notification::{NotificationWorker, NotificationWorkerConfig},
    push::PushSink,
    tools::memory::{MemoryConsolidationConfig, MemoryConsolidationWorker},
    tools::rss::{RssScheduler, RssSchedulerConfig},
    tools::{TodoReminderScheduler, TodoReminderSchedulerConfig, TodoReminderSentHook},
    translation::TranslationService,
};

use super::runtime::CoreRuntimeState;

#[derive(Clone)]
pub struct CoreWorkers {
    pub memory_consolidation_worker: Option<MemoryConsolidationWorker>,
    pub rss_scheduler: Option<RssScheduler>,
    pub notification_worker: Option<NotificationWorker>,
    pub todo_reminder_scheduler: Option<TodoReminderScheduler>,
}

impl CoreWorkers {
    pub fn from_runtime_state(
        state: &CoreRuntimeState,
        push_sink: Option<Arc<dyn PushSink>>,
    ) -> anyhow::Result<Self> {
        let config = &state.config;
        let memory_consolidation_worker = config.memory_consolidation_enabled.then(|| {
            MemoryConsolidationWorker::new(
                state.stores.memory_store.clone(),
                MemoryConsolidationConfig {
                    enabled: true,
                    check_interval_seconds: config.memory_consolidation_check_interval_seconds,
                    min_interval_seconds: config.memory_consolidation_min_interval_seconds,
                    min_new_records: config.memory_consolidation_min_new_records as usize,
                    min_distinct_sources: config.memory_consolidation_min_distinct_sources as usize,
                    max_records: config.memory_consolidation_max_records as usize,
                    max_input_chars: config.memory_consolidation_max_input_chars as usize,
                },
            )
        });
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
                state.stores.notification_store.clone(),
                push_sink,
                NotificationWorkerConfig::default(),
            )
            .with_command_prefix(config.command_prefix)
            .with_after_sent_hook(Arc::new(TodoReminderSentHook::new(
                state.stores.todo_store.clone(),
                state.stores.notification_store.clone(),
            )))
        });
        let translation_service = TranslationService::new(state.provider.clone(), None)
            .with_agent_config(config.agent_config.clone());
        let rss_scheduler = if config.rss_enabled {
            Some(RssScheduler::new(
                state.stores.rss_store.clone(),
                state.rss_fetcher.clone(),
                state.stores.notification_store.clone(),
                translation_service,
                RssSchedulerConfig {
                    enabled: config.rss_enabled,
                    translation_enabled: config.rss_translation_enabled,
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
                state.stores.todo_store.clone(),
                state.stores.notification_store.clone(),
                TodoReminderSchedulerConfig {
                    enabled: true,
                    reminder_time: config.todo_daily_reminder_time,
                    command_prefix: config.command_prefix,
                },
            ))
        } else {
            None
        };

        Ok(Self {
            memory_consolidation_worker,
            rss_scheduler,
            notification_worker,
            todo_reminder_scheduler,
        })
    }

    pub fn spawn(&self) {
        if let Some(worker) = self.memory_consolidation_worker.clone() {
            worker.spawn();
        }
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
}
