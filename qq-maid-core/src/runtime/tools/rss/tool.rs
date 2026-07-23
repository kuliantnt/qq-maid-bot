//! RSS Tool。
//!
//! 最近条目查询只读取当前会话 scope 下已入库的 RSS 状态，不触发远端刷新。
//! “上次某订阅发布了什么”这类问题应基于本地轮询留下的可信状态回答。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_common::identity_context::ConversationKind;
#[cfg(test)]
use qq_maid_common::identity_context::{ExecutionActorContext, ExecutionConversationContext};
use qq_maid_llm::tool::{Tool, ToolContext, ToolEffect, ToolMetadata, ToolOutput};

use crate::{error::LlmError, runtime::group_role::is_group_owner_or_admin};

use super::{
    feed::{RssFeedError, RssFetcher},
    storage::{RssRecentItem, RssStore, RssSubscription, RssTarget, RssTargetType},
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

pub(crate) mod route {
    //! RSS 普通消息的状态意图判断；不参与工具暴露、权限或执行决策。

    pub(crate) fn has_rss_intent(text: &str, local_text_processing_intent: bool) -> bool {
        if local_text_processing_intent {
            return false;
        }

        let normalized = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let compact = normalized.split_whitespace().collect::<String>();
        let explicit_subscription_phrase = contains_any(
            &compact,
            &["查订阅", "查询订阅", "查看订阅", "看看订阅", "最近订阅"],
        );
        let rss_query = contains_ascii_word(&normalized, "rss")
            && contains_any(
                &compact,
                &[
                    "查",
                    "查询",
                    "看看",
                    "看下",
                    "看一下",
                    "最近",
                    "更新",
                    "发布了",
                    "上次",
                ],
            );
        explicit_subscription_phrase || rss_query
    }

    fn contains_any(text: &str, needles: &[&str]) -> bool {
        needles.iter().any(|needle| text.contains(needle))
    }

    fn contains_ascii_word(text: &str, expected: &str) -> bool {
        text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .any(|word| word == expected)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rss_status_requires_a_query_request() {
            for input in ["看看 RSS 最近更新", "最近订阅有什么更新", "查询订阅记录"]
            {
                assert!(has_rss_intent(input, false), "{input}");
            }
            for input in [
                "RSS 是什么格式",
                "这个 RSS parser 怎么设计",
                "聊聊 rss",
                "订阅更新的数据结构怎么设计",
            ] {
                assert!(!has_rss_intent(input, false), "{input}");
            }

            assert!(!has_rss_intent("最近 RSS 更新\n总结这段", true));
        }
    }
}

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

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
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
            .recent_items_by_scope(&context.conversation.scope_id, query.as_deref(), limit)
            .map_err(|err| {
                LlmError::new(
                    err.code().to_owned(),
                    format!("rss store failed: {}", err.message()),
                    "rss",
                )
            })?;
        Ok(ToolOutput::json(json!({
            "scope_id": context.conversation.scope_id,
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
        let target = target_from_context(context)?;
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
            "scope_id": compact_manage_string(&context.conversation.scope_id, RSS_MANAGE_OUTPUT_SCOPE_MAX_CHARS, &mut details_truncated),
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
            .list_by_scope(&context.conversation.scope_id)
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
                .delete_for_scope(&context.conversation.scope_id, &subscription.id)
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
            "scope_id": compact_manage_string(&context.conversation.scope_id, RSS_MANAGE_OUTPUT_SCOPE_MAX_CHARS, &mut details_truncated),
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
        let operation = required_string(arguments.get("operation"), "operation")?;
        if let Some(output) = validate_manage_context(&context) {
            return Ok(output);
        }
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
    validate_add_entries(&entries)?;
    Ok(entries)
}

fn validate_add_entries(entries: &[RssToolAddEntry]) -> Result<(), LlmError> {
    for entry in entries {
        validate_url(&entry.url)?;
    }
    Ok(())
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

fn validate_manage_context(context: &ToolContext) -> Option<ToolOutput> {
    let message = match context.conversation.kind {
        ConversationKind::Private | ConversationKind::ServiceAccount => return None,
        ConversationKind::Group
            if context
                .actor
                .group_member_role
                .as_deref()
                .is_some_and(is_group_owner_or_admin) =>
        {
            return None;
        }
        ConversationKind::Group => "群聊 RSS 管理只允许群主或管理员执行。",
        ConversationKind::Channel => "频道会话不允许执行 RSS 管理操作。",
        ConversationKind::Unknown => "无法确认会话类型，已拒绝执行 RSS 管理操作。",
    };
    Some(ToolOutput::json(json!({
        "ok": false,
        "error": {"code": "permission_denied", "message": message},
    })))
}

fn target_from_context(context: &ToolContext) -> Result<RssTarget, LlmError> {
    let (target_type, target_id) = match context.conversation.kind {
        ConversationKind::Group => (
            RssTargetType::Group,
            non_empty_id(context.conversation.target_id.as_deref()).ok_or_else(|| {
                LlmError::new(
                    "missing_conversation_target",
                    "group RSS management requires an authoritative conversation target id",
                    "tool",
                )
            })?,
        ),
        ConversationKind::Private | ConversationKind::ServiceAccount => (
            RssTargetType::Private,
            non_empty_id(context.conversation.target_id.as_deref())
                .or_else(|| non_empty_id(context.actor.user_id.as_deref()))
                .ok_or_else(|| {
                    LlmError::new(
                        "missing_conversation_target",
                        "private RSS management requires a conversation or actor target id",
                        "tool",
                    )
                })?,
        ),
        ConversationKind::Channel | ConversationKind::Unknown => {
            return Err(LlmError::new(
                "permission_denied",
                "rss management is only available in private or group chat scope",
                "tool",
            ));
        }
    };
    Ok(RssTarget {
        target_type,
        target_id: target_id.to_owned(),
        scope_key: context.conversation.scope_id.clone(),
    })
}

fn non_empty_id(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
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

fn rss_store_error(err: super::storage::RssStoreError) -> LlmError {
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
#[path = "tool_tests.rs"]
mod tests;
