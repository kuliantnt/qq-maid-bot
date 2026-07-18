//! 请求 / 响应类型定义。
//!
//! 提供 `RespondRequest`、`RespondResponse`、`ChatResponse` 以及
//! `RespondPurpose` 等核心数据类型，用于在 HTTP facade 层与 Rust 内部
//! 各子模块之间传递请求与响应。

use std::collections::HashMap;

use qq_maid_llm::provider::types::{ChatMessage, ReasoningEffort, TokenUsage};

use crate::{error::ErrorInfo, service::VisibleEntitySnapshot, util::metrics::LlmMetrics};
use qq_maid_common::{
    identity_context::{ConversationKind, IdentitySource, MessageContext},
    input_part::{MessageInputPart, QuotedMessageContext},
};
use serde::{Deserialize, Serialize};

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
    /// 请求级输出预算覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// 请求级推理强度。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// 业务用途（聊天 / 记忆草稿 / 待办解析 / 压缩）
    #[serde(default)]
    pub purpose: RespondPurpose,
    /// 用户消息文本（优先于 content）
    #[serde(default)]
    pub user_text: String,
    /// 原始消息内容（当 user_text 为空时作为 fallback）
    #[serde(default)]
    pub content: String,
    /// 当前用户输入的有序内容块。为空时按旧纯文本消息兼容。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_parts: Vec<MessageInputPart>,
    /// 当前消息引用 / 回复的上下文，由 Gateway 归一化，Core 负责组装为 LLM 可见上下文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted: Option<QuotedMessageContext>,
    /// LLM 可见的当前消息身份上下文，不作为权限、owner 或 session scope 来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_context: Option<MessageContext>,
    /// 引用消息绑定的工具可见实体快照。仅服务端内部用于 Tool selection scope，
    /// 不进入模型 prompt，也不作为用户可见响应序列化。
    #[serde(default, skip)]
    pub visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    /// 作用域键，用于隔离不同群 / 频道的会话
    #[serde(default)]
    pub scope_key: String,
    /// 服务端归一化的会话类型，不从 scope 或模型可见上下文反推。
    #[serde(default)]
    pub conversation_kind: ConversationKind,
    /// 服务端归一化的原始会话目标 ID；不是投递凭据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// actor-aware interaction 作用域；为空时 ToolContext 退回 conversation scope。
    #[serde(default)]
    pub interaction_scope_key: String,
    /// 用户 ID
    #[serde(default)]
    pub user_id: Option<String>,
    /// 用户稳定 ID 的服务端权威来源；强权限不能接受 LegacyFallback / TextWeak。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_identity_source: Option<IdentitySource>,
    /// 群成员角色，仅群聊请求使用。缺失时群管理类操作按无权限处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_member_role: Option<String>,
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
    /// 平台账号/机器人账号标识；只参与业务隔离键，不作为发送目标。
    #[serde(default)]
    pub account_id: Option<String>,
    /// 事件类型（如 "message"）
    #[serde(default)]
    pub event_type: String,
    /// 系统提示词列表
    #[serde(default)]
    pub system_prompts: Vec<String>,
    /// 长期记忆上下文
    #[serde(default)]
    pub memory_context: String,
    /// 本轮本地知识检索上下文，仅普通聊天使用，不会写入长期记忆。
    #[serde(default)]
    pub knowledge_context: String,
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

    /// 获取可参与确定性命令识别的用户实际正文。
    ///
    /// Gateway 可能把平台语音转写作为用户角色的补充文本传入 LLM；这类合成文本
    /// 不能越过“仅当前消息正文可触发命令”的边界。旧纯文本请求没有 input parts，
    /// 继续回退到既有文本字段，保持内部调用兼容。
    pub fn effective_command_text(&self) -> String {
        if self.input_parts.is_empty() {
            return self.effective_user_text();
        }
        self.input_parts
            .iter()
            .filter_map(|part| match part {
                MessageInputPart::Text {
                    text,
                    source:
                        None
                        | Some(qq_maid_common::input_part::TextSource::Body)
                        | Some(qq_maid_common::input_part::TextSource::Caption),
                } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// LLM 可见的有序内容块；旧纯文本请求自动转换为单个 text part。
    pub fn effective_input_parts(&self) -> Vec<MessageInputPart> {
        if !self.input_parts.is_empty() {
            return self.input_parts.clone();
        }
        let text = self.effective_user_text();
        if text.trim().is_empty() {
            Vec::new()
        } else {
            vec![MessageInputPart::text(text)]
        }
    }

    pub fn has_non_text_input_parts(&self) -> bool {
        self.input_parts.iter().any(MessageInputPart::is_non_text)
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
            max_output_tokens: None,
            reasoning_effort: None,
            purpose: RespondPurpose::Chat,
            user_text: String::new(),
            content: String::new(),
            input_parts: Vec::new(),
            quoted: None,
            message_context: None,
            visible_entity_snapshot: None,
            scope_key: String::new(),
            conversation_kind: ConversationKind::Unknown,
            conversation_id: None,
            interaction_scope_key: String::new(),
            user_id: None,
            user_identity_source: None,
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
    /// 当前回复绑定的工具可见实体快照。该字段只供 Gateway 写入引用索引，
    /// 序列化时跳过，避免内部实体 ID 暴露到 HTTP/诊断面。
    #[serde(default, skip)]
    pub visible_entity_snapshot: Option<VisibleEntitySnapshot>,
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
            visible_entity_snapshot: None,
        }
    }
}
