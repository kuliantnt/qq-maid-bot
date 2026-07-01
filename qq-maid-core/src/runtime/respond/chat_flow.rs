//! 普通聊天流程。
//!
//! 承担 `RustRespondService` 中"兜底聊天"路径的实现：
//! 组装 LLM 请求、发起调用、保存对话记录、自动生成会话标题等。

use std::{future::Future, pin::Pin};

use serde_json::{Value, json};

use crate::{
    error::LlmError,
    provider::types::{ChatMessage, ChatRole},
    runtime::{
        prompt::{MemberIdMatch, build_member_identity_context, unknown_member_id_reply},
        session::{DEFAULT_SESSION_TITLE, SessionMeta, SessionRecord, redact_sensitive_text},
    },
};

use super::{
    ChatToolPlan, RespondPurpose, RespondRequest, RespondResponse, RustRespondService,
    common::{
        SESSION_HISTORY_MESSAGE_LIMIT, SESSION_STATE_SHORT_TEXT_LIMIT, command_response,
        empty_respond_request, memory_error, merge_metadata, session_error, state_string,
        truncate_chars,
    },
    llm_service::{ChatService, LlmChatService, response_from_output},
    session_flow::build_session_context,
    title::{context_session_title, generate_session_title},
};

pub(super) mod todo_guard;

impl RustRespondService {
    /// 处理普通聊天请求。
    ///
    /// 1. 空消息直接返回提示。
    /// 2. 更新会话状态（话题、场景、模式等）。
    /// 3. 处理成员编号 @ 提及。
    /// 4. 构建会话上下文与记忆上下文。
    /// 5. 调用 LLM 获取回复。
    /// 6. 保存对话记录。
    /// 7. 尝试自动生成会话标题。
    pub(super) async fn handle_chat(
        &self,
        req: RespondRequest,
        user_text: String,
        meta: SessionMeta,
        mut session: SessionRecord,
        chat_tool_plan: ChatToolPlan,
    ) -> Result<RespondResponse, LlmError> {
        if user_text.trim().is_empty() {
            let reply = "唔，小女仆在。可以直接说要我看哪一块。";
            self.session_store
                .append_exchange(&mut session, &user_text, reply)
                .map_err(session_error)?;
            return Ok(command_response(
                reply,
                Some(session.session_id),
                Some("empty_chat"),
            ));
        }

        update_session_state_from_user(&mut session, &user_text);
        let is_group_chat = meta
            .group_id
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        // 群聊里不要求用户先带成员编号；成员映射仍保留给私聊或明确编号的场景，
        // 避免群里普通三位数字被误判成身份切换或触发未知编号追问。
        let member_matches = if is_group_chat {
            Vec::new()
        } else {
            self.prompt_config.find_member_id_mentions(&user_text)?
        };
        if !is_group_chat
            && let Some(unknown) = member_matches.iter().find(|item| item.name.is_none())
        {
            let mapping = self.prompt_config.load_member_id_mapping()?;
            let reply = unknown_member_id_reply(&unknown.member_id, &mapping);
            self.session_store
                .append_exchange(&mut session, &user_text, &reply)
                .map_err(session_error)?;
            return Ok(command_response(
                reply,
                Some(session.session_id),
                Some("member_id_unknown"),
            ));
        }
        update_session_speaker_hint(&mut session, &member_matches);

        let mut session_context = build_session_context(&session);
        if let Some(identity_context) = build_member_identity_context(&member_matches) {
            session_context.push_str("\n\n");
            session_context.push_str(&identity_context);
        }

        let knowledge_context = self.knowledge_index.search_context(&user_text)?;
        let used_knowledge = !knowledge_context.text.trim().is_empty();
        let memory_context = self.build_memory_context(&meta)?;
        let used_memory = !memory_context.trim().is_empty();
        let system_prompts = if is_group_chat {
            self.prompt_config.load_static_prompts_only()?
        } else {
            self.prompt_config.load_system_prompts()?
        };
        let chat_req = RespondRequest {
            session_id: session.session_id.clone(),
            purpose: RespondPurpose::Chat,
            user_text: user_text.clone(),
            system_prompts,
            memory_context,
            knowledge_context: knowledge_context.text.clone(),
            session_context,
            history_messages: recent_session_messages(&session, SESSION_HISTORY_MESSAGE_LIMIT),
            scope_key: meta.scope_key.clone(),
            user_id: meta.user_id.clone(),
            group_id: meta.group_id.clone(),
            guild_id: meta.guild_id.clone(),
            channel_id: meta.channel_id.clone(),
            message_id: req.message_id.clone(),
            timestamp: req.timestamp.clone(),
            platform: meta.platform.clone(),
            event_type: req.event_type.clone(),
            metadata: merge_metadata(
                req.metadata,
                &[
                    ("purpose", "chat"),
                    ("platform", meta.platform.as_str()),
                    ("scope_key", meta.scope_key.as_str()),
                ],
            ),
            ..empty_respond_request()
        };
        let service =
            LlmChatService::with_context_budget(self.provider.clone(), self.context_budget);
        let use_tool_loop = matches!(chat_tool_plan, ChatToolPlan::ForceCompleteToolLoop);
        let (output, todo_success_validation) = if use_tool_loop {
            let output = service
                .respond_with_tools(
                    chat_req,
                    self.tool_registry.clone(),
                    self.tool_calling_max_rounds,
                )
                .await?;
            let validation = todo_guard::validate_todo_success_reply(&output);
            let output = if validation.passed() {
                output
            } else {
                todo_success_not_verified_output(output)
            };
            (output, validation)
        } else {
            (
                service.respond(chat_req).await?,
                todo_guard::TodoSuccessValidation::Passed {
                    claimed_success: false,
                },
            )
        };

        let reply = output.reply.clone();
        let executed_tools = output.executed_tools.clone();
        if use_tool_loop {
            let mut latest_session = self
                .session_store
                .get(&session.session_id)
                .map_err(session_error)?
                .ok_or_else(|| {
                    LlmError::new(
                        "session_missing",
                        format!(
                            "session `{}` disappeared after tool loop",
                            session.session_id
                        ),
                        "session",
                    )
                })?;
            // Tool 执行会基于同一 active session 保存 pending/最近 Todo 查询等字段；
            // 这里只把本轮聊天在调用模型前更新的状态合并回最新记录，避免旧 SessionRecord 覆盖工具结果。
            latest_session.state = session.state.clone();
            session = latest_session;
        }
        self.session_store
            .append_exchange(&mut session, &user_text, &reply)
            .map_err(session_error)?;
        self.schedule_auto_title(session.clone());

        let mut response = response_from_output(output);
        response.session_id = Some(session.session_id);
        response.command = None;
        response.handled = Some(true);
        response.diagnostics = Some(json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": used_memory,
            "used_knowledge": used_knowledge,
            "knowledge_hit_count": knowledge_context.hit_count,
            "used_search": false,
            "tool_calling_enabled": use_tool_loop,
            "tool_loop_executed_tools": executed_tools,
            "todo_success_claimed": todo_success_validation.claimed_success(),
            "todo_success_verified": todo_success_validation.passed(),
            "tool_retry_count": 0,
            "error_code": if use_tool_loop && !todo_success_validation.passed() {
                json!("todo_success_not_verified")
            } else {
                Value::Null
            },
        }));
        Ok(response)
    }

    /// 普通聊天真流式路径：复用非流式聊天的上下文构造和后处理，只替换 LLM 调用方式。
    pub async fn handle_chat_stream<F>(
        &self,
        req: RespondRequest,
        on_delta: F,
    ) -> Result<RespondResponse, LlmError>
    where
        F: FnMut(String) -> Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send>> + Send,
    {
        let user_text = req.effective_user_text();
        let meta = SessionMeta::new(
            req.scope_key.clone(),
            req.user_id.clone(),
            req.group_id.clone(),
            req.guild_id.clone(),
            req.channel_id.clone(),
            req.platform.clone(),
        );
        let mut session = self
            .session_store
            .get_or_create_active(&meta)
            .map_err(session_error)?;
        if user_text.trim().is_empty() {
            return self
                .handle_chat(req, user_text, meta, session, ChatToolPlan::Plain)
                .await;
        }

        update_session_state_from_user(&mut session, &user_text);
        let is_group_chat = meta
            .group_id
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        let member_matches = if is_group_chat {
            Vec::new()
        } else {
            self.prompt_config.find_member_id_mentions(&user_text)?
        };
        if !is_group_chat
            && let Some(unknown) = member_matches.iter().find(|item| item.name.is_none())
        {
            let mapping = self.prompt_config.load_member_id_mapping()?;
            let reply = unknown_member_id_reply(&unknown.member_id, &mapping);
            self.session_store
                .append_exchange(&mut session, &user_text, &reply)
                .map_err(session_error)?;
            return Ok(command_response(
                reply,
                Some(session.session_id),
                Some("member_id_unknown"),
            ));
        }
        update_session_speaker_hint(&mut session, &member_matches);

        let mut session_context = build_session_context(&session);
        if let Some(identity_context) = build_member_identity_context(&member_matches) {
            session_context.push_str("\n\n");
            session_context.push_str(&identity_context);
        }

        let knowledge_context = self.knowledge_index.search_context(&user_text)?;
        let used_knowledge = !knowledge_context.text.trim().is_empty();
        let memory_context = self.build_memory_context(&meta)?;
        let used_memory = !memory_context.trim().is_empty();
        let system_prompts = if is_group_chat {
            self.prompt_config.load_static_prompts_only()?
        } else {
            self.prompt_config.load_system_prompts()?
        };
        let service =
            LlmChatService::with_context_budget(self.provider.clone(), self.context_budget);
        let output = service
            .stream_respond(
                RespondRequest {
                    session_id: session.session_id.clone(),
                    purpose: RespondPurpose::Chat,
                    user_text: user_text.clone(),
                    system_prompts,
                    memory_context,
                    knowledge_context: knowledge_context.text.clone(),
                    session_context,
                    history_messages: recent_session_messages(
                        &session,
                        SESSION_HISTORY_MESSAGE_LIMIT,
                    ),
                    scope_key: meta.scope_key.clone(),
                    user_id: meta.user_id.clone(),
                    group_id: meta.group_id.clone(),
                    guild_id: meta.guild_id.clone(),
                    channel_id: meta.channel_id.clone(),
                    message_id: req.message_id.clone(),
                    timestamp: req.timestamp.clone(),
                    platform: meta.platform.clone(),
                    event_type: req.event_type.clone(),
                    metadata: merge_metadata(
                        req.metadata,
                        &[
                            ("purpose", "chat"),
                            ("platform", meta.platform.as_str()),
                            ("scope_key", meta.scope_key.as_str()),
                        ],
                    ),
                    ..empty_respond_request()
                },
                on_delta,
            )
            .await?;

        let reply = output.reply.clone();
        self.session_store
            .append_exchange(&mut session, &user_text, &reply)
            .map_err(session_error)?;
        self.schedule_auto_title(session.clone());

        let mut response = response_from_output(output);
        response.session_id = Some(session.session_id);
        response.command = None;
        response.handled = Some(true);
        response.diagnostics = Some(json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": used_memory,
            "used_knowledge": used_knowledge,
            "knowledge_hit_count": knowledge_context.hit_count,
            "used_search": false,
        }));
        Ok(response)
    }

    /// 从长期记忆存储中读取当前请求可访问的最近记录，组装为系统提示上下文。
    ///
    /// 个人和群记忆先在 SQL 中限定各自合法作用域，再沿用原有 `row_id DESC LIMIT 12`
    /// 合并排序；这里不做固定配额，避免低排序记忆挤掉原本更靠前的合法记忆。
    pub(super) fn build_memory_context(&self, meta: &SessionMeta) -> Result<String, LlmError> {
        let records = self
            .memory_store
            .list_accessible_for_context(meta.user_id.as_deref(), meta.group_id.as_deref(), 12)
            .map_err(memory_error)?;
        let rows = records
            .iter()
            .filter(|record| !record.content.trim().is_empty())
            .map(|record| format!("- [{}] {}", record.ts, record.content))
            .collect::<Vec<_>>();
        if rows.is_empty() {
            Ok(String::new())
        } else {
            let mut context = format!(
                "以下是用户明确要求记录的本地记忆，只作为参考，不要机械复述：\n{}",
                rows.join("\n")
            );
            if meta
                .group_id
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                context.push_str(
                    "\n群聊隐私约束：个人记忆只用于理解当前发言者，不要主动披露、列举或转述个人记忆。",
                );
            }
            Ok(context)
        }
    }

    /// 如果会话标题还是默认值，且用户消息轮数在 2~4 之间，则后台尝试生成标题。
    ///
    /// 主聊天回复已经完成落库，标题只是展示增强；不能让标题模型的慢响应、
    /// 失败或取消影响本轮 `Completed`。后台任务只允许条件更新标题，不能保存
    /// 旧的完整会话快照，否则会覆盖期间继续写入的历史、pending 或手工重命名。
    fn schedule_auto_title(&self, session: SessionRecord) {
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

fn todo_success_not_verified_output(
    output: super::llm_service::RespondOutput,
) -> super::llm_service::RespondOutput {
    let reply = todo_guard::todo_success_not_verified_reply();
    super::llm_service::RespondOutput {
        reply: reply.clone(),
        text: reply.clone(),
        markdown: None,
        chat: super::types::ChatResponse::ok(
            reply,
            crate::util::metrics::LlmMetrics {
                provider: "rust".to_owned(),
                model: "tool-loop-guard".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 0,
            },
            None,
        ),
        executed_tools: output.executed_tools,
        tool_results: output.tool_results,
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
            }),
            "assistant" => Some(ChatMessage {
                role: ChatRole::Assistant,
                content: message.content.clone(),
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
/// 根据用户输入更新会话状态（话题、场景、模式、焦点等）。
fn update_session_state_from_user(session: &mut SessionRecord, user_text: &str) {
    let text = user_text.trim();
    if text.is_empty() {
        return;
    }
    let current_topic = state_string(session, "current_topic")
        .or_else(|| context_session_title(Some(session.title.as_str())));
    if current_topic.is_none() && !is_short_followup(text) {
        let topic = compact_topic(text, 32);
        if !topic.is_empty() {
            session
                .state
                .insert("current_topic".to_owned(), Value::String(topic.clone()));
        }
    }
    session
        .state
        .entry("active_scene")
        .or_insert_with(|| Value::String("默认会话".to_owned()));
    let mode = infer_expected_mode(text, state_string(session, "expected_mode").as_deref());
    session
        .state
        .insert("expected_mode".to_owned(), Value::String(mode));
    if let Some(focus) = infer_recent_session_focus(text) {
        set_short_state(session, "recent_session_focus", focus);
    }
    if current_topic.is_some() && looks_like_correction(text) {
        set_short_state(session, "last_user_correction", compact_topic(text, 48));
    }
}

/// 根据成员编号匹配结果更新会话中的说话者提示。
fn update_session_speaker_hint(session: &mut SessionRecord, matches: &[MemberIdMatch]) {
    let rows = matches
        .iter()
        .filter_map(|item| {
            let name = item.name.as_deref()?.trim();
            if name.is_empty() {
                None
            } else {
                Some(format!("{} {}", item.member_id, name))
            }
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return;
    }
    set_short_state(
        session,
        "current_speaker_hint",
        format!("本轮明确编号：{}", rows.join(" / ")),
    );
}

/// 将短文本写入会话状态，自动脱敏并截断。
fn set_short_state(session: &mut SessionRecord, key: &str, value: impl AsRef<str>) {
    let value = redact_sensitive_text(value.as_ref());
    let value = truncate_chars(&value, SESSION_STATE_SHORT_TEXT_LIMIT);
    if value.trim().is_empty() {
        return;
    }
    session
        .state
        .insert(key.to_owned(), Value::String(value.trim().to_owned()));
}

/// 从用户输入推断最近会话焦点类别（身份、场景、设定、记忆边界等）。
fn infer_recent_session_focus(text: &str) -> Option<&'static str> {
    if contains_any(text, &["前台", "身份", "切换", "编号", "成员", "说话者"]) {
        return Some("身份/成员识别");
    }
    if contains_any(text, &["场景", "背景", "上下文"]) {
        return Some("会话场景");
    }
    if contains_any(text, &["设定", "剧情", "世界观", "档案", "角色"]) {
        return Some("设定整理");
    }
    if contains_any(text, &["记忆", "记一下", "/memory", "/记忆", "/记"]) {
        return Some("长期记忆边界");
    }
    None
}

/// 从用户输入推断期望的对话模式（书记官整理 / 方案讨论 / 低电量陪伴 / 继续上一轮等）。
fn infer_expected_mode(text: &str, current_mode: Option<&str>) -> String {
    let lowered = text.to_ascii_lowercase();
    if [
        "codex",
        "readme",
        "wiki",
        "整理",
        "确认",
        "出版本",
        "存档",
        "归档",
        "文档",
        "修改说明",
    ]
    .iter()
    .any(|keyword| lowered.contains(&keyword.to_ascii_lowercase()))
    {
        return "书记官整理".to_owned();
    }
    if [
        "怎么定",
        "怎么改",
        "怎么处理",
        "选哪个",
        "要不要",
        "给几个方案",
        "方案",
    ]
    .iter()
    .any(|keyword| text.contains(keyword))
    {
        return "方案讨论".to_owned();
    }
    if ["累", "困", "焦虑", "睡不着", "不想动", "低电量"]
        .iter()
        .any(|keyword| text.contains(keyword))
    {
        return "低电量陪伴".to_owned();
    }
    if text.contains("继续") {
        return current_mode.unwrap_or("继续上一轮").to_owned();
    }
    current_mode.unwrap_or("陪聊 + 轻量整理").to_owned()
}

/// 判断用户输入是否包含修正性用语（"不是""应该是""补充"等）。
fn looks_like_correction(text: &str) -> bool {
    [
        "不是",
        "不对",
        "我的意思是",
        "我是说",
        "应该是",
        "其实",
        "补充",
        "还有",
        "漏了",
        "改成",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// 检查文本是否包含关键字列表中的任意一个。
fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// 判断是否为短接续句（字符数 <= 24）。
fn is_short_followup(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && text.chars().count() <= 24
}

/// 将用户输入压缩为简短话题词，去除首尾标点和"小女仆"称谓。
fn compact_topic(text: &str, max_length: usize) -> String {
    let mut topic = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(&[' ', '：', ':', '，', ',', '。', '.', '!', '！', '?', '？'][..])
        .replace("小女仆", "");
    topic = topic
        .trim_matches(&[' ', '：', ':', '，', ',', '。', '.', '!', '！', '?', '？'][..])
        .to_owned();
    truncate_chars(&topic, max_length)
}
