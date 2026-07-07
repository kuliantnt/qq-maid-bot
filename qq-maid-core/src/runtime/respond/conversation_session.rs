//! Conversation session 协作服务。
//!
//! 本模块负责聊天历史向 LLM 消息的转换，以及聊天完成后的异步标题生成。
//! 它只操作 conversation session，不处理群内 actor-aware interaction 状态。

use crate::{
    provider::types::{ChatMessage, ChatRole},
    runtime::session::{DEFAULT_SESSION_TITLE, SessionRecord},
};

use super::{RustRespondService, title::generate_session_title};

impl RustRespondService {
    /// 如果会话标题还是默认值，且用户消息轮数在 2~4 之间，则后台尝试生成标题。
    ///
    /// 主聊天回复已经完成落库，标题只是展示增强；不能让标题模型的慢响应、
    /// 失败或取消影响本轮 `Completed`。后台任务只允许条件更新标题，不能保存
    /// 旧的完整会话快照，否则会覆盖期间继续写入的历史、pending 或手工重命名。
    pub(super) fn schedule_auto_title(&self, session: SessionRecord) {
        let Some(title_model) = self.title_model.clone() else {
            return;
        };
        if session.title != DEFAULT_SESSION_TITLE {
            return;
        }
        let user_message_count = session
            .history
            .iter()
            .filter(|message| message.role == "user" && !message.content.trim().is_empty())
            .count();
        if !(2..=4).contains(&user_message_count) {
            return;
        }

        let provider = self.provider.clone();
        let session_store = self.session_store.clone();
        let session_id = session.session_id.clone();
        let history = session.history.clone();
        tokio::spawn(async move {
            match generate_session_title(provider.as_ref(), &title_model, &history, false).await {
                Ok(title) => {
                    match session_store.update_title_if_current(
                        &session_id,
                        DEFAULT_SESSION_TITLE,
                        &title,
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::debug!(
                                session_id = %session_id,
                                "generated session title ignored because current title changed"
                            );
                        }
                        Err(err) => {
                            tracing::warn!(
                                error = %err.message(),
                                session_id = %session_id,
                                "failed to save generated session title"
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        session_id = %session_id,
                        "session auto title generation failed"
                    );
                }
            }
        });
    }
}

/// 从会话历史中截取最近的 N 条消息，转换为 LLM `ChatMessage` 格式。
///
/// 仅保留 user 和 assistant 角色，按时间正序返回。
pub(super) fn recent_session_messages(session: &SessionRecord, limit: usize) -> Vec<ChatMessage> {
    session
        .history
        .iter()
        .rev()
        .filter_map(|message| match message.role.as_str() {
            "user" => Some(ChatMessage {
                role: ChatRole::User,
                content: message.content.clone(),
                content_parts: Vec::new(),
            }),
            "assistant" => Some(ChatMessage {
                role: ChatRole::Assistant,
                content: message.content.clone(),
                content_parts: Vec::new(),
            }),
            _ => None,
        })
        .filter(|message| !message.content.trim().is_empty())
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}
