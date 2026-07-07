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
    pub tools_visible_snapshot: Option<ToolsVisibleSnapshot>,
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
pub struct ToolsVisibleSnapshot {
    pub platform: String,
    pub account_id: Option<String>,
    pub scope_key: String,
    pub owner_key: Option<String>,
    pub items: Vec<ToolsVisibleItem>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolsVisibleItem {
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
    pub text: Option<String>,
    pub markdown: Option<String>,
    pub handled: Option<bool>,
    pub session_id: Option<String>,
    pub command: Option<String>,
    pub diagnostics: Option<serde_json::Value>,
    pub tools_visible_snapshot: Option<ToolsVisibleSnapshot>,
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
