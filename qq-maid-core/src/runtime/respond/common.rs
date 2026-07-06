//! 通用工具函数与常量。
//!
//! 提供会话流、聊天流等子模块共享的辅助函数：
//! 错误转换、请求构造、元数据合并、字符串清理、JSON 抽取等。

use std::collections::HashMap;

pub(super) use qq_maid_common::text::truncate_chars_with_ellipsis_trimmed as truncate_chars;
use serde_json::{Value, json};

use crate::{error::LlmError, runtime::session::SessionRecord, util::metrics::LlmMetrics};

use super::{RespondPurpose, RespondRequest, RespondResponse};

/// 命令回复的双通道正文。
///
/// `text` 必须始终可读；`markdown` 仅在需要保留结构化排版时提供。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandBody {
    pub text: String,
    pub markdown: Option<String>,
}

impl CommandBody {
    pub(super) fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            markdown: None,
        }
    }

    pub(super) fn dual(text: impl Into<String>, markdown: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            markdown: Some(markdown.into()),
        }
    }
}

/// 将结构化命令正文显式拆成 `markdown + text` 双通道。
///
/// 旧 gateway 曾直接把 `text` 当 Markdown 发送；双通道改造后，这类回复需要在
/// LLM 层明确保留原正文，同时生成纯文本 fallback，避免把 Markdown 判定重新放回
/// gateway。
pub(super) fn structured_command_body(markdown: impl Into<String>) -> CommandBody {
    let markdown = markdown.into();
    CommandBody::dual(
        super::markdown_strip::strip_markdown_for_chat(&markdown),
        markdown,
    )
}

pub(super) const GROUP_ADMIN_REQUIRED_REPLY: &str = "这个群管理操作只允许群主或管理员执行。";

pub(super) fn group_management_allowed(req: &RespondRequest) -> bool {
    !is_group_request(req)
        || req
            .group_member_role
            .as_deref()
            .is_some_and(is_group_owner_or_admin)
}

fn is_group_request(req: &RespondRequest) -> bool {
    req.group_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || req.scope_key.starts_with("group:")
}

fn is_group_owner_or_admin(role: &str) -> bool {
    matches!(role.trim().to_ascii_lowercase().as_str(), "owner" | "admin")
}

impl From<String> for CommandBody {
    fn from(value: String) -> Self {
        Self::plain(value)
    }
}

impl From<&str> for CommandBody {
    fn from(value: &str) -> Self {
        Self::plain(value)
    }
}

/// 从会话历史中截取给 LLM 的最大消息条数
pub(super) const SESSION_HISTORY_MESSAGE_LIMIT: usize = 30;
/// 压缩后保留的最新消息条数
pub(super) const COMPACT_KEEP_MESSAGE_LIMIT: usize = 16;
/// 最近查询结果的 TTL（秒）
pub(super) const LAST_QUERY_TTL_SECONDS: i64 = crate::runtime::session::LAST_QUERY_TTL_SECONDS;

/// 判断一条“最近查询”记录是否仍在有效期内（created_at 为 RFC3339，TTL 单位为秒）。
pub(super) fn query_is_fresh(created_at: &str, ttl_seconds: i64) -> bool {
    crate::runtime::session::query_is_fresh(created_at, ttl_seconds)
}

/// 构造一个空的 `RespondRequest`，各字段均为默认值。
///
/// 主要用于在内部 flow 组装请求时，通过 `..empty_respond_request()` 填充剩余字段。
pub(super) fn empty_respond_request() -> RespondRequest {
    RespondRequest {
        session_id: String::new(),
        model: None,
        max_output_tokens: None,
        reasoning_effort: None,
        purpose: RespondPurpose::Chat,
        user_text: String::new(),
        content: String::new(),
        input_parts: Vec::new(),
        quoted: None,
        tools_visible_snapshot: None,
        scope_key: String::new(),
        user_id: None,
        group_member_role: None,
        group_id: None,
        guild_id: None,
        channel_id: None,
        message_id: None,
        timestamp: None,
        platform: String::new(),
        account_id: None,
        event_type: String::new(),
        system_prompts: Vec::new(),
        memory_context: String::new(),
        knowledge_context: String::new(),
        session_context: String::new(),
        history_messages: Vec::new(),
        session: Value::Null,
        metadata: HashMap::new(),
    }
}

/// 构造会话指令的响应（如 /new, /clear, /help 等）。
///
/// 固定设置 `handled = true`，`metrics.provider = "rust"`，
/// `metrics.model = "session-command"` 以区分于 LLM 调用。
pub(super) fn command_response(
    body: impl Into<CommandBody>,
    session_id: Option<String>,
    command: Option<impl Into<String>>,
) -> RespondResponse {
    command_response_with_stream(body, session_id, command, false)
}

/// 构造会话指令或流式查询使用的统一响应。
///
/// `stream` 仅用于指标，不改变用户可见输出；流式查询会传 `true`。
pub(super) fn command_response_with_stream(
    body: impl Into<CommandBody>,
    session_id: Option<String>,
    command: Option<impl Into<String>>,
    stream: bool,
) -> RespondResponse {
    let body = body.into();
    RespondResponse {
        ok: true,
        text: Some(body.text),
        markdown: body.markdown,
        handled: Some(true),
        session_id,
        command: command.map(Into::into),
        diagnostics: Some(json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": false,
            "used_search": false,
        })),
        metrics: LlmMetrics {
            provider: "rust".to_owned(),
            model: "session-command".to_owned(),
            stream,
            ttfe_ms: None,
            ttft_ms: None,
            total_latency_ms: 0,
        },
        usage: None,
        error: None,
        tools_visible_snapshot: None,
    }
}

/// 将 `SessionError` 转换为统一的 `LlmError`。
pub(super) fn session_error(err: crate::runtime::session::SessionError) -> LlmError {
    LlmError::new(
        err.code().to_owned(),
        format!("session store failed: {}", err.message()),
        "session",
    )
}

/// 将 `MemoryError` 转换为统一的 `LlmError`。
pub(super) fn memory_error(err: crate::runtime::memory::MemoryError) -> LlmError {
    LlmError::new(
        err.code().to_owned(),
        format!("memory store failed: {}", err.message()),
        "memory",
    )
}

/// 将 `TodoError` 转换为统一的 `LlmError`。
pub(super) fn todo_error(err: crate::runtime::todo::TodoError) -> LlmError {
    LlmError::new(
        err.code().to_owned(),
        format!("todo store failed: {}", err.message()),
        "todo",
    )
}

/// 将 `RssStoreError` 转换为统一的 `LlmError`。
pub(super) fn rss_error(err: crate::runtime::rss::RssStoreError) -> LlmError {
    LlmError::new(
        err.code().to_owned(),
        format!("rss store failed: {}", err.message()),
        "rss",
    )
}

/// 将一组键值对合并到已有元数据中，跳过空值。
pub(super) fn merge_metadata(
    mut metadata: HashMap<String, String>,
    values: &[(&str, &str)],
) -> HashMap<String, String> {
    for (key, value) in values {
        if !value.trim().is_empty() {
            metadata.insert((*key).to_owned(), (*value).to_owned());
        }
    }
    metadata
}

/// 从会话状态的 `state` JSON 中读取指定 key 的字符串值，并清理空白。
pub(super) fn state_string(session: &SessionRecord, key: &str) -> Option<String> {
    session
        .state
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| clean_string(value.to_owned()))
}

/// 去除字符串两端空白，若结果为空则返回 None。
pub(super) fn clean_string(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

/// 从模型输出中尽力抽取一个 JSON 对象：
/// 1) 先尝试整体 parse；
/// 2) 失败后剥离最外层 ```code fence``` 再 parse；
/// 3) 仍失败则截取首个 `{` 到最后一个 `}` 的片段 parse。
pub(super) fn extract_json_object(raw: &str) -> Option<Value> {
    let text = raw.trim();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    if let Some(fenced) = strip_outer_json_fence(text)
        && let Ok(value) = serde_json::from_str::<Value>(fenced)
    {
        return Some(value);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if start >= end {
        return None;
    }
    serde_json::from_str::<Value>(&text[start..=end]).ok()
}

fn strip_outer_json_fence(text: &str) -> Option<&str> {
    let text = text.trim();
    if !text.starts_with("```") {
        return None;
    }
    let body_start = text.find('\n')? + 1;
    let body = &text[body_start..];
    let fence_end = body.rfind("```")?;
    Some(body[..fence_end].trim())
}
