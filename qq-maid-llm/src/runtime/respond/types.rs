//! 请求 / 响应类型定义。
//!
//! 提供 `RespondRequest`、`RespondResponse`、`ChatResponse` 以及
//! `RespondPurpose` 等核心数据类型，用于在 HTTP facade 层与 Rust 内部
//! 各子模块之间传递请求与响应。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{
    error::ErrorInfo,
    provider::types::{ChatMessage, TokenUsage},
    util::metrics::LlmMetrics,
};

/// 请求用途标记，用于区分当前请求的业务语义。
///
/// 不同的 `RespondPurpose` 决定了 LLM 请求的消息组装策略
/// （见 `llm_service::build_respond_messages`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RespondPurpose {
    /// 普通聊天
    #[default]
    Chat,
    /// 长期记忆草稿抽取
    MemoryDraft,
    /// 待办事项结构化解析
    TodoParse,
    /// 会话上下文压缩
    Compact,
}

/// 聊天 / 功能请求。
///
/// 承载来自 HTTP facade 或内部子 flow 的所有参数，
/// 包括用户输入、会话上下文、系统提示词等。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RespondRequest {
    /// 会话 ID，用于关联历史对话
    #[serde(default)]
    pub session_id: String,
    /// 内部调用可按业务用途指定模型；外部 HTTP facade 不反序列化这个字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 业务用途（聊天 / 记忆草稿 / 待办解析 / 压缩）
    #[serde(default)]
    pub purpose: RespondPurpose,
    /// 用户消息文本（优先于 content）
    #[serde(default)]
    pub user_text: String,
    /// 原始消息内容（当 user_text 为空时作为 fallback）
    #[serde(default)]
    pub content: String,
    /// 作用域键，用于隔离不同群 / 频道的会话
    #[serde(default)]
    pub scope_key: String,
    /// 用户 ID
    #[serde(default)]
    pub user_id: Option<String>,
    /// 群组 ID
    #[serde(default)]
    pub group_id: Option<String>,
    /// 频道 / 服务器 ID
    #[serde(default)]
    pub guild_id: Option<String>,
    /// 子频道 / 私聊通道 ID
    #[serde(default)]
    pub channel_id: Option<String>,
    /// 消息 ID
    #[serde(default)]
    pub message_id: Option<String>,
    /// 消息时间戳
    #[serde(default)]
    pub timestamp: Option<String>,
    /// 平台标识（如 "qq"）
    #[serde(default)]
    pub platform: String,
    /// 事件类型（如 "message"）
    #[serde(default)]
    pub event_type: String,
    /// 系统提示词列表
    #[serde(default)]
    pub system_prompts: Vec<String>,
    /// 长期记忆上下文
    #[serde(default)]
    pub memory_context: String,
    /// 会话状态上下文
    #[serde(default)]
    pub session_context: String,
    /// 最近 N 条历史消息
    #[serde(default)]
    pub history_messages: Vec<ChatMessage>,
    /// 当前会话的完整序列化状态（用于压缩、待办修订等场景）
    #[serde(default)]
    pub session: serde_json::Value,
    /// 附加元数据（memory_operation、todo_operation 等）
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl RespondRequest {
    /// 获取有效的用户输入文本。
    ///
    /// 优先返回 `user_text`；若为空则 fallback 到 `content`。
    pub fn effective_user_text(&self) -> String {
        let user_text = self.user_text.trim();
        if !user_text.is_empty() {
            return self.user_text.clone();
        }
        self.content.clone()
    }

    /// 判断是否为"标准"消息（具有 scope_key 或 content）。
    pub fn is_standard_message(&self) -> bool {
        !self.scope_key.trim().is_empty() || !self.content.trim().is_empty()
    }
}

impl Default for RespondRequest {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            model: None,
            purpose: RespondPurpose::Chat,
            user_text: String::new(),
            content: String::new(),
            scope_key: String::new(),
            user_id: None,
            group_id: None,
            guild_id: None,
            channel_id: None,
            message_id: None,
            timestamp: None,
            platform: String::new(),
            event_type: String::new(),
            system_prompts: Vec::new(),
            memory_context: String::new(),
            session_context: String::new(),
            history_messages: Vec::new(),
            session: serde_json::Value::Null,
            metadata: HashMap::new(),
        }
    }
}

/// LLM 聊天的原始响应。
///
/// 与 `RespondResponse` 的区别：`ChatResponse` 包含 LLM 返回的原始 `reply`
/// 和 Token 用量等信息，供调用方进一步加工后再组装成 `RespondResponse`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatResponse {
    /// 是否成功
    pub ok: bool,
    /// LLM 原始回复内容
    pub reply: Option<String>,
    /// 调用指标（模型名、延迟等）
    pub metrics: LlmMetrics,
    /// Token 用量统计
    pub usage: Option<TokenUsage>,
    /// 错误信息（ok 为 false 时存在）
    pub error: Option<ErrorInfo>,
}

/// 统一的响应结构。
///
/// 所有路由分派最终都返回 `RespondResponse`。
/// `text` 是对 `ChatResponse.reply` 进一步加工后的展示文本
/// （如去除 Markdown 格式、翻译等），供 HTTP facade 直接发送给用户。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RespondResponse {
    /// 是否成功
    pub ok: bool,
    /// 纯文本正文，也是未启用 Markdown 或发送失败时的兼容 fallback。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 结构化 Markdown 正文；仅在需要保留排版时返回。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    /// 是否已被某个子 flow 处理
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handled: Option<bool>,
    /// 关联的会话 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 匹配到的指令名（如 "new", "help"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// 诊断信息（后端类型、是否使用记忆 / 搜索等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<serde_json::Value>,
    /// 调用指标
    pub metrics: LlmMetrics,
    /// Token 用量统计
    pub usage: Option<TokenUsage>,
    /// 错误信息
    pub error: Option<ErrorInfo>,
}

/// `/v1/respond` 的传输结果。
///
/// 默认仍然返回完整 JSON；当命中流式查询路径时，HTTP 层会改为 SSE。
#[derive(Debug)]
pub enum RespondTransport {
    /// 一次性 JSON 响应。
    Json(Box<RespondResponse>),
    /// SSE 流式响应。
    Stream(RespondStream),
}

/// SSE 流式响应的事件。
#[derive(Debug, Clone)]
pub enum RespondStreamEvent {
    /// 流式增量文本。
    Delta { text: String },
    /// 结束事件，携带完整响应。
    Final { response: Box<RespondResponse> },
}

/// SSE 流式响应载体。
#[derive(Debug)]
pub struct RespondStream {
    /// 从后台任务接收流式事件。
    pub receiver: mpsc::Receiver<RespondStreamEvent>,
}

impl ChatResponse {
    /// 构造成功响应。
    pub fn ok(reply: impl Into<String>, metrics: LlmMetrics, usage: Option<TokenUsage>) -> Self {
        Self {
            ok: true,
            reply: Some(reply.into()),
            metrics,
            usage,
            error: None,
        }
    }

    /// 构造错误响应。
    pub fn error(metrics: LlmMetrics, error: ErrorInfo) -> Self {
        Self {
            ok: false,
            reply: None,
            metrics,
            usage: None,
            error: Some(error),
        }
    }
}

impl RespondResponse {
    /// 从 `ChatResponse` 构造统一的响应。
    pub fn from_chat(chat: ChatResponse, text: Option<String>, markdown: Option<String>) -> Self {
        Self {
            ok: chat.ok,
            text,
            markdown,
            handled: Some(chat.ok),
            session_id: None,
            command: None,
            diagnostics: None,
            metrics: chat.metrics,
            usage: chat.usage,
            error: chat.error,
        }
    }
}
