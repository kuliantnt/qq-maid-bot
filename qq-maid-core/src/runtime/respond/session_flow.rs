//! 会话管理指令处理流程。
//!
//! 实现 `/new`、`/rename`、`/resume`、`/list`、`/clear`、`/state`、
//! `/compact`、`/help` 等会话管理指令的解析与执行。

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    config::ChatScene,
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        session::{SessionMeta, SessionRecord},
    },
};

use super::{
    RespondPurpose, RespondRequest, RespondResponse, RustRespondService,
    common::{
        COMPACT_KEEP_MESSAGE_LIMIT, clean_string, command_response, empty_respond_request,
        session_error, state_string, structured_command_body, truncate_chars,
    },
    help::{HelpContext, format_help_reply},
    llm_service::{ChatService, LlmChatService},
    title::{context_session_title, display_session_title, generate_session_title},
};

impl RustRespondService {
    /// 处理会话管理指令。
    ///
    /// 根据指令动作分别调用对应的子流程：
    /// - `new` → 创建新会话
    /// - `rename` → 重命名（可自动生成标题）
    /// - `resume` / `list` → 恢复历史会话
    /// - `clear` → 清空上下文
    /// - `state` → 展示当前会话状态
    /// - `compact` → 压缩历史
    /// - `help` → 查看帮助
    pub(super) async fn handle_session_command(
        &self,
        command: ParsedCommand,
        meta: &SessionMeta,
    ) -> Result<RespondResponse, LlmError> {
        match command.action.as_str() {
            "new" => {
                let title = command.argument.trim();
                let session = self
                    .session_store
                    .create(meta, title, true)
                    .map_err(session_error)?;
                Ok(command_response(
                    format!(
                        "新会话已开。{}已经准备好新的上下文，之前的会话仍可通过恢复入口找回。",
                        self.bot_display_name()
                    ),
                    Some(session.session_id),
                    Some("new"),
                ))
            }
            "help" => {
                let session = self
                    .session_store
                    .get_or_create_active(meta)
                    .map_err(session_error)?;
                let scene = if meta
                    .group_id
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                {
                    ChatScene::Group
                } else {
                    ChatScene::Private
                };
                let policy = self.agent_config.resolve(scene)?;
                Ok(command_response(
                    format_help_reply(
                        &command.argument,
                        HelpContext {
                            is_group: matches!(scene, ChatScene::Group),
                            tool_calling_enabled: policy.tool_calling_enabled,
                            group_tool_calling_enabled: policy.group_tool_calling_enabled,
                        },
                    ),
                    Some(session.session_id),
                    Some("help"),
                ))
            }
            "rename" => {
                let title = command.argument.trim();
                if title.is_empty() {
                    let Some(title_model) = self.title_model.as_deref() else {
                        return Ok(command_response(
                            "当前未配置标题生成模型。",
                            None,
                            Some("rename"),
                        ));
                    };
                    let mut session = self
                        .session_store
                        .get_or_create_active(meta)
                        .map_err(session_error)?;
                    match generate_session_title(
                        self.provider.as_ref(),
                        title_model,
                        &session.history,
                        true,
                    )
                    .await
                    {
                        Ok(title) => {
                            session.title = title.clone();
                            session
                                .state
                                .insert("current_topic".to_owned(), Value::String(title.clone()));
                            self.session_store
                                .save(&mut session)
                                .map_err(session_error)?;
                            return Ok(command_response(
                                format!("已重命名为：{title}"),
                                Some(session.session_id),
                                Some("rename"),
                            ));
                        }
                        Err(err) => {
                            tracing::debug!(
                                error = %err,
                                session_id = %session.session_id,
                                "session title generation failed"
                            );
                            return Ok(command_response(
                                "当前内容还不够生成标题，先保持原标题。",
                                Some(session.session_id),
                                Some("rename"),
                            ));
                        }
                    }
                }
                let mut session = self
                    .session_store
                    .get_or_create_active(meta)
                    .map_err(session_error)?;
                session.title = title.to_owned();
                session
                    .state
                    .insert("current_topic".to_owned(), Value::String(title.to_owned()));
                self.session_store
                    .save(&mut session)
                    .map_err(session_error)?;
                Ok(command_response(
                    format!("当前会话已重命名：{title}"),
                    Some(session.session_id),
                    Some("rename"),
                ))
            }
            "clear" => {
                let mut session = self
                    .session_store
                    .get_or_create_active(meta)
                    .map_err(session_error)?;
                session.reset();
                self.session_store
                    .save(&mut session)
                    .map_err(session_error)?;
                Ok(command_response(
                    "当前上下文已清空。桌面收好了，但旧档案没有销毁。",
                    Some(session.session_id),
                    Some("clear"),
                ))
            }
            "state" => {
                let session = self
                    .session_store
                    .get_or_create_active(meta)
                    .map_err(session_error)?;
                Ok(command_response(
                    structured_command_body(format_session_state_reply(
                        &session,
                        self.bot_display_name(),
                    )),
                    Some(session.session_id),
                    Some("state"),
                ))
            }
            "resume" => {
                self.handle_resume_command(&command.argument, meta, false)
                    .await
            }
            "list" => {
                self.handle_resume_command(&command.argument, meta, true)
                    .await
            }
            "compact" => self.handle_compact_command(meta).await,
            _ => Ok(command_response(
                format!("唔，这个会话指令{}暂时不认识。", self.bot_display_name()),
                None,
                Some(command.action),
            )),
        }
    }

    /// 处理 /resume 和 /list 指令。
    ///
    /// - 无参数时列出最近会话列表。
    /// - 带数字参数时恢复指定编号的会话。
    /// - `deprecated_list` 为 true 时对应旧版 /list 指令。
    async fn handle_resume_command(
        &self,
        argument: &str,
        meta: &SessionMeta,
        deprecated_list: bool,
    ) -> Result<RespondResponse, LlmError> {
        let current = self
            .session_store
            .get_or_create_active(meta)
            .map_err(session_error)?;
        let candidates = self
            .session_store
            .list_for_scope(&meta.scope_key, Some(&current.session_id))
            .map_err(session_error)?;
        let argument = argument.trim();

        if deprecated_list && !argument.is_empty() {
            return Ok(command_response(
                "用法：/list",
                Some(current.session_id),
                Some("list"),
            ));
        }

        if argument.is_empty() || deprecated_list {
            let mut reply = format_resume_list(&candidates);
            if deprecated_list {
                reply.push_str("\n\n提示：/list 已不推荐，以后建议使用 /resume 或 /恢复。");
            }
            return Ok(command_response(
                structured_command_body(reply),
                Some(current.session_id),
                Some(if deprecated_list { "list" } else { "resume" }),
            ));
        }

        let Ok(index) = argument.parse::<usize>() else {
            return Ok(command_response(
                "唔，/resume 后面可以写列表里的数字，比如 /resume 1。要看列表可以用 /resume。",
                Some(current.session_id),
                Some("resume"),
            ));
        };

        let Some(restored) = candidates
            .get(index.saturating_sub(1))
            .filter(|_| index > 0)
        else {
            return Ok(command_response(
                "唔，这个编号不在最近会话列表里。可以先用 /resume 看一下。",
                Some(current.session_id),
                Some("resume"),
            ));
        };

        self.session_store
            .set_active_session_id(&meta.scope_key, &restored.session_id)
            .map_err(session_error)?;
        let title = session_title_for_display(restored);
        let reply = format!(
            "已恢复会话：{title}\n\n{}",
            format_session_state_reply(restored, self.bot_display_name())
        );
        Ok(command_response(
            structured_command_body(reply),
            Some(restored.session_id.clone()),
            Some("resume"),
        ))
    }

    /// 处理 /compact 指令。
    ///
    /// 调用 LLM 将当前会话历史压缩为摘要，然后裁剪历史到保留最近 N 条。
    pub(super) async fn handle_compact_command(
        &self,
        meta: &SessionMeta,
    ) -> Result<RespondResponse, LlmError> {
        let mut session = self
            .session_store
            .get_or_create_active(meta)
            .map_err(session_error)?;
        if session.history.is_empty() {
            return Ok(command_response(
                "当前没有可压缩的上下文。桌面本来就是空的。",
                Some(session.session_id),
                Some("compact"),
            ));
        }

        let service = LlmChatService::new(self.provider.clone());
        let output = service
            .respond(RespondRequest {
                session_id: session.session_id.clone(),
                model: self.compact_model.clone(),
                purpose: RespondPurpose::Compact,
                session: serde_json::to_value(&session).unwrap_or_default(),
                metadata: HashMap::from([("purpose".to_owned(), "compact".to_owned())]),
                ..empty_respond_request()
            })
            .await?;
        if output.reply.trim().is_empty() {
            return Ok(command_response(
                format!(
                    "唔，{}刚刚没压缩出可用摘要。可以稍后再试一次。",
                    self.bot_display_name()
                ),
                Some(session.session_id),
                Some("compact"),
            ));
        }
        self.session_store
            .compact_history(
                &mut session,
                output.reply.trim(),
                COMPACT_KEEP_MESSAGE_LIMIT,
            )
            .map_err(session_error)?;
        Ok(command_response(
            "已压缩上下文。长档案收进抽屉，桌面只保留当前要用的便签。",
            Some(session.session_id),
            Some("compact"),
        ))
    }
}

/// 尝试从用户输入中解析出会话管理指令。
///
/// 支持的指令：new, rename, resume, list, clear, state, compact, help。
pub(super) fn parse_session_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_slash_command(text)?;
    if matches!(
        command.action.as_str(),
        "new" | "rename" | "resume" | "list" | "clear" | "state" | "compact" | "help"
    ) {
        Some(command)
    } else {
        None
    }
}

/// 解析"可跳等待办"的会话指令。
///
/// 如果用户输入是 new / resume / list / clear / state / help 之一，
/// 则跳过待办 pending 流程检查，直接执行会话指令。
pub(super) fn parse_pending_bypass_session_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_session_command(text)?;
    if matches!(
        command.action.as_str(),
        "new" | "resume" | "list" | "clear" | "state" | "help"
    ) {
        Some(command)
    } else {
        None
    }
}

/// 格式化当前会话状态的展示文本。
fn format_session_state_reply(session: &SessionRecord, bot_display_name: &str) -> String {
    if session.state.is_empty() && session.summary.trim().is_empty() && session.history.is_empty() {
        return format!("当前没有明确话题。{bot_display_name}桌面是空的，可以直接开新话题。");
    }
    let topic = state_string(session, "current_topic")
        .or_else(|| context_session_title(Some(session.title.as_str())))
        .unwrap_or_else(|| "未明确".to_owned());
    format!(
        "当前状态：\n- 话题：{topic}\n- 历史轮数：{}",
        session.history.len()
    )
}

/// 格式化会话恢复列表（编号 / 标题 / 更新时间 / 预览）。
fn format_resume_list(candidates: &[SessionRecord]) -> String {
    if candidates.is_empty() {
        return "最近没有可恢复的旧会话。".to_owned();
    }
    let mut rows = vec!["最近会话：".to_owned()];
    for (index, session) in candidates.iter().take(5).enumerate() {
        let title = display_session_title(Some(session.title.as_str()));
        let updated_at = datetime_for_display(&session.updated_at);
        let preview = session_preview(session);
        if preview.is_empty() {
            rows.push(format!("{}. {}｜{}", index + 1, title, updated_at));
        } else {
            rows.push(format!(
                "{}. {}｜{}｜{}",
                index + 1,
                title,
                updated_at,
                preview
            ));
        }
    }
    rows.push(String::new());
    rows.push("使用 /resume 1 恢复。".to_owned());
    rows.join("\n")
}

/// 获取会话标题的展示形式。
fn session_title_for_display(session: &SessionRecord) -> String {
    display_session_title(Some(session.title.as_str()))
}

/// 获取会话预览文本（优先使用 summary，否则使用最后一条消息内容）。
fn session_preview(session: &SessionRecord) -> String {
    clean_string(session.summary.clone())
        .or_else(|| {
            session
                .history
                .iter()
                .rev()
                .find(|message| !message.content.trim().is_empty())
                .map(|message| message.content.clone())
        })
        .map(|text| truncate_chars(&text, 36))
        .unwrap_or_default()
}

/// 将 ISO 时间戳格式化为可读形式（"YYYY-MM-DD HH:MM"）。
pub(super) fn datetime_for_display(value: &str) -> String {
    if value.trim().is_empty() {
        return "未知时间".to_owned();
    }
    value.replace('T', " ").chars().take(16).collect()
}

/// 构建会话上下文的系统提示文本，供 LLM 理解当前会话状态。
///
/// 包含：会话标题、已持久化的正式会话状态、会话摘要和理解要求。
/// 普通聊天不再按关键词推断场景、模式或修正状态；历史启发式状态由 migration 清理。
pub(super) fn build_session_context(session: &SessionRecord) -> String {
    let mut rows = vec!["以下是当前 QQ 会话上下文，只用于理解本轮普通聊天：".to_owned()];
    if let Some(title) = context_session_title(Some(session.title.as_str())) {
        rows.push(format!("[会话标题]\n{title}"));
    }
    let state_rows = session
        .state
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key, value)))
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|(key, value)| format!("{key}: {value}"))
        .collect::<Vec<_>>();
    if state_rows.is_empty() {
        rows.push("[当前会话状态]\n暂无明确状态。".to_owned());
    } else {
        rows.push(format!("[当前会话状态]\n{}", state_rows.join("\n")));
    }
    if session.summary.trim().is_empty() {
        rows.push("[会话摘要]\n暂无。".to_owned());
    } else {
        rows.push(format!("[会话摘要]\n{}", session.summary.trim()));
    }
    rows.push(
        "[理解要求]\n如果当前用户消息是短句、补充句、纠正句或“继续/给 codex”一类指代句，优先结合最近对话和当前会话状态理解，不要当成孤立单轮问答。"
            .to_owned(),
    );
    rows.join("\n\n")
}
