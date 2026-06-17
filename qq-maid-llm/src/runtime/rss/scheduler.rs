//! RSS 后台轮询调度。
//!
//! 调度器只启动一个循环，逐个处理启用中的订阅，避免同一订阅并发拉取。
//! 网络请求不在 SQLite 锁内执行；发送成功后才写入 pushed_at。

use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};

use crate::{
    storage::rss::{RssPendingItem, RssStore, RssSubscription, RssTarget},
    util::time_context::format_rss_time_for_display,
};

use super::{
    feed::{RssFeedError, RssFetcher},
    push::{RssPushClient, RssPushError},
};

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

#[derive(Debug, Clone)]
pub struct RssScheduler {
    store: RssStore,
    fetcher: RssFetcher,
    push_client: RssPushClient,
    config: RssSchedulerConfig,
}

impl RssScheduler {
    pub fn new(
        store: RssStore,
        fetcher: RssFetcher,
        push_client: RssPushClient,
        config: RssSchedulerConfig,
    ) -> Self {
        Self {
            store,
            fetcher,
            push_client,
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
        let target = RssTarget {
            target_type: subscription.target_type.clone(),
            target_id: subscription.target_id.clone(),
            scope_key: subscription.scope_key.clone(),
        };
        let fallback_text = format_push_message(&subscription.title, item);
        let markdown_text = format_push_markdown(&subscription.title, item);
        let message_type = self.config.push_message_type.trim();
        let (message_type, text) = if message_type.eq_ignore_ascii_case("markdown") {
            ("markdown", markdown_text.as_str())
        } else {
            ("text", fallback_text.as_str())
        };
        match self
            .push_client
            .send(&target, message_type, text, Some(&fallback_text))
            .await
        {
            Ok(()) => {
                if let Err(err) = self
                    .store
                    .mark_item_pushed(&subscription.id, &item.fingerprint)
                {
                    warn!(
                        subscription_id = %short_id(&subscription.id),
                        item = %short_id(&item.fingerprint),
                        error = %err,
                        "failed to mark RSS item pushed"
                    );
                    return;
                }
                info!(
                    subscription_id = %short_id(&subscription.id),
                    item = %short_id(&item.fingerprint),
                    "RSS push succeeded"
                );
            }
            Err(err) => {
                warn!(
                    subscription_id = %short_id(&subscription.id),
                    item = %short_id(&item.fingerprint),
                    error = %safe_push_error(&err),
                    "RSS push failed"
                );
                if let Err(store_err) = self.store.record_item_push_failure(
                    &subscription.id,
                    &item.fingerprint,
                    &safe_push_error(&err),
                ) {
                    warn!(
                        subscription_id = %short_id(&subscription.id),
                        item = %short_id(&item.fingerprint),
                        error = %store_err,
                        "failed to persist RSS push failure"
                    );
                }
            }
        }
    }
}

pub fn format_push_message(subscription_title: &str, item: &RssPendingItem) -> String {
    let mut rows = vec![
        format!("【RSS 更新】{}", subscription_title.trim()),
        String::new(),
        item.title.trim().to_owned(),
    ];
    if let Some(summary) = item
        .summary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(summary.trim().to_owned());
    }
    if let Some(published_at) = item
        .published_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(format!(
            "发布时间：{}",
            format_rss_time_for_display(published_at)
        ));
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
    let mut rows = vec![
        format!("## RSS 更新：{}", subscription_title.trim()),
        String::new(),
    ];
    if let Some(link) = item
        .link
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(format!("### [{}]({})", item.title.trim(), link.trim()));
    } else {
        rows.push(format!("### {}", item.title.trim()));
    }
    if let Some(summary) = item
        .summary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(String::new());
        rows.push(summary.trim().to_owned());
    }
    if let Some(published_at) = item
        .published_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        rows.push(String::new());
        rows.push(format!(
            "发布时间：`{}`",
            format_rss_time_for_display(published_at)
        ));
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

fn safe_feed_error(err: &RssFeedError) -> String {
    err.to_string()
}

fn safe_push_error(err: &RssPushError) -> String {
    match err {
        RssPushError::Status { status, .. } => format!("push endpoint returned {status}"),
        _ => err.to_string(),
    }
}

fn short_id(value: &str) -> String {
    value.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_message_omits_empty_summary() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            fingerprint: "f1".to_owned(),
            item_key: "k1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: None,
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
            fingerprint: "f1".to_owned(),
            item_key: "k1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: Some("2026-06-17T00:00:00+00:00".to_owned()),
            summary: Some("摘要".to_owned()),
            failed_count: 0,
        };

        let text = format_push_markdown("订阅", &item);
        assert!(text.contains("## RSS 更新：订阅"));
        assert!(text.contains("[文章标题](https://example.test/a)"));
        assert!(text.contains("摘要"));
    }

    #[test]
    fn push_messages_keep_original_link_with_summary() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            fingerprint: "f1".to_owned(),
            item_key: "k1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/original".to_owned()),
            published_at: None,
            summary: Some("短摘要".to_owned()),
            failed_count: 0,
        };

        let text = format_push_message("订阅", &item);
        let markdown = format_push_markdown("订阅", &item);

        assert!(text.contains("短摘要"));
        assert!(text.contains("链接：https://example.test/original"));
        assert!(markdown.contains("[文章标题](https://example.test/original)"));
        assert!(markdown.contains("链接：https://example.test/original"));
    }

    #[test]
    fn push_messages_localize_published_at_for_display_only() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            fingerprint: "f1".to_owned(),
            item_key: "k1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: Some("2026-06-17T00:00:00+00:00".to_owned()),
            summary: None,
            failed_count: 0,
        };

        let text = format_push_message("订阅", &item);
        let markdown = format_push_markdown("订阅", &item);

        assert_eq!(
            item.published_at.as_deref(),
            Some("2026-06-17T00:00:00+00:00")
        );
        assert!(text.contains("发布时间：2026-06-17 08:00"));
        assert!(markdown.contains("发布时间：`2026-06-17 08:00`"));
    }

    #[test]
    fn push_messages_keep_original_published_at_when_parse_fails() {
        let item = RssPendingItem {
            subscription_id: "s1".to_owned(),
            fingerprint: "f1".to_owned(),
            item_key: "k1".to_owned(),
            title: "文章标题".to_owned(),
            link: Some("https://example.test/a".to_owned()),
            published_at: Some("无法解析的发布时间".to_owned()),
            summary: None,
            failed_count: 0,
        };

        let text = format_push_message("订阅", &item);
        let markdown = format_push_markdown("订阅", &item);

        assert!(text.contains("发布时间：无法解析的发布时间"));
        assert!(markdown.contains("发布时间：`无法解析的发布时间`"));
    }
}
