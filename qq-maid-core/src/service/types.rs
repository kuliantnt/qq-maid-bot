use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use qq_maid_common::{
    identity_context::{ConversationContext, MentionIdentity, MessageActorContext, MessageContext},
    input_part::{MessageInputPart, QuotedMessageContext},
};
use tokio::sync::mpsc;

use crate::identity::conversation_scope_key;

use super::UpstreamStatusSnapshot;

#[async_trait]
pub trait CoreService: Send + Sync {
    async fn respond(&self, request: CoreRequest) -> Result<CoreRespondOutput, CoreError>;

    async fn classify_inbound(
        &self,
        request: CoreRequest,
    ) -> Result<CoreInboundClassification, CoreError>;

    async fn upstream_check(&self) -> Result<(), CoreError>;

    fn health_snapshot(&self) -> CoreHealthSnapshot;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreRequest {
    pub text: String,
    pub input_parts: Vec<MessageInputPart>,
    pub quoted: Option<QuotedMessageContext>,
    pub visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    pub platform: Platform,
    pub account_id: Option<String>,
    pub actor: CoreActor,
    pub mentions: Vec<MentionIdentity>,
    pub conversation: CoreConversation,
}

/// 工具输出绑定到出站消息的通用可见实体快照。
///
/// Gateway 只负责按消息引用索引保存和回填本结构，不理解具体业务域。
/// Core 内各 Tool 消费自己认识的 `domain`，例如 Todo Tool 使用 `todo` 项把
/// visible number 映射回服务端内部实体 ID。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleEntitySnapshot {
    pub platform: String,
    pub account_id: Option<String>,
    pub scope_key: String,
    pub owner_key: Option<String>,
    pub items: Vec<VisibleEntityItem>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleEntityItem {
    pub domain: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub visible_number: usize,
    pub label: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreInboundClassification {
    pub kind: CoreInboundKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreInboundKind {
    NormalChat,
    Immediate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    QqOfficial,
    OneBot,
    WechatService,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreActor {
    /// 当前消息的实际操作者。群聊中它只表示发言人，不参与 conversation scope 拆分。
    pub user_id: Option<String>,
    pub union_id: Option<String>,
    pub display_name: Option<String>,
    pub group_member_role: Option<CoreGroupMemberRole>,
    pub is_bot: bool,
    pub identity_source: qq_maid_common::identity_context::IdentitySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreGroupMemberRole {
    Owner,
    Admin,
    Member,
    Unknown,
}

impl CoreGroupMemberRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreConversation {
    /// 私聊 conversation scope，session / pending / ref_index 等会话状态以该空间隔离。
    Private { peer_id: String },
    /// 群聊 conversation scope，按群空间隔离；群内个人状态需再叠加 actor/owner scope。
    Group { group_id: String },
    ServiceAccount {
        account_id: Option<String>,
        peer_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreResponse {
    /// 结构化出站内容，Core → Gateway 完整回复的正式契约。
    ///
    /// Gateway 出站渲染、ref_index 文本回填、流式收尾等读取用户可见正文时，
    /// 应优先通过 [`CoreResponse::text_content`] / [`CoreResponse::markdown_content`]
    /// 访问，而不是直接读下方的兼容字段。
    pub output: Option<AssistantOutput>,
    /// 兼容字段：与 `output.text_fallback` 保持同步。
    ///
    /// 仅为过渡期保留，覆盖 Core 内部 `RespondResponse` 仍按 text/markdown 双通道
    /// 组装正文、以及部分测试断言的旧路径。待 Core 内部 flow 全量迁移到
    /// `AssistantOutput` 且无测试直接断言后可删除。
    pub text: Option<String>,
    /// 兼容字段：与 `output.markdown` 保持同步，删除条件同 `text`。
    pub markdown: Option<String>,
    pub handled: Option<bool>,
    pub session_id: Option<String>,
    pub command: Option<String>,
    pub diagnostics: Option<serde_json::Value>,
    pub visible_entity_snapshot: Option<VisibleEntitySnapshot>,
}

/// 平台无关的助手出站内容模型。
///
/// `text_fallback` 是所有平台都可降级发送的纯文本；`markdown` 保留现有 Markdown
/// 通道兼容；`parts` 为后续图片、文件、卡片等结构化出站内容预留顺序化载体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantOutput {
    pub text_fallback: String,
    pub markdown: Option<String>,
    pub parts: Vec<OutputPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputPart {
    Text { text: String },
    Markdown { markdown: String },
    Image { media: OutputMedia },
    File { media: OutputMedia },
}

/// 出站媒体占位信息。
///
/// 第一阶段只作为 Core 内结构化输出契约，不要求 Gateway 立即接入发送。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputMedia {
    pub mime_type: Option<String>,
    pub filename: Option<String>,
    pub size_bytes: Option<u64>,
    pub url: Option<String>,
    pub media_id: Option<String>,
    pub file_id: Option<String>,
    pub platform: Option<String>,
    pub fallback_text: Option<String>,
}

#[derive(Debug)]
pub enum CoreRespondOutput {
    Complete(Box<CoreResponse>),
    Stream(CoreResponseStream),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreOutputPolicy {
    DirectStream,
    CompleteThenSend,
    CompleteToolLoop,
    ProgressThenComplete,
    ProgressThenStream,
}

impl CoreOutputPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectStream => "direct_stream",
            Self::CompleteThenSend => "ordinary_complete",
            Self::CompleteToolLoop => "complete_tool_loop",
            Self::ProgressThenComplete => "progress_then_complete",
            Self::ProgressThenStream => "progress_then_stream",
        }
    }
}

#[derive(Debug)]
pub struct CoreResponseStream {
    pub(super) receiver: mpsc::Receiver<CoreResponseEvent>,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) output_policy: CoreOutputPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreResponseEvent {
    /// 系统生成的受控状态提示。
    ///
    /// 该事件只用于工具/长任务进度，不承载模型中间推理、工具参数或工具结果原文；
    /// Gateway 可以按平台策略选择记录、忽略或限流发送，最终回复仍只能由 `Completed` 收口。
    Status(CoreResponseStatus),
    /// 用户可见的最终文本增量。
    ///
    /// Tool Loop 路径只会在工具循环完成、业务校验通过并生成最终回复后发送该事件；
    /// 工具参数、工具结果原文和模型中间候选文本不得通过此事件外发。
    TextDelta(String),
    Completed(Box<CoreResponse>),
    Failed(CoreRespondFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreResponseStatus {
    pub kind: CoreResponseStatusKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreResponseStatusKind {
    ToolLoopStarted,
    ToolLoopRunning,
    ToolLoopFinalizing,
}

impl CoreResponseStatusKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolLoopStarted => "tool_loop_started",
            Self::ToolLoopRunning => "tool_loop_running",
            Self::ToolLoopFinalizing => "tool_loop_finalizing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreRespondFailure {
    pub kind: CoreFailureKind,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreFailureKind {
    SearchTimeout,
    SearchFailed,
    LlmTimeout,
    LlmFailed,
    Cancelled,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreHealthSnapshot {
    pub ok: bool,
    pub provider: String,
    pub model: String,
    pub stream: bool,
    pub upstream: UpstreamStatusSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}@{stage}: {message}")]
pub struct CoreError {
    pub code: String,
    pub stage: String,
    pub message: String,
}

impl CoreRespondOutput {
    pub fn output_policy(&self) -> CoreOutputPolicy {
        match self {
            Self::Complete(_) => CoreOutputPolicy::CompleteThenSend,
            Self::Stream(stream) => stream.output_policy(),
        }
    }
}

impl CoreResponse {
    pub fn with_output(mut self, output: AssistantOutput) -> Self {
        self.text = Some(output.text_fallback.clone());
        self.markdown = output.markdown.clone();
        self.output = Some(output);
        self
    }

    /// 用户可见文本 fallback，优先取结构化 `AssistantOutput::text_fallback`。
    ///
    /// Gateway 出站 / ref_index 回填 / 流式收尾 / 日志等需要读取最终正文的路径应统一
    /// 走本访问器，不再直接读 `text` 兼容字段，避免 Core 内部迁移到 `AssistantOutput`
    /// 后出现两套数据源。`text` 字段仅在 `output` 缺失（旧兼容路径）时兜底。
    pub fn text_content(&self) -> Option<&str> {
        self.output
            .as_ref()
            .map(|output| output.text_fallback.as_str())
            .or(self.text.as_deref())
    }

    /// 用户可见 Markdown 正文，优先取结构化 `AssistantOutput::markdown`。
    ///
    /// 与 [`Self::text_content`] 对应；`markdown` 兼容字段仅在 `output` 缺失或其
    /// `markdown` 为 `None` 时兜底。
    pub fn markdown_content(&self) -> Option<&str> {
        self.output
            .as_ref()
            .and_then(|output| output.markdown.as_deref())
            .or(self.markdown.as_deref())
    }
}

impl AssistantOutput {
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            text_fallback: text.clone(),
            markdown: None,
            parts: non_empty_output_parts([OutputPart::Text { text }]),
        }
    }

    pub fn markdown(text_fallback: impl Into<String>, markdown: impl Into<String>) -> Self {
        let text_fallback = text_fallback.into();
        let markdown = markdown.into();
        let mut parts = Vec::new();
        if !markdown.trim().is_empty() {
            parts.push(OutputPart::Markdown {
                markdown: markdown.clone(),
            });
        }
        if parts.is_empty() && !text_fallback.trim().is_empty() {
            parts.push(OutputPart::Text {
                text: text_fallback.clone(),
            });
        }
        Self {
            text_fallback,
            markdown: Some(markdown),
            parts,
        }
    }

    pub fn from_compat_fields(text: Option<&str>, markdown: Option<&str>) -> Option<Self> {
        let text = text?.to_owned();
        let markdown = markdown.map(str::to_owned);
        Some(match markdown {
            Some(markdown) => Self::markdown(text, markdown),
            None => Self::text(text),
        })
    }
}

impl OutputPart {
    fn is_empty(&self) -> bool {
        match self {
            Self::Text { text } => text.trim().is_empty(),
            Self::Markdown { markdown } => markdown.trim().is_empty(),
            Self::Image { .. } | Self::File { .. } => false,
        }
    }
}

fn non_empty_output_parts(parts: impl IntoIterator<Item = OutputPart>) -> Vec<OutputPart> {
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

impl CoreRequest {
    /// 生成 conversation scope。
    ///
    /// 该值是业务隔离键，不是平台发送地址；Gateway 回复和 Core 主动推送必须使用显式的
    /// ReplyTarget / DeliveryTarget / PushTarget。
    pub fn scope_key(&self) -> String {
        match &self.conversation {
            CoreConversation::Private { peer_id } => conversation_scope_key(
                self.platform.as_str(),
                self.account_id.as_deref(),
                "private",
                peer_id,
            ),
            CoreConversation::Group { group_id } => conversation_scope_key(
                self.platform.as_str(),
                self.account_id.as_deref(),
                "group",
                group_id,
            ),
            CoreConversation::ServiceAccount {
                account_id,
                peer_id,
            } => conversation_scope_key(
                self.platform.as_str(),
                self.account_id.as_deref().or(account_id.as_deref()),
                "private",
                peer_id,
            ),
        }
    }

    /// 由权威字段（actor / mentions / conversation / platform / account_id）派生 LLM 可见
    /// `MessageContext`（#319 收敛）。Gateway 不再单独构造一份 message_context，避免双份数据源
    /// 不一致；HTTP / facade 路径也由此自动获得 message_context。
    pub fn message_context(&self) -> MessageContext {
        let actor = MessageActorContext {
            user_id: self.actor.user_id.clone(),
            union_id: self.actor.union_id.clone(),
            display_name: self.actor.display_name.clone(),
            display_name_source: self
                .actor
                .display_name
                .as_ref()
                .map(|_| self.actor.identity_source.as_str().to_owned()),
            group_member_role: self
                .actor
                .group_member_role
                .map(|role| role.as_str().to_owned()),
            is_bot: Some(self.actor.is_bot),
            source: self.actor.identity_source,
        };
        let (kind, id) = match &self.conversation {
            CoreConversation::Private { peer_id } => ("private", peer_id.clone()),
            CoreConversation::Group { group_id } => ("group", group_id.clone()),
            CoreConversation::ServiceAccount { peer_id, .. } => {
                ("service_account", peer_id.clone())
            }
        };
        MessageContext {
            actor: Some(actor),
            mentions: self.mentions.clone(),
            conversation: ConversationContext {
                kind: kind.to_owned(),
                id: Some(id),
                platform: Some(self.platform.as_str().to_owned()),
                account_id: self.account_id.clone(),
            },
        }
    }
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QqOfficial => "qq_official",
            Self::OneBot => "onebot",
            Self::WechatService => "wechat_service",
        }
    }
}

impl CoreResponseStream {
    pub async fn recv(&mut self) -> Option<CoreResponseEvent> {
        self.receiver.recv().await
    }

    pub fn output_policy(&self) -> CoreOutputPolicy {
        self.output_policy
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Drop for CoreResponseStream {
    fn drop(&mut self) {
        self.cancel();
    }
}
