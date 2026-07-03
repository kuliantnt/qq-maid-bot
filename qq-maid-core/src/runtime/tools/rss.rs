//! RSS 最近条目 Tool。
//!
//! 该 Tool 只读取当前会话 scope 下已入库的 RSS 状态，不新增订阅、不触发远端刷新。
//! “上次某订阅发布了什么”这类问题应基于本地轮询留下的可信状态回答。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    storage::rss::{RssRecentItem, RssStore},
};

const RSS_TOOL_NAME: &str = "get_rss_recent_items";
const RSS_TOOL_QUERY_MAX_CHARS: usize = 80;
const RSS_TOOL_DEFAULT_LIMIT: usize = 3;
const RSS_TOOL_MAX_LIMIT: usize = 10;

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
            description: "查询当前会话已订阅 RSS / Atom 的最近条目。用于回答某个订阅或关键词上次发布了什么、最近 RSS 更新有哪些；只读取本地已轮询入库状态，不新增订阅、不刷新远端。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": ["string", "null"],
                        "description": "订阅名、RSS 地址、条目标题、摘要或链接关键词，例如 codex；不确定时传 null"
                    },
                    "limit": {
                        "type": ["integer", "null"],
                        "description": "返回条数，1 到 10；询问“上次/最新一条”时传 1，不确定时传 null",
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
        })))
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
            _ => reject_bad_arguments("limit must be an integer between 1 and 10"),
        },
        _ => reject_bad_arguments("limit must be an integer or null"),
    }
}

fn reject_bad_arguments<T>(message: &str) -> Result<T, LlmError> {
    tracing::warn!(
        tool = RSS_TOOL_NAME,
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
    use crate::{
        storage::{
            APP_MIGRATIONS,
            database::SqliteDatabase,
            rss::{RssFeedItem, RssTarget, RssTargetType},
        },
        util::time_context::now_iso_cn,
    };

    use super::*;

    fn test_context() -> ToolContext {
        ToolContext {
            task_id: "msg-1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            tool_call_id: Some("call-1".to_owned()),
        }
    }

    fn test_store() -> RssStore {
        RssStore::new(SqliteDatabase::open_temp("rss-tool-tests", APP_MIGRATIONS).unwrap())
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
}
