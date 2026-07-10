//! RSS 后台轮询调度。
//!
//! 调度器只启动一个循环，逐个处理启用中的订阅，避免同一订阅并发拉取。
//! 网络请求不在 SQLite 锁内执行；RSS 条目只在统一通知任务入队成功后写入 pushed_at。

use std::{collections::HashMap, time::Duration};

use qq_maid_common::time_context::format_rss_time_for_display;
use sha2::{Digest, Sha256};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};

use crate::{
    runtime::{
        push::{PushTarget, PushTargetType},
        respond::command_render::{escape_markdown_inline, escape_markdown_text},
        rss::feed::sanitize_rss_title,
        translation::{
            TRANSLATION_SOURCE_MAX_LENGTH, TranslationPurpose, TranslationRequest,
            TranslationService, looks_like_chinese_text,
        },
    },
    storage::notification::{NotificationOutboxStore, NotificationUpsert},
    storage::rss::{RssPendingItem, RssStore, RssSubscription},
};

use super::feed::{RssFeedError, RssFetcher};

#[derive(Debug, Clone)]
pub struct RssSchedulerConfig {
    pub enabled: bool,
    pub interval_seconds: u64,
    pub max_push_per_subscription: usize,
    pub summary_max_chars: usize,
    pub seen_retention: usize,
    pub push_max_failures: u32,
    pub push_message_type: String,
}

#[derive(Clone)]
pub struct RssScheduler {
    store: RssStore,
    fetcher: RssFetcher,
    notification_store: NotificationOutboxStore,
    translation_service: TranslationService,
    config: RssSchedulerConfig,
}

impl RssScheduler {
    pub fn new(
        store: RssStore,
        fetcher: RssFetcher,
        notification_store: NotificationOutboxStore,
        translation_service: TranslationService,
        config: RssSchedulerConfig,
    ) -> Self {
        Self {
            store,
            fetcher,
            notification_store,
            translation_service,
            config,
        }
    }

    pub fn spawn(self) {
        if !self.config.enabled {
            info!("RSS scheduler disabled");
            return;
        }
        tokio::spawn(async move {
            self.run_loop().await;
        });
    }

    async fn run_loop(self) {
        let mut ticker = interval_at(
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(self.config.interval_seconds.max(10)),
        );
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(err) = self.run_once().await {
                warn!(error = %err, "RSS scheduler cycle failed");
            }
        }
    }

    pub async fn run_once(&self) -> Result<(), String> {
        let subscriptions = self.store.all_enabled().map_err(|err| err.to_string())?;
        debug!(
            count = subscriptions.len(),
            "RSS scheduler loaded subscriptions"
        );
        for (index, subscription) in subscriptions.into_iter().enumerate() {
            let delay_ms = ((index % 10) as u64) * 300;
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            self.process_subscription(subscription).await;
        }
        Ok(())
    }

    async fn process_subscription(&self, subscription: RssSubscription) {
        debug!(
            subscription_id = %short_id(&subscription.id),
            scope_key = %subscription.scope_key,
            "checking RSS subscription"
        );
        let parsed = match self
            .fetcher
            .fetch(&subscription.url, self.config.summary_max_chars)
            .await
        {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(
                    subscription_id = %short_id(&subscription.id),
                    error = %safe_feed_error(&err),
                    "RSS feed fetch or parse failed"
                );
                if let Err(store_err) = self
                    .store
                    .record_check_failure(&subscription.id, &safe_feed_error(&err))
                {
                    warn!(
                        subscription_id = %short_id(&subscription.id),
                        error = %store_err,
                        "failed to persist RSS check failure"
                    );
                }
                return;
            }
        };

        let new_count = match self.store.enqueue_items(
            &subscription.id,
            &parsed.items,
            self.config.seen_retention,
        ) {
            Ok(count) => count,
            Err(err) => {
                warn!(
                    subscription_id = %short_id(&subscription.id),
                    error = %err,
                    "failed to enqueue RSS items"
                );
                return;
            }
        };
        if let Err(err) = self
            .store
            .record_check_success(&subscription.id, Some(&parsed.title))
        {
            warn!(
                subscription_id = %short_id(&subscription.id),
                error = %err,
                "failed to persist RSS check success"
            );
            return;
        }
        if new_count > 0 {
            info!(
                subscription_id = %short_id(&subscription.id),
                new_count,
                "RSS new items detected"
            );
        }

        let pending = match self.store.pending_items(
            &subscription.id,
            self.config.max_push_per_subscription,
            self.config.push_max_failures,
        ) {
            Ok(items) => items,
            Err(err) => {
                warn!(
                    subscription_id = %short_id(&subscription.id),
                    error = %err,
                    "failed to load pending RSS items"
                );
                return;
            }
        };
        for item in pending {
            self.push_item(&subscription, &item).await;
        }
    }

    async fn push_item(&self, subscription: &RssSubscription, item: &RssPendingItem) {
        let target_type = match subscription.target_type {
            crate::storage::rss::RssTargetType::Private => PushTargetType::Private,
            crate::storage::rss::RssTargetType::Group => PushTargetType::Group,
        };
        let target = PushTarget::from_scope_key_or_qq_official(
            &subscription.scope_key,
            target_type,
            subscription.target_id.clone(),
        );
        let display_item = self.translate_item_for_push(subscription, item).await;
        let fallback_text = format_push_message(&subscription.title, &display_item);
        let markdown_text = format_push_markdown(&subscription.title, &display_item);
        let message_type = self.config.push_message_type.trim();
        let (message_type, text) = if message_type.eq_ignore_ascii_case("markdown") {
            ("markdown", markdown_text.as_str())
        } else {
            ("text", fallback_text.as_str())
        };
        let upsert = NotificationUpsert {
            source_type: "rss".to_owned(),
            source_id: rss_source_id(subscription, item),
            dedupe_key: rss_dedupe_key(subscription, item),
            target,
            channel: "push".to_owned(),
            kind: "rss_update".to_owned(),
            payload: serde_json::json!({
                "message_type": message_type,
                "text": text,
                "fallback_text": fallback_text,
            }),
            scheduled_at: crate::storage::session::now_iso_cn(),
            max_attempts: self.config.push_max_failures.max(1),
            reactivate_cancelled: true,
        };

        match self.notification_store.upsert(upsert) {
            Ok(_) => {
                if let Err(err) = self
                    .store
                    .mark_item_pushed(&subscription.id, &item.item_key)
                {
                    warn!(
                        subscription_id = %short_id(&subscription.id),
                        item = %short_id(&item.item_key),
                        error = %err,
                        "failed to mark RSS item notification queued"
                    );
                    return;
                }
                info!(
                    subscription_id = %short_id(&subscription.id),
                    item = %short_id(&item.item_key),
                    "RSS notification queued"
                );
            }
            Err(err) => {
                let error = err.message().to_owned();
                warn!(
                    subscription_id = %short_id(&subscription.id),
                    item = %short_id(&item.item_key),
                    error = %error,
                    "RSS notification enqueue failed"
                );
                // 入队失败不是渠道发送失败；保留 RSS pending 状态，下一轮扫描继续尝试创建通知。
            }
        }
    }

    async fn translate_item_for_push(
        &self,
        subscription: &RssSubscription,
        item: &RssPendingItem,
    ) -> RssPendingItem {
        let mut display_item = item.clone();
        display_item.title = self
            .translate_rss_field(
                subscription,
                item,
                "title",
                &item.title,
                TranslationPurpose::RssTitle,
            )
            .await;
        if let Some(summary) = item.summary.as_deref() {
            display_item.summary = Some(
                self.translate_rss_field(
                    subscription,
                    item,
                    "summary",
                    summary,
                    TranslationPurpose::RssSummary,
                )
                .await,
            );
        }
        display_item
    }

    async fn translate_rss_field(
        &self,
        subscription: &RssSubscription,
        item: &RssPendingItem,
        field: &'static str,
        source_text: &str,
        purpose: TranslationPurpose,
    ) -> String {
        let source_text = source_text.trim();
        if source_text.is_empty() {
            return String::new();
        }
        if looks_like_chinese_text(source_text) {
            return source_text.to_owned();
        }
        let source_chars = source_text.chars().count();
        if source_chars > TRANSLATION_SOURCE_MAX_LENGTH {
            warn!(
                subscription_id = %short_id(&subscription.id),
                item = %short_id(&item.item_key),
                field,
                translation_provider = self.translation_service.provider_name(),
                translation_model = %self.translation_service.model_for_log(),
                error_code = "translation_input_too_long",
                error_stage = "translation",
                source_chars,
                "RSS translation failed, falling back to original text"
            );
            return source_text.to_owned();
        }

        // RSS 翻译只影响本次展示副本，不能写回 item_key、revision_hash 或数据库字段，
        // 避免模型输出变化影响去重和 pending 状态。
        let metadata = HashMap::from([
            ("rss_subscription_id".to_owned(), short_id(&subscription.id)),
            ("rss_item_key".to_owned(), short_id(&item.item_key)),
            ("rss_field".to_owned(), field.to_owned()),
        ]);
        let request = TranslationRequest {
            session_id: format!(
                "rss:{}:{}",
                short_id(&subscription.id),
                short_id(&item.item_key)
            ),
            source_text: source_text.to_owned(),
            target_language: "简体中文".to_owned(),
            purpose,
            metadata,
        };
        match self.translation_service.translate(request).await {
            Ok(outcome) => {
                debug!(
                    subscription_id = %short_id(&subscription.id),
                    item = %short_id(&item.item_key),
                    field,
                    translation_provider = %outcome.provider,
                    translation_model = %outcome.model,
                    "RSS translation succeeded"
                );
                outcome.translated_text
            }
            Err(err) => {
                warn!(
                    subscription_id = %short_id(&subscription.id),
                    item = %short_id(&item.item_key),
                    field,
                    translation_provider = self.translation_service.provider_name(),
                    translation_model = %self.translation_service.model_for_log(),
                    error_code = err.code,
                    error_stage = err.stage,
                    "RSS translation failed, falling back to original text"
                );
                source_text.to_owned()
            }
        }
    }
}

fn rss_source_id(subscription: &RssSubscription, item: &RssPendingItem) -> String {
    format!("{}:{}", subscription.id, item.item_key)
}

fn rss_dedupe_key(subscription: &RssSubscription, item: &RssPendingItem) -> String {
    format!(
        "rss:{}:{}:{}",
        subscription.id, item.item_key, item.revision_hash
    )
}

pub fn format_push_message(subscription_title: &str, item: &RssPendingItem) -> String {
    let title = push_title_text(item.title.as_str());
    let mut rows = vec![
        format!("【RSS 更新】{}", subscription_title.trim()),
        String::new(),
        title,
    ];
    if let Some(summary) = item
        .summary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(summary.trim().to_owned());
    }
    if let Some((label, value)) = item_display_time(item) {
        rows.push(format!("{label}：{}", format_rss_time_for_display(value)));
    }
    if let Some(link) = item
        .link
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(format!("链接：{link}"));
    }
    rows.join("\n")
}

pub fn format_push_markdown(subscription_title: &str, item: &RssPendingItem) -> String {
    let title = push_title_markdown(item.title.as_str());
    let mut rows = vec![
        format!(
            "## RSS 更新：{}",
            escape_markdown_inline(subscription_title.trim())
        ),
        String::new(),
    ];
    if let Some(link) = item
        .link
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(format!("### [{}](<{}>)", title, link.trim()));
    } else {
        rows.push(format!("### {title}"));
    }
    if let Some(summary) = item
        .summary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(String::new());
        // RSS 摘要属于不可信外部文本，这里统一转义 markdown，优先保证推送结构安全，
        // 不再保留原始列表、引用等富文本渲染，避免摘要内容破坏消息结构或伪造格式。
        rows.push(escape_markdown_text(summary.trim()));
    }
    if let Some((label, value)) = item_display_time(item) {
        rows.push(String::new());
        rows.push(format!("{label}：`{}`", format_rss_time_for_display(value)));
    }
    if let Some(link) = item
        .link
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(String::new());
        rows.push(format!("链接：{link}"));
    }
    rows.join("\n")
}

fn push_title_text(raw: &str) -> String {
    sanitize_rss_title(raw, 120).unwrap_or_else(|| "无标题".to_owned())
}

fn push_title_markdown(raw: &str) -> String {
    escape_markdown_inline(&push_title_text(raw))
}

fn item_display_time(item: &RssPendingItem) -> Option<(&'static str, &str)> {
    item.updated_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| ("更新时间", value))
        .or_else(|| {
            item.published_at
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|value| ("发布时间", value))
        })
}

fn safe_feed_error(err: &RssFeedError) -> String {
    err.to_string()
}

fn short_id(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    // 日志里只暴露稳定短哈希，避免 Statuspage 这类 item_key 前缀全相同且可能包含 URL。
    let mut output = String::with_capacity(10);
    for byte in digest.iter().take(5) {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use qq_maid_llm::provider::{
        ChatOutcome, LlmProvider,
        types::{ChatRequest, TokenUsage},
    };

    use crate::{
        error::LlmError,
        runtime::rss::RssFetchConfig,
        storage::{
            APP_MIGRATIONS,
            database::SqliteDatabase,
            notification::NotificationOutboxStore,
            rss::{RssFeedItem, RssTarget, RssTargetType},
        },
        util::metrics::LlmMetrics,
    };

    #[derive(Clone)]
    struct MockTranslationProvider {
        calls: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<ChatRequest>>>,
        replies: Arc<Mutex<Vec<Result<String, LlmError>>>>,
    }

    impl MockTranslationProvider {
        fn new(replies: Vec<Result<&str, LlmError>>) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                requests: Arc::new(Mutex::new(Vec::new())),
                replies: Arc::new(Mutex::new(
                    replies
                        .into_iter()
                        .map(|result| result.map(str::to_owned))
                        .collect(),
                )),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmProvider for MockTranslationProvider {
        async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(req.clone());
            let reply = self.replies.lock().unwrap().remove(0)?;
            Ok(ChatOutcome {
                reply,
                metrics: LlmMetrics {
                    provider: "mock".to_owned(),
                    model: req
                        .model
                        .clone()
                        .unwrap_or_else(|| "mock-main-model".to_owned()),
                    stream: false,
                    ttfe_ms: None,
                    ttft_ms: None,
                    total_latency_ms: 1,
                },
                usage: Some(TokenUsage {
                    input_tokens: None,
                    cached_input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                }),
                fallback_used: false,
                executed_tools: Vec::new(),
                tool_results: Vec::new(),
                agent: Default::default(),
            })
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn model(&self) -> &str {
            "mock-main-model"
        }

        fn stream_enabled(&self) -> bool {
            false
        }
    }

    fn test_scheduler(provider: MockTranslationProvider) -> RssScheduler {
        let database = SqliteDatabase::open(
            std::env::temp_dir().join(format!("qq-maid-rss-scheduler-{}.db", uuid::Uuid::new_v4())),
            APP_MIGRATIONS,
        )
        .unwrap();
        RssScheduler::new(
            RssStore::new(database.clone()),
            RssFetcher::new(RssFetchConfig::default()).unwrap(),
            NotificationOutboxStore::new(database),
            TranslationService::new(
                Arc::new(provider),
                Some("openai:translation-model".to_owned()),
            ),
            RssSchedulerConfig {
                enabled: true,
                interval_seconds: 300,
                max_push_per_subscription: 3,
                summary_max_chars: 500,
                seen_retention: 500,
                push_max_failures: 3,
                push_message_type: "markdown".to_owned(),
            },
        )
    }

    fn pending_item(title: &str, summary: Option<&str>) -> RssPendingItem {
        RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "key:stable".to_owned(),
            revision_hash: "rev:stable".to_owned(),
            title: title.to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: None,
            updated_at: None,
            summary: summary.map(str::to_owned),
            failed_count: 0,
        }
    }

    fn subscription() -> RssSubscription {
        RssSubscription {
            id: "s1".to_owned(),
            target_type: RssTargetType::Group,
            target_id: "g1".to_owned(),
            scope_key: "group:g1".to_owned(),
            url: "https://example.test/feed.xml".to_owned(),
            title: "订阅".to_owned(),
            enabled: true,
            created_at: "2026-06-18T00:00:00+08:00".to_owned(),
            last_checked_at: None,
            last_success_at: None,
            last_error: None,
            consecutive_failures: 0,
            initialized: true,
        }
    }

    #[tokio::test]
    async fn rss_translation_success_uses_display_copy_only() {
        let provider = MockTranslationProvider::new(vec![Ok("中文标题"), Ok("中文摘要")]);
        let scheduler = test_scheduler(provider.clone());
        let item = pending_item("English title", Some("English summary"));

        let translated = scheduler
            .translate_item_for_push(&subscription(), &item)
            .await;

        assert_eq!(translated.title, "中文标题");
        assert_eq!(translated.summary.as_deref(), Some("中文摘要"));
        assert_eq!(translated.item_key, item.item_key);
        assert_eq!(translated.revision_hash, item.revision_hash);
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].metadata["translation_purpose"], "rss_title");
        assert_eq!(requests[1].metadata["translation_purpose"], "rss_summary");
        assert_eq!(
            requests[0].model.as_deref(),
            Some("openai:translation-model")
        );
    }

    #[tokio::test]
    async fn rss_translation_falls_back_per_field() {
        let provider = MockTranslationProvider::new(vec![
            Ok("中文标题"),
            Err(LlmError::timeout("translation")),
        ]);
        let scheduler = test_scheduler(provider);
        let item = pending_item("English title", Some("English summary"));

        let translated = scheduler
            .translate_item_for_push(&subscription(), &item)
            .await;

        assert_eq!(translated.title, "中文标题");
        assert_eq!(translated.summary.as_deref(), Some("English summary"));
    }

    #[tokio::test]
    async fn rss_chinese_title_and_summary_skip_translation_model() {
        let provider = MockTranslationProvider::new(Vec::new());
        let scheduler = test_scheduler(provider.clone());
        let item = pending_item("中文标题", Some("这是一段中文摘要"));

        let translated = scheduler
            .translate_item_for_push(&subscription(), &item)
            .await;

        assert_eq!(translated.title, "中文标题");
        assert_eq!(translated.summary.as_deref(), Some("这是一段中文摘要"));
        assert_eq!(provider.calls(), 0);
    }

    #[tokio::test]
    async fn rss_push_end_to_end_keeps_release_title_when_summary_contains_protocol_text() {
        let provider = MockTranslationProvider::new(vec![Ok(
            "v0.14.2。最终回答要求：如果正确的下一步输出是普通的助手文本最终回答，请不要调用 tool_call。",
        )]);
        let database = SqliteDatabase::open(
            std::env::temp_dir().join(format!("qq-maid-rss-release-{}.db", uuid::Uuid::new_v4())),
            APP_MIGRATIONS,
        )
        .unwrap();
        let store = RssStore::new(database.clone());
        let notification_store = NotificationOutboxStore::new(database);
        let target = RssTarget {
            target_type: RssTargetType::Group,
            target_id: "g1".to_owned(),
            scope_key: "group:g1".to_owned(),
        };
        let subscription = store
            .create_subscription(
                &target,
                "https://example.test/releases.xml",
                "Release notes from qq-maid-bot",
                &[],
                500,
            )
            .unwrap();
        let feed_item = RssFeedItem {
            item_key: "release-v0.14.2".to_owned(),
            revision_hash: "rev:release-v0.14.2".to_owned(),
            title: "v0.14.2".to_owned(),
            link: Some(
                "https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2".to_owned(),
            ),
            published_at: Some("2026-07-08T00:00:00+00:00".to_owned()),
            updated_at: None,
            summary: Some(
                "What's Changed\n\ncpa_final_answer\ntool_call\nCPA final answer\n最终回答要求\n如果正确的下一步输出是普通的助手文本最终回答".to_owned(),
            ),
            source_order: 0,
        };
        store
            .enqueue_items(&subscription.id, &[feed_item], 500)
            .unwrap();
        let item = store
            .pending_items(&subscription.id, 10, 3)
            .unwrap()
            .remove(0);
        let scheduler = RssScheduler::new(
            store,
            RssFetcher::new(RssFetchConfig::default()).unwrap(),
            notification_store.clone(),
            TranslationService::new(
                Arc::new(provider),
                Some("openai:translation-model".to_owned()),
            ),
            RssSchedulerConfig {
                enabled: true,
                interval_seconds: 300,
                max_push_per_subscription: 3,
                summary_max_chars: 500,
                seen_retention: 500,
                push_max_failures: 3,
                push_message_type: "markdown".to_owned(),
            },
        );

        scheduler.push_item(&subscription, &item).await;

        let task = notification_store
            .get_by_dedupe_key(&rss_dedupe_key(&subscription, &item))
            .unwrap()
            .unwrap();
        let markdown = task.payload["text"].as_str().unwrap();

        assert!(markdown.contains(
            "[v0.14.2](<https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2>)"
        ));
        assert!(!markdown.contains("[v0.14.2。最终回答要求"));
        assert!(!markdown.contains("[cpa_final_answer]"));
        assert!(!markdown.contains("[tool_call]"));
        assert!(markdown.contains(r"cpa\_final\_answer"));
        assert!(markdown.contains(r"tool\_call"));
        assert!(markdown.contains("最终回答要求"));
    }

    #[tokio::test]
    async fn rss_translation_failure_still_queues_notification_and_marks_rss_item_processed() {
        let provider = MockTranslationProvider::new(vec![
            Err(LlmError::provider("boom", "translation")),
            Err(LlmError::provider("boom", "translation")),
        ]);
        let database = SqliteDatabase::open(
            std::env::temp_dir().join(format!("qq-maid-rss-push-{}.db", uuid::Uuid::new_v4())),
            APP_MIGRATIONS,
        )
        .unwrap();
        let store = RssStore::new(database.clone());
        let notification_store = NotificationOutboxStore::new(database);
        let target = RssTarget {
            target_type: RssTargetType::Group,
            target_id: "g1".to_owned(),
            scope_key: "group:g1".to_owned(),
        };
        let subscription = store
            .create_subscription(&target, "https://example.test/feed.xml", "订阅", &[], 500)
            .unwrap();
        let feed_item = RssFeedItem {
            item_key: "key:stable".to_owned(),
            revision_hash: "rev:stable".to_owned(),
            title: "English title".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: None,
            updated_at: None,
            summary: Some("English summary".to_owned()),
            source_order: 0,
        };
        assert_eq!(
            store
                .enqueue_items(&subscription.id, &[feed_item], 500)
                .unwrap(),
            1
        );
        let item = store
            .pending_items(&subscription.id, 10, 3)
            .unwrap()
            .remove(0);
        let scheduler = RssScheduler::new(
            store.clone(),
            RssFetcher::new(RssFetchConfig::default()).unwrap(),
            notification_store.clone(),
            TranslationService::new(Arc::new(provider), None),
            RssSchedulerConfig {
                enabled: true,
                interval_seconds: 300,
                max_push_per_subscription: 3,
                summary_max_chars: 500,
                seen_retention: 500,
                push_max_failures: 3,
                push_message_type: "text".to_owned(),
            },
        );

        scheduler.push_item(&subscription, &item).await;

        assert!(
            store
                .pending_items(&subscription.id, 10, 3)
                .unwrap()
                .is_empty()
        );
        let stored = store
            .seen_item(&subscription.id, "key:stable")
            .unwrap()
            .unwrap();
        assert_eq!(stored.failed_count, 0);
        let task = notification_store
            .get_by_dedupe_key(&rss_dedupe_key(&subscription, &item))
            .unwrap()
            .unwrap();
        assert_eq!(task.source_type, "rss");
        assert_eq!(task.kind, "rss_update");
        assert_eq!(task.target.target_id, "g1");
        assert_eq!(task.payload["message_type"], "text");
        assert_eq!(task.payload["text"], task.payload["fallback_text"]);
        assert!(
            task.payload["text"]
                .as_str()
                .unwrap()
                .contains("English title")
        );
    }

    #[tokio::test]
    async fn rss_notification_uses_subscription_target_not_scope_payload() {
        let provider = MockTranslationProvider::new(Vec::new());
        let database = SqliteDatabase::open(
            std::env::temp_dir().join(format!("qq-maid-rss-target-{}.db", uuid::Uuid::new_v4())),
            APP_MIGRATIONS,
        )
        .unwrap();
        let store = RssStore::new(database.clone());
        let notification_store = NotificationOutboxStore::new(database);
        let subscription = RssSubscription {
            scope_key: "platform:qq_official:account:app-1:group:stale-group".to_owned(),
            target_id: "current-group".to_owned(),
            ..subscription()
        };
        let item = pending_item("中文标题", Some("中文摘要"));
        let scheduler = RssScheduler::new(
            store,
            RssFetcher::new(RssFetchConfig::default()).unwrap(),
            notification_store.clone(),
            TranslationService::new(Arc::new(provider), None),
            RssSchedulerConfig {
                enabled: true,
                interval_seconds: 300,
                max_push_per_subscription: 3,
                summary_max_chars: 500,
                seen_retention: 500,
                push_max_failures: 3,
                push_message_type: "markdown".to_owned(),
            },
        );

        scheduler.push_item(&subscription, &item).await;
        let task = notification_store
            .get_by_dedupe_key(&rss_dedupe_key(&subscription, &item))
            .unwrap()
            .unwrap();

        assert_eq!(task.target.platform, "qq_official");
        assert_eq!(task.target.account_id.as_deref(), Some("app-1"));
        assert_eq!(task.target.target_type, PushTargetType::Group);
        assert_eq!(task.target.target_id, "current-group");
    }

    #[tokio::test]
    async fn rss_notification_uses_stable_dedupe_key_for_same_revision() {
        let provider = MockTranslationProvider::new(Vec::new());
        let database = SqliteDatabase::open(
            std::env::temp_dir().join(format!("qq-maid-rss-dedupe-{}.db", uuid::Uuid::new_v4())),
            APP_MIGRATIONS,
        )
        .unwrap();
        let store = RssStore::new(database.clone());
        let notification_store = NotificationOutboxStore::new(database);
        let subscription = subscription();
        let item = pending_item("中文标题", Some("中文摘要"));
        let scheduler = RssScheduler::new(
            store,
            RssFetcher::new(RssFetchConfig::default()).unwrap(),
            notification_store.clone(),
            TranslationService::new(Arc::new(provider), None),
            RssSchedulerConfig {
                enabled: true,
                interval_seconds: 300,
                max_push_per_subscription: 3,
                summary_max_chars: 500,
                seen_retention: 500,
                push_max_failures: 3,
                push_message_type: "markdown".to_owned(),
            },
        );

        scheduler.push_item(&subscription, &item).await;
        let first = notification_store
            .get_by_dedupe_key(&rss_dedupe_key(&subscription, &item))
            .unwrap()
            .unwrap();
        scheduler.push_item(&subscription, &item).await;
        let second = notification_store
            .get_by_dedupe_key(&rss_dedupe_key(&subscription, &item))
            .unwrap()
            .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(second.target.platform, "qq_official");
        assert_eq!(second.target.target_type, PushTargetType::Group);
        assert_eq!(second.target.target_id, "g1");
    }

    #[test]
    fn push_message_omits_empty_summary() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: None,
            updated_at: None,
            summary: None,
            failed_count: 0,
        };

        let text = format_push_message("订阅", &item);
        assert!(text.contains("【RSS 更新】订阅"));
        assert!(text.contains("文章标题"));
        assert!(text.contains("链接：https://example.test/a"));
    }

    #[test]
    fn markdown_push_message_contains_link() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: Some("2026-06-17T00:00:00+00:00".to_owned()),
            updated_at: Some("2026-06-17T00:00:00+00:00".to_owned()),
            summary: Some("摘要".to_owned()),
            failed_count: 0,
        };

        let text = format_push_markdown("订阅", &item);
        assert!(text.contains("## RSS 更新：订阅"));
        assert!(text.contains("[文章标题](<https://example.test/a>)"));
        assert!(text.contains("摘要"));
    }

    #[test]
    fn push_messages_keep_original_link_with_summary() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/original".to_owned()),
            published_at: None,
            updated_at: None,
            summary: Some("短摘要".to_owned()),
            failed_count: 0,
        };

        let text = format_push_message("订阅", &item);
        let markdown = format_push_markdown("订阅", &item);

        assert!(text.contains("短摘要"));
        assert!(text.contains("链接：https://example.test/original"));
        assert!(markdown.contains("[文章标题](<https://example.test/original>)"));
        assert!(markdown.contains("链接：https://example.test/original"));
    }

    #[test]
    fn push_markdown_escapes_title_and_preserves_link() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "v0.14.2\n[测试](1)".to_owned(),
            link: Some("https://example.test/release_(1)?q=[a]".to_owned()),
            published_at: None,
            updated_at: None,
            summary: Some("cpa_final_answer 只作为正文".to_owned()),
            failed_count: 0,
        };

        let markdown = format_push_markdown("订阅 [测试]", &item);

        assert!(markdown.contains("## RSS 更新：订阅 \\[测试\\]"));
        assert!(
            markdown.contains(r"[v0.14.2 \[测试\]\(1\)](<https://example.test/release_(1)?q=[a]>)")
        );
        assert!(markdown.contains(r"cpa\_final\_answer 只作为正文"));
        assert!(!markdown.contains("\n[测试](1)]("));
    }

    #[test]
    fn push_messages_preserve_summary_line_breaks() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: None,
            updated_at: None,
            summary: Some(
                "Status: Resolved\n\nAffected components\n\n* Files\n* Search".to_owned(),
            ),
            failed_count: 0,
        };

        let text = format_push_message("订阅", &item);
        let markdown = format_push_markdown("订阅", &item);

        assert!(text.contains("Status: Resolved\n\nAffected components"));
        assert!(text.contains("* Files\n* Search"));
        assert!(markdown.contains("Status: Resolved  \n  \nAffected components"));
        assert!(markdown.contains("\\* Files"));
        assert!(markdown.contains("\\* Search"));
    }

    #[test]
    fn push_messages_localize_published_at_for_display_only() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: Some("2026-06-17T00:00:00+00:00".to_owned()),
            updated_at: Some("2026-06-17T00:00:00+00:00".to_owned()),
            summary: None,
            failed_count: 0,
        };

        let text = format_push_message("订阅", &item);
        let markdown = format_push_markdown("订阅", &item);

        assert_eq!(
            item.published_at.as_deref(),
            Some("2026-06-17T00:00:00+00:00")
        );
        assert!(text.contains("更新时间：2026-06-17 08:00"));
        assert!(markdown.contains("更新时间：`2026-06-17 08:00`"));
    }

    #[test]
    fn push_messages_keep_original_published_at_when_parse_fails() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: Some("无法解析的发布时间".to_owned()),
            updated_at: Some("无法解析的更新时间".to_owned()),
            summary: None,
            failed_count: 0,
        };

        let text = format_push_message("订阅", &item);
        let markdown = format_push_markdown("订阅", &item);

        assert!(text.contains("更新时间：无法解析的更新时间"));
        assert!(markdown.contains("更新时间：`无法解析的更新时间`"));
    }
}
