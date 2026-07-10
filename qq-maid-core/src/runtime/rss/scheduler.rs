//! RSS 后台轮询调度。
//!
//! 调度器只启动一个循环，逐个处理启用中的订阅，避免同一订阅并发拉取。
//! 网络请求不在 SQLite 锁内执行；RSS 条目只在统一通知任务入队成功后写入 pushed_at。

use std::{collections::HashMap, time::Duration};

use pulldown_cmark::{Event, Options, Parser, Tag};
use qq_maid_common::{
    markdown_strip::{
        render_markdown_as_plain_text, render_markdown_for_qq, render_markdown_for_qq_with_limit,
    },
    time_context::format_rss_time_for_display,
};
use sha2::{Digest, Sha256};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};

use crate::{
    runtime::{
        push::{PushTarget, PushTargetType},
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
            let translated = self
                .translate_rss_field(
                    subscription,
                    item,
                    "summary",
                    summary,
                    TranslationPurpose::RssSummary,
                )
                .await;
            // 翻译模型只能改可见文本，不能改写、删除或新增链接目标。链接不一致时
            // 回退原摘要；无论是否翻译成功，最终都重新解析并按 QQ 子集安全渲染。
            let source = if markdown_http_links(summary) == markdown_http_links(&translated) {
                translated.as_str()
            } else {
                warn!(
                    subscription_id = %short_id(&subscription.id),
                    item = %short_id(&item.item_key),
                    field = "summary",
                    error_code = "translation_links_changed",
                    error_stage = "translation",
                    "RSS translation changed Markdown links, falling back to original text"
                );
                summary
            };
            display_item.summary = Some(render_markdown_for_qq_with_limit(
                source,
                self.config.summary_max_chars,
            ));
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
        format!(
            "【RSS 更新】{}",
            push_subscription_title(subscription_title)
        ),
        String::new(),
        title,
    ];
    if let Some(summary) = item
        .summary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let summary = render_markdown_as_plain_text(summary.trim());
        if !summary.is_empty() {
            rows.push(summary);
        }
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
    let title = markdown_inline_text(&push_title_text(item.title.as_str()));
    let subscription_title = markdown_inline_text(&push_subscription_title(subscription_title));
    let link = item.link.as_deref().and_then(http_markdown_link);
    let mut rows = vec![
        format!("## RSS 更新：{subscription_title}"),
        String::new(),
        match link.as_deref() {
            Some(link) => format!("### [{title}](<{link}>)"),
            None => format!("### {title}"),
        },
    ];
    if let Some(summary) = item
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let summary = render_markdown_for_qq(summary);
        if !summary.is_empty() {
            rows.push(String::new());
            rows.push(summary);
        }
    }
    if let Some((label, value)) = item_display_time(item) {
        rows.push(String::new());
        rows.push(format!("{label}：{}", format_rss_time_for_display(value)));
    }
    if let Some(link) = link {
        rows.push(String::new());
        rows.push(format!("原文：[查看条目](<{link}>)"));
    }
    rows.join("\n")
}

fn push_title_text(raw: &str) -> String {
    sanitize_rss_title(raw, 120).unwrap_or_else(|| "无标题".to_owned())
}

fn push_subscription_title(raw: &str) -> String {
    sanitize_rss_title(raw, 120).unwrap_or_else(|| "未命名订阅".to_owned())
}

fn markdown_inline_text(raw: &str) -> String {
    raw.chars()
        .map(|ch| match ch {
            '`' => '｀',
            '*' => '＊',
            '_' => '＿',
            '[' => '［',
            ']' => '］',
            '(' => '（',
            ')' => '）',
            '<' => '＜',
            '>' => '＞',
            '|' => '｜',
            _ => ch,
        })
        .collect()
}

fn http_markdown_link(raw: &str) -> Option<String> {
    let link = raw.trim();
    let lower = link.to_ascii_lowercase();
    (!link.is_empty() && (lower.starts_with("https://") || lower.starts_with("http://")))
        .then(|| link.replace(['\n', '\r', '<', '>'], ""))
}

fn markdown_http_links(markdown: &str) -> Vec<String> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    Parser::new_ext(markdown, options)
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                http_markdown_link(&dest_url)
            }
            _ => None,
        })
        .collect()
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
                push_message_type: crate::config::DEFAULT_RSS_PUSH_MESSAGE_TYPE.to_owned(),
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
    async fn rss_translation_rerenders_release_markdown_and_preserves_links() {
        let provider = MockTranslationProvider::new(vec![
            Ok("中文标题"),
            Ok(
                "## 更新内容\n\n* 由 [维护者](https://example.test/maintainer) 发布\n* 运行 `cargo test`",
            ),
        ]);
        let scheduler = test_scheduler(provider);
        let item = pending_item(
            "Release title",
            Some(
                "## What's Changed\n\n- by [maintainer](<https://example.test/maintainer>)\n- run `cargo test`",
            ),
        );

        let translated = scheduler
            .translate_item_for_push(&subscription(), &item)
            .await;
        let summary = translated.summary.as_deref().unwrap();

        assert!(summary.starts_with("## 更新内容"));
        assert!(summary.contains("- 由 [维护者](<https://example.test/maintainer>) 发布"));
        assert!(summary.contains("- 运行 `cargo test`"));
        assert_eq!(
            markdown_http_links(summary),
            markdown_http_links(item.summary.as_deref().unwrap())
        );
    }

    #[tokio::test]
    async fn rss_translation_with_broken_link_falls_back_to_safe_original_summary() {
        let provider = MockTranslationProvider::new(vec![
            Ok("中文标题"),
            Ok("## 更新内容\n\n- [维护者](https://changed.test/broken"),
        ]);
        let scheduler = test_scheduler(provider);
        let item = pending_item(
            "Release title",
            Some("## What's Changed\n\n- by [maintainer](<https://example.test/maintainer>)"),
        );

        let translated = scheduler
            .translate_item_for_push(&subscription(), &item)
            .await;
        let summary = translated.summary.as_deref().unwrap();

        assert_eq!(
            summary,
            render_markdown_for_qq(item.summary.as_deref().unwrap())
        );
        assert!(!summary.contains("changed.test"));
        assert_eq!(
            summary.matches("](<").count(),
            summary.matches(">)").count()
        );
        assert!(!format_push_message("订阅", &translated).contains("](<"));
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
                push_message_type: crate::config::DEFAULT_RSS_PUSH_MESSAGE_TYPE.to_owned(),
            },
        );

        scheduler.push_item(&subscription, &item).await;

        let task = notification_store
            .get_by_dedupe_key(&rss_dedupe_key(&subscription, &item))
            .unwrap()
            .unwrap();
        let message = task.payload["text"].as_str().unwrap();
        let fallback = task.payload["fallback_text"].as_str().unwrap();

        assert_eq!(task.payload["message_type"], "markdown");
        assert!(message.starts_with("## RSS 更新：Release notes from qq-maid-bot"));
        assert!(message.contains("v0.14.2"));
        assert!(message.contains(
            "[v0.14.2](<https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2>)"
        ));
        assert!(!message.contains("[v0.14.2。最终回答要求"));
        assert!(!message.contains("[cpa_final_answer]"));
        assert!(!message.contains("[tool_call]"));
        assert!(message.contains("cpa_final_answer"));
        assert!(message.contains("tool_call"));
        assert!(message.contains("最终回答要求"));
        assert_ne!(message, fallback);
        assert!(
            fallback
                .contains("链接：https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2")
        );
        assert!(!fallback.contains("## "));
        assert!(!fallback.contains("](<"));
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
    fn push_message_replaces_null_or_missing_optional_fields() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "null".to_owned(),
            link: None,
            published_at: None,
            updated_at: None,
            summary: None,
            failed_count: 0,
        };

        let text = format_push_message("null", &item);

        assert!(text.starts_with("【RSS 更新】未命名订阅"));
        assert!(text.contains("无标题"));
        assert!(!text.to_ascii_lowercase().contains("null"));
    }

    #[test]
    fn push_markdown_keeps_structure_when_optional_fields_are_empty() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "null".to_owned(),
            link: None,
            published_at: None,
            updated_at: None,
            summary: None,
            failed_count: 0,
        };

        let markdown = format_push_markdown("null", &item);

        assert_eq!(markdown, "## RSS 更新：未命名订阅\n\n### 无标题");
        assert!(!markdown.to_ascii_lowercase().contains("null"));
    }

    #[test]
    fn markdown_payload_uses_headings_and_inline_links() {
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

        let markdown = format_push_markdown("订阅", &item);
        assert!(markdown.starts_with("## RSS 更新：订阅"));
        assert!(markdown.contains("### [文章标题](<https://example.test/a>)"));
        assert!(markdown.contains("原文：[查看条目](<https://example.test/a>)"));
        assert!(markdown.contains("摘要"));
    }

    #[test]
    fn github_release_markdown_and_plain_fallback_have_independent_semantics() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            item_key: "k1".to_owned(),
            revision_hash: "r1".to_owned(),
            title: "v0.15.2".to_owned(),
            link: Some("https://example.test/releases/v0.15.2".to_owned()),
            published_at: None,
            updated_at: Some("2026-07-10T10:02:00Z".to_owned()),
            summary: Some(
                "## What's Changed\n\n- docs: 重构 README by [@kuliantnt](<https://example.test/kuliantnt>) in [#408](<https://example.test/pull/408>)\n- 修复待办详情清除与虚假成功确认"
                    .to_owned(),
            ),
            failed_count: 0,
        };

        let markdown = format_push_markdown("Release notes from qq-maid-bot", &item);
        let fallback = format_push_message("Release notes from qq-maid-bot", &item);

        assert!(markdown.contains("## What's Changed"));
        assert!(
            markdown
                .contains("- docs: 重构 README by [@kuliantnt](<https://example.test/kuliantnt>)")
        );
        assert!(markdown.contains("[#408](<https://example.test/pull/408>)"));
        assert!(markdown.contains("更新时间：2026-07-10 18:02"));
        assert!(!markdown.contains("[1]:"));
        assert!(!markdown.contains(r"\#"));
        assert!(!markdown.contains(r"\["));
        assert!(!markdown.contains(r"\-"));

        assert_ne!(markdown, fallback);
        assert!(
            fallback
                .contains("• docs: 重构 README by @kuliantnt（https://example.test/kuliantnt）")
        );
        assert!(fallback.contains("#408（https://example.test/pull/408）"));
        assert!(!fallback.contains("## What's Changed"));
        assert!(!fallback.contains("](<"));
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
        assert_ne!(markdown, text);
        assert!(markdown.contains("[文章标题](<https://example.test/original>)"));
        assert!(markdown.contains("原文：[查看条目](<https://example.test/original>)"));
    }

    #[test]
    fn push_markdown_sanitizes_dynamic_titles_without_backslash_escapes() {
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

        assert!(markdown.contains("## RSS 更新：订阅 ［测试］"));
        assert!(markdown.contains("v0.14.2 ［测试］（1）"));
        assert!(markdown.contains("cpa_final_answer 只作为正文"));
        assert!(markdown.contains("原文：[查看条目](<https://example.test/release_(1)?q=[a]>)"));
        assert!(!markdown.contains('\\'));
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
        assert!(text.contains("• Files\n• Search"));
        assert_ne!(markdown, text);
        assert!(markdown.contains("Status: Resolved\n\nAffected components"));
        assert!(markdown.contains("- Files"));
        assert!(markdown.contains("- Search"));
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
        assert_ne!(markdown, text);
        assert!(markdown.contains("更新时间：2026-06-17 08:00"));
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
        assert_ne!(markdown, text);
        assert!(markdown.contains("更新时间：无法解析的更新时间"));
    }
}
