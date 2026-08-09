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
    runtime::tools::rss::{RssFeedItem, RssFetchConfig, RssTarget, RssTargetType},
    storage::{APP_MIGRATIONS, database::SqliteDatabase, notification::NotificationOutboxStore},
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
            output_parts: Vec::new(),
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
    test_scheduler_with_translation(provider, true)
}

fn test_scheduler_with_translation(
    provider: MockTranslationProvider,
    translation_enabled: bool,
) -> RssScheduler {
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
            translation_enabled,
            interval_seconds: 300,
            max_push_per_subscription: 3,
            summary_max_chars: 500,
            seen_retention: 500,
            push_max_failures: 3,
            push_message_type: crate::config::DEFAULT_RSS_PUSH_MESSAGE_TYPE.to_owned(),
        },
    )
}

fn test_scheduler_with_agent_config(provider: MockTranslationProvider) -> RssScheduler {
    let database = SqliteDatabase::open(
        std::env::temp_dir().join(format!("qq-maid-rss-scheduler-{}.db", uuid::Uuid::new_v4())),
        APP_MIGRATIONS,
    )
    .unwrap();
    let agent_config =
        crate::config::AgentRuntimeConfig::for_test("main-model", "search-model", false, false, 3)
            .with_scene_models_for_test(
                "private-main",
                Some("private-aux"),
                "group-main",
                Some("group-aux"),
            );
    RssScheduler::new(
        RssStore::new(database.clone()),
        RssFetcher::new(RssFetchConfig::default()).unwrap(),
        NotificationOutboxStore::new(database),
        TranslationService::new(Arc::new(provider), None).with_agent_config(agent_config),
        RssSchedulerConfig {
            enabled: true,
            translation_enabled: true,
            interval_seconds: 300,
            max_push_per_subscription: 3,
            summary_max_chars: 500,
            seen_retention: 500,
            push_max_failures: 3,
            push_message_type: crate::config::DEFAULT_RSS_PUSH_MESSAGE_TYPE.to_owned(),
        },
    )
}

#[tokio::test]
async fn rss_translation_is_disabled_by_default_switch() {
    let provider = MockTranslationProvider::new(Vec::new());
    let scheduler = test_scheduler_with_translation(provider.clone(), false);
    let item = pending_item("English title", Some("English **summary**"));

    let display = scheduler
        .translate_item_for_push(&subscription(), &item)
        .await;

    assert_eq!(provider.calls(), 0);
    assert_eq!(display.title, "English title");
    assert_eq!(display.summary.as_deref(), Some("English summary"));
}

#[tokio::test]
async fn rss_translation_uses_subscription_scene_aux_model() {
    let provider = MockTranslationProvider::new(vec![Ok("中文标题")]);
    let scheduler = test_scheduler_with_agent_config(provider.clone());

    let display = scheduler
        .translate_item_for_push(&subscription(), &pending_item("English title", None))
        .await;

    assert_eq!(display.title, "中文标题");
    assert_eq!(provider.requests()[0].model.as_deref(), Some("group-aux"));
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
    let provider =
        MockTranslationProvider::new(vec![Ok("中文标题"), Err(LlmError::timeout("translation"))]);
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

    assert_eq!(summary, to_qq(item.summary.as_deref().unwrap()));
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
            translation_enabled: true,
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
    assert!(
        message
            .contains("[v0.14.2](<https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2>)")
    );
    assert!(!message.contains("[v0.14.2。最终回答要求"));
    assert!(!message.contains("[cpa_final_answer]"));
    assert!(!message.contains("[tool_call]"));
    assert!(message.contains("cpa_final_answer"));
    assert!(message.contains("tool_call"));
    assert!(message.contains("最终回答要求"));
    assert_ne!(message, fallback);
    assert!(
        fallback.contains("链接：https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2")
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
            translation_enabled: true,
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
            translation_enabled: true,
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
            translation_enabled: true,
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
        markdown.contains("- docs: 重构 README by [@kuliantnt](<https://example.test/kuliantnt>)")
    );
    assert!(markdown.contains("[#408](<https://example.test/pull/408>)"));
    assert!(markdown.contains("更新时间：2026-07-10 18:02"));
    assert!(!markdown.contains("[1]:"));
    assert!(!markdown.contains(r"\#"));
    assert!(!markdown.contains(r"\["));
    assert!(!markdown.contains(r"\-"));

    assert_ne!(markdown, fallback);
    assert!(
        fallback.contains("• docs: 重构 README by @kuliantnt（https://example.test/kuliantnt）")
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
        summary: Some("Status: Resolved\n\nAffected components\n\n* Files\n* Search".to_owned()),
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
