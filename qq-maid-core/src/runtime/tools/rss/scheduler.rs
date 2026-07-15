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
    config::ChatScene,
    runtime::{
        push::{PushTarget, PushTargetType},
        translation::{
            TRANSLATION_SOURCE_MAX_LENGTH, TranslationPurpose, TranslationRequest,
            TranslationService, looks_like_chinese_text,
        },
    },
    storage::notification::{NotificationOutboxStore, NotificationUpsert},
};

use super::{
    feed::{RssFeedError, RssFetcher, sanitize_rss_title},
    storage::{RssPendingItem, RssStore, RssSubscription, RssTargetType},
};

#[derive(Debug, Clone)]
pub struct RssSchedulerConfig {
    pub enabled: bool,
    pub translation_enabled: bool,
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
            RssTargetType::Private => PushTargetType::Private,
            RssTargetType::Group => PushTargetType::Group,
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
        if !self.config.translation_enabled {
            if let Some(summary) = item.summary.as_deref() {
                display_item.summary = Some(render_markdown_for_qq_with_limit(
                    summary,
                    self.config.summary_max_chars,
                ));
            }
            return display_item;
        }
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
        let scene = match subscription.target_type {
            RssTargetType::Group => ChatScene::Group,
            RssTargetType::Private => ChatScene::Private,
        };
        let translation_model = match self.translation_service.model_for_scene(scene) {
            Ok(model) => model,
            Err(err) => {
                warn!(
                    subscription_id = %short_id(&subscription.id),
                    item = %short_id(&item.item_key),
                    field,
                    error_code = err.code,
                    error_stage = err.stage,
                    "RSS translation model resolution failed, falling back to original text"
                );
                return source_text.to_owned();
            }
        };
        let translation_model_for_log = self
            .translation_service
            .model_name_for_log(translation_model.as_deref())
            .to_owned();
        match self
            .translation_service
            .translate_with_model(request, translation_model)
            .await
        {
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
                    translation_model = %translation_model_for_log,
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
#[path = "scheduler_tests.rs"]
mod tests;
