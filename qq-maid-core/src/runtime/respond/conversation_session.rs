//! Conversation session 协作服务。
//!
//! 本模块负责聊天历史向 LLM 消息的转换，以及聊天完成后的异步标题生成。
//! 它只操作 conversation session，不处理群内 actor-aware interaction 状态。

use qq_maid_llm::provider::types::{ChatMessage, ChatRole};

use crate::runtime::session::{
    DEFAULT_SESSION_TITLE, SessionMessage, SessionRecord, SessionTurnActor,
};

use super::{RustRespondService, title::generate_session_title};

impl RustRespondService {
    /// 如果会话标题还是默认值，且用户消息轮数在 2~4 之间，则后台尝试生成标题。
    ///
    /// 主聊天回复已经完成落库，标题只是展示增强；不能让标题模型的慢响应、
    /// 失败或取消影响本轮 `Completed`。后台任务只允许条件更新标题，不能保存
    /// 旧的完整会话快照，否则会覆盖期间继续写入的历史、pending 或手工重命名。
    pub(super) fn schedule_auto_title(&self, session: SessionRecord, title_model: Option<String>) {
        let Some(title_model) = title_model else {
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
    let actor_aware = session.scope == "group";
    session
        .history
        .iter()
        .rev()
        .filter(|message| !message.content.trim().is_empty())
        .filter_map(|message| match message.role.as_str() {
            "user" => Some(ChatMessage {
                role: ChatRole::User,
                content: render_session_message_for_model(message, actor_aware),
                content_parts: Vec::new(),
            }),
            "assistant" => Some(ChatMessage {
                role: ChatRole::Assistant,
                content: render_session_message_for_model(message, actor_aware),
                content_parts: Vec::new(),
            }),
            _ => None,
        })
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// 把 Session 原始正文转换为模型可见文本。
///
/// actor 标记只用于共享群聊历史；私聊保持原文，避免单用户会话出现多余前缀。
pub(super) fn render_session_message_for_model(
    message: &SessionMessage,
    actor_aware: bool,
) -> String {
    if !actor_aware {
        return message.content.clone();
    }

    let label = match message.role.as_str() {
        "user" => render_turn_actor_label("历史发言人", message.turn_actor.as_ref()),
        "assistant" => render_turn_actor_label("机器人当时回复给", message.turn_actor.as_ref()),
        _ => return message.content.clone(),
    };
    format!("[{label}]\n{}", message.content)
}

fn render_turn_actor_label(label: &str, actor: Option<&SessionTurnActor>) -> String {
    let Some(actor) = actor else {
        return format!("{label}：unknown（旧会话未保存 actor 归属，不得视为当前发言人）");
    };
    let actor_ref = actor.actor_ref.as_deref().unwrap_or("unknown");
    let display_name = actor.display_name.as_deref().unwrap_or("unknown");
    let display_name_source = actor.display_name_source.as_deref().unwrap_or("unknown");
    let group_member_role = actor.group_member_role.as_deref().unwrap_or("unknown");
    let identity_source = actor.identity_source.as_deref().unwrap_or("unknown");
    let missing_stable_ref = actor
        .actor_ref
        .is_none()
        .then_some("，当时缺少稳定 actor 标识，不得仅按展示名判断与其他消息是否属于同一人");
    format!(
        "{label}：actor_ref={actor_ref}，展示名={display_name}，展示名来源={display_name_source}，群角色={group_member_role}，身份来源={identity_source}{}",
        missing_stable_ref.unwrap_or("")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str, actor_ref: Option<&str>) -> SessionMessage {
        SessionMessage {
            role: role.to_owned(),
            content: content.to_owned(),
            ts: "2026-07-15T10:00:00+08:00".to_owned(),
            turn_actor: actor_ref.map(|actor_ref| SessionTurnActor {
                actor_ref: Some(actor_ref.to_owned()),
                display_name: Some("测试昵称".to_owned()),
                display_name_source: Some("event".to_owned()),
                group_member_role: Some("member".to_owned()),
                identity_source: Some("event".to_owned()),
            }),
        }
    }

    #[test]
    fn group_history_marks_user_and_assistant_with_same_turn_actor() {
        let user = render_session_message_for_model(
            &message("user", "我的昵称是什么", Some("actor_a")),
            true,
        );
        let assistant = render_session_message_for_model(
            &message("assistant", "你的昵称是测试昵称", Some("actor_a")),
            true,
        );

        assert!(user.starts_with("[历史发言人：actor_ref=actor_a"));
        assert!(assistant.starts_with("[机器人当时回复给：actor_ref=actor_a"));
        assert!(user.contains("展示名来源=event"));
        assert!(assistant.contains("展示名来源=event"));
    }

    #[test]
    fn different_group_actors_render_different_stable_refs() {
        let actor_a =
            render_session_message_for_model(&message("user", "A 的消息", Some("actor_a")), true);
        let actor_b =
            render_session_message_for_model(&message("user", "B 的消息", Some("actor_b")), true);

        assert!(actor_a.contains("actor_ref=actor_a"));
        assert!(actor_b.contains("actor_ref=actor_b"));
        assert_ne!(actor_a.lines().next(), actor_b.lines().next());
    }

    #[test]
    fn legacy_group_history_is_unknown_instead_of_current_actor() {
        let rendered = render_session_message_for_model(&message("user", "旧消息", None), true);

        assert!(rendered.starts_with("[历史发言人：unknown"));
        assert!(rendered.contains("不得视为当前发言人"));
    }

    #[test]
    fn private_history_keeps_original_content() {
        let rendered = render_session_message_for_model(
            &message("user", "私聊原文", Some("actor_private")),
            false,
        );

        assert_eq!(rendered, "私聊原文");
    }
}
