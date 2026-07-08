//! RSS Tool。
//!
//! 最近条目查询只读取当前会话 scope 下已入库的 RSS 状态，不触发远端刷新。
//! “上次某订阅发布了什么”这类问题应基于本地轮询留下的可信状态回答。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::{
        group_role::group_management_allowed,
        rss::{RssFetcher, feed::RssFeedError},
    },
    storage::rss::{RssRecentItem, RssStore, RssSubscription, RssTarget, RssTargetType},
};

const RSS_TOOL_NAME: &str = "get_rss_recent_items";
const RSS_MANAGE_TOOL_NAME: &str = "manage_rss_subscriptions";
const RSS_TOOL_QUERY_MAX_CHARS: usize = 80;
const RSS_TOOL_DEFAULT_LIMIT: usize = 3;
const RSS_TOOL_MAX_LIMIT: usize = 20;
const RSS_TOOL_MAX_BATCH_ITEMS: usize = 10;
const RSS_TOOL_NAME_MAX_CHARS: usize = 120;
const RSS_TOOL_URL_MAX_CHARS: usize = 500;
const RSS_MANAGE_OUTPUT_TITLE_MAX_CHARS: usize = 80;
const RSS_MANAGE_OUTPUT_URL_MAX_CHARS: usize = 180;
const RSS_MANAGE_OUTPUT_ERROR_MAX_CHARS: usize = 180;
const RSS_MANAGE_OUTPUT_TARGET_MAX_CHARS: usize = 120;
const RSS_MANAGE_OUTPUT_SCOPE_MAX_CHARS: usize = 120;

/// 模型可调用的 RSS 最近条目查询 Tool。
#[derive(Clone)]
pub struct RssRecentItemsTool {
    store: RssStore,
}

impl RssRecentItemsTool {
    pub fn new(store: RssStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for RssRecentItemsTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: RSS_TOOL_NAME.to_owned(),
            description: "查询当前会话已订阅 RSS / Atom 的最近条目。用于回答某个订阅或关键词上次发布了什么、最近 RSS 更新有哪些，也用于先读取本地条目的标题、摘要、链接和时间后总结最近更新；只读取本地已轮询入库状态，不新增订阅、不刷新远端。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": ["string", "null"],
                        "description": "订阅名、RSS 地址、条目标题、摘要或链接关键词，例如 codex；不确定时传 null"
                    },
                    "limit": {
                        "type": ["integer", "null"],
                        "description": "返回条数，1 到 20；询问“上次/最新一条”时传 1，需要总结最近更新时可传 10 到 20，不确定时传 null",
                        "minimum": 1,
                        "maximum": RSS_TOOL_MAX_LIMIT
                    }
                },
                "required": ["query", "limit"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let query = parse_query(arguments.get("query"))?;
        let limit = parse_limit(arguments.get("limit"))?;
        let items = self
            .store
            .recent_items_by_scope(&context.scope_id, query.as_deref(), limit)
            .map_err(|err| {
                LlmError::new(
                    err.code().to_owned(),
                    format!("rss store failed: {}", err.message()),
                    "rss",
                )
            })?;
        Ok(ToolOutput::json(json!({
            "scope_id": context.scope_id,
            "query": query,
            "limit": limit,
            "items": items.iter().map(recent_item_json).collect::<Vec<_>>(),
            "summary_guidance": "如用户要求总结 RSS 最近更新，请只基于 items 中的 title/summary/link/published_at/updated_at 归纳，不要编造未返回的更新。",
        })))
    }
}

/// 模型可调用的 RSS 订阅管理 Tool。
#[derive(Clone)]
pub struct RssManageSubscriptionsTool {
    store: RssStore,
    fetcher: RssFetcher,
    summary_max_chars: usize,
    seen_retention: usize,
}

impl RssManageSubscriptionsTool {
    pub fn new(
        store: RssStore,
        fetcher: RssFetcher,
        summary_max_chars: usize,
        seen_retention: usize,
    ) -> Self {
        Self {
            store,
            fetcher,
            summary_max_chars,
            seen_retention,
        }
    }

    async fn execute_add(
        &self,
        context: &ToolContext,
        arguments: &Value,
    ) -> Result<ToolOutput, LlmError> {
        let entries = parse_tool_add_entries(arguments)?;
        let target = target_from_context(context);
        let mut created = Vec::new();
        let mut failed = Vec::new();
        let mut details_truncated = false;
        for entry in entries {
            match self.fetcher.fetch(&entry.url, self.summary_max_chars).await {
                Ok(feed) => {
                    let item_count = feed.items.len();
                    let title = entry.title.unwrap_or(feed.title);
                    match self.store.create_subscription(
                        &target,
                        &entry.url,
                        &title,
                        &feed.items,
                        self.seen_retention,
                    ) {
                        Ok(subscription) => created.push(compact_manage_subscription_json(
                            &subscription,
                            Some(item_count),
                            &mut details_truncated,
                        )),
                        Err(err) => failed.push(compact_manage_failure_json(
                            &entry.url,
                            err.message(),
                            &mut details_truncated,
                        )),
                    }
                }
                Err(err) => failed.push(compact_manage_failure_json(
                    &entry.url,
                    &feed_error_reply(&err),
                    &mut details_truncated,
                )),
            }
        }
        Ok(ToolOutput::json(json!({
            "ok": !created.is_empty(),
            "operation": "add",
            "scope_id": compact_manage_string(&context.scope_id, RSS_MANAGE_OUTPUT_SCOPE_MAX_CHARS, &mut details_truncated),
            "created": created,
            "failed": failed,
            "details_truncated": details_truncated,
            "message": format_manage_message("add", created.len(), failed.len()),
        })))
    }

    async fn execute_delete(
        &self,
        context: &ToolContext,
        arguments: &Value,
    ) -> Result<ToolOutput, LlmError> {
        let targets = parse_tool_delete_targets(arguments)?;
        let subscriptions = self
            .store
            .list_by_scope(&context.scope_id)
            .map_err(rss_store_error)?;
        let mut resolved = Vec::<&RssSubscription>::new();
        let mut missing = Vec::<String>::new();
        let mut details_truncated = false;
        for target in &targets {
            if let Some(subscription) = resolve_subscription_target(&subscriptions, target) {
                if !resolved.iter().any(|item| item.id == subscription.id) {
                    resolved.push(subscription);
                }
            } else {
                missing.push(compact_manage_string(
                    target,
                    RSS_MANAGE_OUTPUT_TARGET_MAX_CHARS,
                    &mut details_truncated,
                ));
            }
        }

        let mut deleted = Vec::new();
        for subscription in resolved {
            if self
                .store
                .delete_for_scope(&context.scope_id, &subscription.id)
                .map_err(rss_store_error)?
            {
                deleted.push(compact_manage_subscription_json(
                    subscription,
                    None,
                    &mut details_truncated,
                ));
            }
        }
        Ok(ToolOutput::json(json!({
            "ok": !deleted.is_empty(),
            "operation": "delete",
            "scope_id": compact_manage_string(&context.scope_id, RSS_MANAGE_OUTPUT_SCOPE_MAX_CHARS, &mut details_truncated),
            "deleted": deleted,
            "missing": missing,
            "details_truncated": details_truncated,
            "message": format_manage_message("delete", deleted.len(), missing.len()),
        })))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RssToolAddEntry {
    url: String,
    title: Option<String>,
}

#[async_trait]
impl Tool for RssManageSubscriptionsTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: RSS_MANAGE_TOOL_NAME.to_owned(),
            description: "管理当前会话 scope 下的 RSS / Atom 订阅，支持批量新增和批量删除。新增会真实访问并解析 URL，成功后写入订阅表并把当前历史条目标记为已见；删除只会删除当前 scope 内匹配的订阅。群聊中只有群主或管理员允许执行。用户只是询问最近更新或要求总结时，不要调用本工具，应调用 get_rss_recent_items。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["add", "delete"],
                        "description": "要执行的管理动作。"
                    },
                    "feeds": {
                        "type": ["array", "null"],
                        "description": "新增订阅列表；operation=add 时使用。每项 url 必填，title 可为 null。",
                        "minItems": 1,
                        "maxItems": RSS_TOOL_MAX_BATCH_ITEMS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "url": {"type": "string"},
                                "title": {"type": ["string", "null"]}
                            },
                            "required": ["url", "title"],
                            "additionalProperties": false
                        }
                    },
                    "targets": {
                        "type": ["array", "null"],
                        "description": "删除目标列表；operation=delete 时使用。可传 /rss list 中显示的编号、订阅 ID 或 ID 前缀。",
                        "minItems": 1,
                        "maxItems": RSS_TOOL_MAX_BATCH_ITEMS,
                        "items": {"type": "string"}
                    },
                    "raw_text": {
                        "type": ["string", "null"],
                        "description": "兼容用户粘贴的多行 RSS 列表，例如“1. 标题\\nhttps://example.com/feed.xml”。feeds/targets 已结构化时传 null。"
                    }
                },
                "required": ["operation", "feeds", "targets", "raw_text"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        if !group_management_allowed(
            None,
            &context.scope_id,
            context.group_member_role.as_deref(),
        ) {
            return Ok(ToolOutput::json(json!({
                "ok": false,
                "error": {"code": "permission_denied", "message": "群聊 RSS 管理只允许群主或管理员执行。"},
            })));
        }

        let operation = required_string(arguments.get("operation"), "operation")?;
        match operation.as_str() {
            "add" => self.execute_add(&context, &arguments).await,
            "delete" => self.execute_delete(&context, &arguments).await,
            _ => reject_bad_arguments("operation must be add or delete"),
        }
    }
}

fn parse_query(value: Option<&Value>) -> Result<Option<String>, LlmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let query = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(query) = query else {
        return reject_bad_arguments("query must be a string or null");
    };
    if query.chars().count() > RSS_TOOL_QUERY_MAX_CHARS {
        return reject_bad_arguments("query is too long");
    }
    Ok(Some(query.to_owned()))
}

fn parse_limit(value: Option<&Value>) -> Result<usize, LlmError> {
    let Some(value) = value else {
        return Ok(RSS_TOOL_DEFAULT_LIMIT);
    };
    if value.is_null() {
        return Ok(RSS_TOOL_DEFAULT_LIMIT);
    }
    match value {
        Value::Number(n) if !n.is_f64() => match n.as_i64() {
            Some(i) if (1..=RSS_TOOL_MAX_LIMIT as i64).contains(&i) => Ok(i as usize),
            _ => reject_bad_arguments("limit must be an integer between 1 and 20"),
        },
        _ => reject_bad_arguments("limit must be an integer or null"),
    }
}

fn parse_tool_add_entries(arguments: &Value) -> Result<Vec<RssToolAddEntry>, LlmError> {
    let mut entries = Vec::new();
    if let Some(feeds) = arguments.get("feeds").and_then(Value::as_array) {
        if feeds.len() > RSS_TOOL_MAX_BATCH_ITEMS {
            return reject_bad_arguments("feeds is too long");
        }
        for feed in feeds {
            let url = required_string(feed.get("url"), "url")?;
            validate_url(&url)?;
            let title = optional_string(feed.get("title"), "title", RSS_TOOL_NAME_MAX_CHARS)?;
            entries.push(RssToolAddEntry { url, title });
        }
    }
    if entries.is_empty()
        && let Some(raw_text) = optional_string(arguments.get("raw_text"), "raw_text", 4000)?
    {
        entries = parse_raw_add_entries(&raw_text);
    }
    if entries.is_empty() {
        return reject_bad_arguments("feeds or raw_text must contain at least one RSS URL");
    }
    if entries.len() > RSS_TOOL_MAX_BATCH_ITEMS {
        return reject_bad_arguments("too many RSS feeds");
    }
    Ok(entries)
}

fn parse_tool_delete_targets(arguments: &Value) -> Result<Vec<String>, LlmError> {
    let mut targets = Vec::new();
    if let Some(values) = arguments.get("targets").and_then(Value::as_array) {
        if values.len() > RSS_TOOL_MAX_BATCH_ITEMS {
            return reject_bad_arguments("targets is too long");
        }
        for value in values {
            targets.push(required_string(Some(value), "target")?);
        }
    }
    if targets.is_empty()
        && let Some(raw_text) = optional_string(arguments.get("raw_text"), "raw_text", 4000)?
    {
        targets = raw_text
            .lines()
            .flat_map(|line| line.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | '，')))
            .map(strip_list_marker)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();
    }
    if targets.is_empty() {
        return reject_bad_arguments("targets or raw_text must contain at least one delete target");
    }
    if targets.len() > RSS_TOOL_MAX_BATCH_ITEMS {
        return reject_bad_arguments("too many delete targets");
    }
    Ok(targets)
}

fn parse_raw_add_entries(raw_text: &str) -> Vec<RssToolAddEntry> {
    let mut entries = Vec::new();
    let mut pending_title: Option<String> = None;
    for line in raw_text.lines() {
        let line = strip_list_marker(line.trim());
        if line.is_empty() {
            continue;
        }
        if let Some((title_prefix, url, title_suffix)) = extract_url_from_line(line) {
            let title = clean_optional(title_prefix, RSS_TOOL_NAME_MAX_CHARS)
                .or_else(|| pending_title.take())
                .or_else(|| clean_optional(title_suffix, RSS_TOOL_NAME_MAX_CHARS));
            entries.push(RssToolAddEntry {
                url: url.to_owned(),
                title,
            });
            continue;
        }
        pending_title = clean_optional(line, RSS_TOOL_NAME_MAX_CHARS);
    }
    entries
}

fn required_string(value: Option<&Value>, field: &str) -> Result<String, LlmError> {
    optional_string(value, field, RSS_TOOL_URL_MAX_CHARS)?
        .ok_or_else(|| LlmError::new("bad_tool_arguments", format!("{field} is required"), "tool"))
}

fn optional_string(
    value: Option<&Value>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>, LlmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(text) = value.as_str() else {
        return reject_bad_arguments(&format!("{field} must be a string or null"));
    };
    Ok(clean_optional(text, max_chars))
}

fn clean_optional(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(max_chars).collect())
}

fn validate_url(url: &str) -> Result<(), LlmError> {
    if url.chars().count() > RSS_TOOL_URL_MAX_CHARS {
        return reject_bad_arguments("url is too long");
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return reject_bad_arguments("url must be http or https");
    }
    Ok(())
}

fn target_from_context(context: &ToolContext) -> RssTarget {
    let is_group = context.scope_id.starts_with("group:") || context.scope_id.contains(":group:");
    let target_id = if is_group {
        id_from_scope(&context.scope_id, "group").unwrap_or_else(|| context.scope_id.clone())
    } else {
        id_from_scope(&context.scope_id, "private").unwrap_or_else(|| {
            context
                .user_id
                .clone()
                .unwrap_or_else(|| context.scope_id.clone())
        })
    };
    RssTarget {
        target_type: if is_group {
            RssTargetType::Group
        } else {
            RssTargetType::Private
        },
        // ToolContext 不额外携带 group_id；这里仅从服务端 scope 中恢复订阅目标 id，
        // scope_key 仍是隔离边界，删除和查询不会跨会话泄漏。
        target_id,
        scope_key: context.scope_id.clone(),
    }
}

fn id_from_scope(scope_id: &str, marker: &str) -> Option<String> {
    let prefix = format!("{marker}:");
    if let Some(id) = scope_id.strip_prefix(&prefix) {
        return clean_optional(id, RSS_TOOL_URL_MAX_CHARS);
    }
    let marker = format!(":{marker}:");
    scope_id
        .rsplit_once(&marker)
        .and_then(|(_, id)| clean_optional(id, RSS_TOOL_URL_MAX_CHARS))
}

fn resolve_subscription_target<'a>(
    subscriptions: &'a [RssSubscription],
    target: &str,
) -> Option<&'a RssSubscription> {
    let target = target.split_whitespace().next().unwrap_or("").trim();
    if target.chars().all(|ch| ch.is_ascii_digit()) {
        let index = target.parse::<usize>().ok()?;
        return subscriptions
            .get(index.saturating_sub(1))
            .filter(|_| index > 0);
    }
    subscriptions
        .iter()
        .find(|subscription| subscription.id == target || subscription.id.starts_with(target))
}

fn strip_list_marker(value: &str) -> &str {
    let value = value.trim();
    let digit_count = value.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count == 0 {
        return value;
    }
    let Some(rest) = value.get(digit_count..) else {
        return value;
    };
    let rest = rest.trim_start();
    let mut chars = rest.chars();
    match chars.next() {
        Some('.' | '。' | '、' | ')' | '）' | ':' | '：') => chars.as_str().trim_start(),
        _ => value,
    }
}

fn extract_url_from_line(line: &str) -> Option<(&str, &str, &str)> {
    let start = line.find("https://").or_else(|| line.find("http://"))?;
    let before = line[..start].trim();
    let after_start = &line[start..];
    let url_len = after_start
        .find(char::is_whitespace)
        .unwrap_or(after_start.len());
    let url = &after_start[..url_len];
    let after = after_start[url_len..].trim();
    Some((before, url, after))
}

fn rss_store_error(err: crate::storage::rss::RssStoreError) -> LlmError {
    LlmError::new(
        err.code().to_owned(),
        format!("rss store failed: {}", err.message()),
        "rss",
    )
}

fn feed_error_reply(err: &RssFeedError) -> String {
    match err {
        RssFeedError::Status(status) => format!("HTTP {status}"),
        RssFeedError::UnsafeHost => "地址指向本机、内网或 metadata，已拦截".to_owned(),
        _ => err.to_string(),
    }
}

fn compact_manage_subscription_json(
    subscription: &RssSubscription,
    baseline_item_count: Option<usize>,
    details_truncated: &mut bool,
) -> Value {
    let mut item = json!({
        "id": subscription.id,
        "title": compact_manage_string(
            &subscription.title,
            RSS_MANAGE_OUTPUT_TITLE_MAX_CHARS,
            details_truncated,
        ),
        "url": compact_manage_string(
            &subscription.url,
            RSS_MANAGE_OUTPUT_URL_MAX_CHARS,
            details_truncated,
        ),
    });
    if let Some(count) = baseline_item_count {
        item["baseline_item_count"] = json!(count);
    }
    item
}

fn compact_manage_failure_json(url: &str, error: &str, details_truncated: &mut bool) -> Value {
    json!({
        "url": compact_manage_string(url, RSS_MANAGE_OUTPUT_URL_MAX_CHARS, details_truncated),
        "error": compact_manage_string(
            error,
            RSS_MANAGE_OUTPUT_ERROR_MAX_CHARS,
            details_truncated,
        ),
    })
}

fn compact_manage_string(value: &str, max_chars: usize, details_truncated: &mut bool) -> String {
    let value = value.trim();
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    *details_truncated = true;
    value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>()
        + "..."
}

fn format_manage_message(operation: &str, success: usize, failed: usize) -> String {
    match operation {
        "add" => format!("RSS 批量新增完成：成功 {success} 个，失败 {failed} 个。"),
        "delete" => format!("RSS 批量删除完成：成功 {success} 个，未找到 {failed} 个。"),
        _ => "RSS 管理操作完成。".to_owned(),
    }
}

fn reject_bad_arguments<T>(message: &str) -> Result<T, LlmError> {
    tracing::warn!(
        tool = "rss",
        error_code = "bad_tool_arguments",
        "invalid RSS tool argument rejected",
    );
    Err(LlmError::new("bad_tool_arguments", message, "tool"))
}

fn recent_item_json(item: &RssRecentItem) -> Value {
    json!({
        "subscription": {
            "id": item.subscription_id,
            "title": item.subscription_title,
            "url": item.subscription_url,
        },
        "item": {
            "item_key": item.item_key,
            "revision_hash": item.revision_hash,
            "title": item.title,
            "link": item.link,
            "published_at": item.published_at,
            "updated_at": item.updated_at,
            "summary": item.summary,
            "pushed_at": item.pushed_at,
            "last_seen_at": item.last_seen_at,
        },
    })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use crate::{
        runtime::rss::RssFetchConfig,
        storage::{
            APP_MIGRATIONS,
            database::SqliteDatabase,
            rss::{RssFeedItem, RssSubscription, RssTarget, RssTargetType},
        },
        util::time_context::now_iso_cn,
    };

    use super::*;

    fn test_context() -> ToolContext {
        ToolContext {
            task_id: "msg-1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            group_member_role: None,
            tool_call_id: Some("call-1".to_owned()),
        }
    }

    fn test_store() -> RssStore {
        RssStore::new(SqliteDatabase::open_temp("rss-tool-tests", APP_MIGRATIONS).unwrap())
    }

    fn test_fetcher() -> RssFetcher {
        RssFetcher::new(RssFetchConfig {
            timeout_seconds: 5,
            max_body_bytes: 1024 * 1024,
            user_agent: "rss-tool-test".to_owned(),
            allow_private_networks: true,
        })
        .unwrap()
    }

    fn spawn_feed_server(title: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let body = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>{title}</title><item><title>Item</title><link>https://example.test/item</link><guid>{title}</guid></item></channel></rss>"#
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });
        format!("http://{addr}/feed.xml")
    }

    fn feed_item(key: &str, title: &str) -> RssFeedItem {
        RssFeedItem {
            item_key: key.to_owned(),
            revision_hash: format!("rev:{key}"),
            title: title.to_owned(),
            link: Some(format!("https://example.test/{key}")),
            published_at: Some("2026-06-18T00:00:00+00:00".to_owned()),
            updated_at: Some("2026-06-18T01:00:00+00:00".to_owned()),
            summary: Some("Codex 发布摘要".to_owned()),
            source_order: 0,
        }
    }

    #[tokio::test]
    async fn rss_tool_reads_recent_items_from_current_scope() {
        let store = test_store();
        let target = RssTarget {
            target_type: RssTargetType::Private,
            target_id: "u1".to_owned(),
            scope_key: "private:u1".to_owned(),
        };
        let sub = store
            .create_subscription(
                &target,
                "https://example.test/codex.xml",
                "Codex 发布",
                &[],
                50,
            )
            .unwrap();
        store
            .enqueue_items(&sub.id, &[feed_item("codex-1", "Codex v1")], 50)
            .unwrap();
        let tool = RssRecentItemsTool::new(store);

        let output = tool
            .execute(test_context(), json!({"query": "codex", "limit": 1}))
            .await
            .unwrap();

        assert_eq!(output.value["items"].as_array().unwrap().len(), 1);
        assert_eq!(
            output.value["items"][0]["subscription"]["title"],
            "Codex 发布"
        );
        assert_eq!(output.value["items"][0]["item"]["title"], "Codex v1");
        assert_eq!(output.value["query"], "codex");
    }

    #[tokio::test]
    async fn rss_tool_rejects_invalid_limit() {
        let tool = RssRecentItemsTool::new(test_store());

        let err = tool
            .execute(test_context(), json!({"query": null, "limit": 0}))
            .await
            .unwrap_err();

        assert_eq!(err.code, "bad_tool_arguments");
    }

    #[tokio::test]
    async fn rss_tool_returns_empty_list_when_no_match() {
        let store = test_store();
        let target = RssTarget {
            target_type: RssTargetType::Private,
            target_id: "u1".to_owned(),
            scope_key: "private:u1".to_owned(),
        };
        let sub = store
            .create_subscription(
                &target,
                "https://example.test/feed.xml",
                "普通订阅",
                &[RssFeedItem {
                    item_key: "baseline".to_owned(),
                    revision_hash: "rev:baseline".to_owned(),
                    title: "普通标题".to_owned(),
                    link: None,
                    published_at: None,
                    updated_at: None,
                    summary: None,
                    source_order: 0,
                }],
                50,
            )
            .unwrap();
        store.mark_item_pushed(&sub.id, "baseline").unwrap();
        let tool = RssRecentItemsTool::new(store);

        let output = tool
            .execute(test_context(), json!({"query": "codex", "limit": null}))
            .await
            .unwrap();

        assert!(output.value["items"].as_array().unwrap().is_empty());
        assert!(
            output.value["scope_id"]
                .as_str()
                .unwrap()
                .starts_with("private:")
        );
        assert!(!now_iso_cn().is_empty());
    }

    #[tokio::test]
    async fn rss_manage_tool_adds_numbered_raw_text_in_current_scope() {
        let store = test_store();
        let first = spawn_feed_server("Feed One");
        let second = spawn_feed_server("Feed Two");
        let tool = RssManageSubscriptionsTool::new(store.clone(), test_fetcher(), 500, 50);

        let output = tool
            .execute(
                test_context(),
                json!({
                    "operation": "add",
                    "feeds": null,
                    "targets": null,
                    "raw_text": format!("1. Release notes\n{first}\n2. Recent Commits\n{second}")
                }),
            )
            .await
            .unwrap();

        assert_eq!(output.value["ok"], true);
        assert_eq!(output.value["created"].as_array().unwrap().len(), 2);
        let subscriptions = store.list_by_scope("private:u1").unwrap();
        assert_eq!(subscriptions.len(), 2);
        assert!(
            subscriptions
                .iter()
                .any(|subscription| subscription.title == "Release notes")
        );
        assert!(
            subscriptions
                .iter()
                .any(|subscription| subscription.title == "Recent Commits")
        );
    }

    #[tokio::test]
    async fn rss_manage_tool_rejects_group_member_without_admin_role() {
        let tool = RssManageSubscriptionsTool::new(test_store(), test_fetcher(), 500, 50);
        let mut context = test_context();
        context.scope_id = "platform:qq_official:account:app-1:group:g1".to_owned();
        context.group_member_role = Some("member".to_owned());

        let output = tool
            .execute(
                context,
                json!({
                    "operation": "delete",
                    "feeds": null,
                    "targets": ["1"],
                    "raw_text": null
                }),
            )
            .await
            .unwrap();

        assert_eq!(output.value["ok"], false);
        assert_eq!(output.value["error"]["code"], "permission_denied");
    }

    #[test]
    fn rss_manage_compact_output_stays_under_default_tool_limit_for_full_batch() {
        let long_title = "很长的订阅标题".repeat(20);
        let long_url = format!("https://example.test/{}", "a".repeat(470));
        let long_error = "解析失败：返回内容不是 RSS 或 Atom 文档。".repeat(20);
        let now = now_iso_cn();
        let created = (0..RSS_TOOL_MAX_BATCH_ITEMS)
            .map(|index| {
                let subscription = RssSubscription {
                    id: format!("00000000-0000-0000-0000-{index:012}"),
                    target_type: RssTargetType::Private,
                    target_id: "u1".to_owned(),
                    scope_key: "private:u1".to_owned(),
                    url: long_url.clone(),
                    title: long_title.clone(),
                    enabled: true,
                    created_at: now.clone(),
                    last_checked_at: None,
                    last_success_at: None,
                    last_error: None,
                    consecutive_failures: 0,
                    initialized: true,
                };
                let mut details_truncated = false;
                compact_manage_subscription_json(&subscription, Some(1), &mut details_truncated)
            })
            .collect::<Vec<_>>();
        let mut details_truncated = false;
        let failed = (0..RSS_TOOL_MAX_BATCH_ITEMS)
            .map(|_| compact_manage_failure_json(&long_url, &long_error, &mut details_truncated))
            .collect::<Vec<_>>();
        let outputs = [
            json!({
                "ok": true,
                "operation": "add",
                "scope_id": "private:u1",
                "created": created,
                "failed": [],
                "details_truncated": true,
                "message": format_manage_message("add", RSS_TOOL_MAX_BATCH_ITEMS, 0),
            }),
            json!({
                "ok": false,
                "operation": "add",
                "scope_id": "private:u1",
                "created": [],
                "failed": failed,
                "details_truncated": details_truncated,
                "message": format_manage_message("add", 0, RSS_TOOL_MAX_BATCH_ITEMS),
            }),
        ];

        for output in outputs {
            let serialized = serde_json::to_string(&output).unwrap();
            assert!(
                serialized.chars().count() <= qq_maid_llm::tool::DEFAULT_TOOL_OUTPUT_MAX_CHARS,
                "RSS 管理输出不应触发通用 Tool 截断，实际 {} 字符",
                serialized.chars().count()
            );
            assert_eq!(output["details_truncated"], true);
            assert_eq!(output["operation"], "add");
            assert!(output.get("ok").and_then(Value::as_bool).is_some());
        }
    }
}
