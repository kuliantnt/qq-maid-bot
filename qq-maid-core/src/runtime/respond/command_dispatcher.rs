//! Respond 确定性命令分派。
//!
//! 本模块只处理 pending、session command、slash/确定性命令和进入聊天前的状态准备。
//! 若没有命中确定性路径，则返回 `PreparedChat` 交给 Chat flow 继续处理。

use crate::{error::LlmError, runtime::command::ParsedCommand};

use super::{
    PlannedRespond, RespondRequest, RespondResponse, RustRespondService,
    chat_flow::PreparedChat,
    codex_easter_egg,
    common::{command_response, session_error, suppressed_response},
    interaction_state::{
        command_bypasses_pending, prepare_message_context_for_model, respond_interaction_meta,
        respond_meta, session_pending_visible_to_user, shared_session_turn_actor,
        should_try_todo_flow,
    },
    memory_flow, rss_flow, search_flow, session_flow,
    set_flow::{parse_set_command, parse_unset_command},
    train_flow, translation_flow, weather_flow,
};

pub(super) enum DispatchOutcome {
    Respond(Box<RespondResponse>),
    Chat(Box<PreparedChat>),
}

/// 已注册 Slash 的一次性解析结果。
///
/// 识别结果既用于 pending 前的未知命令收口，也供后续原 handler 分派，避免两处各自
/// 维护 parser 清单。Todo、Memory 与 RSS 的 handler 仍保留领域内参数解析和校验。
enum RegisteredSlashCommand {
    Session(ParsedCommand),
    Translation(translation_flow::ParsedTranslationCommand),
    Set(ParsedCommand),
    Unset(ParsedCommand),
    Weather(ParsedCommand),
    Train(train_flow::ParsedTrainCommand),
    Radar(ParsedCommand),
    Roll,
    WebSearch(ParsedCommand),
    Rss,
    Todo,
    Memory,
    Voice(crate::runtime::tools::voice::VoiceCommand),
}

impl RegisteredSlashCommand {
    fn parse(text: &str) -> Option<Self> {
        if let Some(command) = crate::runtime::tools::voice::parse_voice_command(text) {
            return Some(Self::Voice(command));
        }
        if let Some(command) = session_flow::parse_session_command(text) {
            return Some(Self::Session(command));
        }
        if let Some(command) = translation_flow::parse_translation_command(text) {
            return Some(Self::Translation(command));
        }
        if let Some(command) = parse_set_command(text) {
            return Some(Self::Set(command));
        }
        if let Some(command) = parse_unset_command(text) {
            return Some(Self::Unset(command));
        }
        if let Some(command) = weather_flow::parse_weather_command(text) {
            return Some(Self::Weather(command));
        }
        if let Some(command) = train_flow::parse_train_command(text) {
            return Some(Self::Train(command));
        }
        if let Some(command) = crate::runtime::tools::radar::flow::parse_radar_command(text) {
            return Some(Self::Radar(command));
        }
        if crate::runtime::tools::roll::parse_roll_command(text).is_some() {
            return Some(Self::Roll);
        }
        if let Some(command) = search_flow::parse_web_search_command(text) {
            return Some(Self::WebSearch(command));
        }
        if rss_flow::parse_rss_command(text).is_some() {
            return Some(Self::Rss);
        }
        if should_try_todo_flow(text) {
            return Some(Self::Todo);
        }
        memory_flow::parse_memory_command(text).map(|_| Self::Memory)
    }
}

pub(super) struct CommandDispatcher<'a> {
    service: &'a RustRespondService,
}

impl<'a> CommandDispatcher<'a> {
    pub(super) fn new(service: &'a RustRespondService) -> Self {
        Self { service }
    }

    pub(super) async fn dispatch(
        &self,
        req: RespondRequest,
        planned: PlannedRespond,
    ) -> Result<DispatchOutcome, LlmError> {
        let mut outcome = self.dispatch_inner(req, planned).await?;
        if let DispatchOutcome::Respond(response) = &mut outcome {
            self.service.render_command_response(response);
        }
        Ok(outcome)
    }

    async fn dispatch_inner(
        &self,
        mut req: RespondRequest,
        planned: PlannedRespond,
    ) -> Result<DispatchOutcome, LlmError> {
        let user_text = req.effective_command_text();
        let command_text = self.service.command_prefix().normalize(&user_text);
        let command_text = command_text.as_deref();
        let pending_text = command_text.unwrap_or(&user_text);
        let foreign_command_text = self.service.is_foreign_or_repeated_command_text(&user_text);
        let meta = respond_meta(&req);
        let interaction_meta = respond_interaction_meta(&req);

        // `/ops` 必须在任何 session、pending 或模型路径之前确定性收口。
        if let Some(command) = command_text.and_then(crate::runtime::tools::ops::parse_ops_command)
        {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service.handle_ops_command(command, &req),
            )));
        }

        // 正式命令始终优先；未注册命令只有命中显式白名单时才返回 Codex 彩蛋。
        // 两类兜底都在读取 session 和进入 pending 前收口，避免娱乐反馈消费交互状态。
        let registered_slash_command = command_text.and_then(RegisteredSlashCommand::parse);
        if registered_slash_command.is_none()
            && let Some(response) =
                command_text.and_then(|text| codex_easter_egg::try_respond(text, &req))
        {
            return Ok(DispatchOutcome::Respond(Box::new(response)));
        }
        if command_text.is_some() && registered_slash_command.is_none() {
            let response = if req
                .group_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
                && !req.addressed_to_bot
            {
                suppressed_response("unknown_group_slash_command")
            } else {
                command_response(
                    "未知命令，发送 `/help` 查看可用命令。",
                    None,
                    Some("unknown_command"),
                )
            };
            return Ok(DispatchOutcome::Respond(Box::new(response)));
        }

        // `/roll` 是无状态娱乐命令：在读取 pending/session 前生成一次 D20 结果并收口，
        // 避免消费交互状态或继续进入普通 Agent / Tool Loop。
        if matches!(
            registered_slash_command.as_ref(),
            Some(RegisteredSlashCommand::Roll)
        ) {
            return Ok(DispatchOutcome::Respond(Box::new(command_response(
                crate::runtime::tools::roll::roll_default_reply(),
                None,
                Some("roll"),
            ))));
        }

        // pending、Todo 可见编号和 Memory 列表序号属于群内个人交互状态；
        // 普通聊天历史仍保留在 conversation session，避免把群聊上下文强制拆成私聊。
        let mut active_interaction_session = self
            .service
            .session_store
            .get_active(&interaction_meta)
            .map_err(session_error)?;
        let mut active_session = self
            .service
            .session_store
            .get_active(&meta)
            .map_err(session_error)?;
        let turn_actor = shared_session_turn_actor(&self.service.display_name_store, &meta, &req);
        if let Some(session) = active_interaction_session.as_mut() {
            session.set_turn_actor(turn_actor.clone());
        }
        if let Some(session) = active_session.as_mut() {
            session.set_turn_actor(turn_actor.clone());
        }

        // 若用户输入不是可直接执行的显式命令，则先检查是否有待处理操作（pending）。
        let bypass_pending_for_session_command = command_text.is_some_and(command_bypasses_pending);
        if !bypass_pending_for_session_command {
            if let Some(session) = active_interaction_session
                .as_mut()
                .filter(|session| session.pending_operation.is_some())
                && let Some(response) = self
                    .service
                    .handle_pending_operation(&req, pending_text, &meta, session)
                    .await?
            {
                return Ok(DispatchOutcome::Respond(Box::new(response)));
            }
            if let Some(session) = active_session
                .as_mut()
                .filter(|session| session_pending_visible_to_user(session, meta.user_id.as_deref()))
                && let Some(response) = self
                    .service
                    .handle_pending_operation(&req, pending_text, &meta, session)
                    .await?
            {
                return Ok(DispatchOutcome::Respond(Box::new(response)));
            }
        }

        // 检查是否为会话管理指令（/new, /clear, /state 等）
        if let Some(RegisteredSlashCommand::Session(command)) = registered_slash_command.as_ref() {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_session_command(command.clone(), &meta)
                    .await?,
            )));
        }

        if let Some(RegisteredSlashCommand::Voice(command)) = registered_slash_command.as_ref() {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service.handle_voice_command(*command, &req)?,
            )));
        }

        // 确保存在活跃会话（无则创建）
        let mut session = match active_session {
            Some(session) => session,
            None => self
                .service
                .session_store
                .get_or_create_active(&meta)
                .map_err(session_error)?,
        };
        session.set_turn_actor(turn_actor.clone());
        let force_tool_loop = planned
            .respond_route()
            .is_some_and(|decision| decision.uses_agent_runtime());

        // 检查是否为翻译指令（如 "/翻译 文本"、"/翻译日语 文本"）
        if let Some(RegisteredSlashCommand::Translation(command)) =
            registered_slash_command.as_ref()
        {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_translation_command(command.clone(), &meta, &user_text, &mut session)
                    .await?,
            )));
        }

        // 检查是否为用户偏好设置指令（如 "/set 昵称 脸脸"、"/unset 昵称"）
        if let Some(RegisteredSlashCommand::Set(command)) = registered_slash_command.as_ref() {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_set_command(
                        command.clone(),
                        &req,
                        &user_text,
                        meta.user_id.as_deref(),
                        &mut session,
                    )
                    .await?,
            )));
        }
        if let Some(RegisteredSlashCommand::Unset(command)) = registered_slash_command.as_ref() {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_unset_command(
                        command.clone(),
                        &req,
                        &user_text,
                        meta.user_id.as_deref(),
                        &mut session,
                    )
                    .await?,
            )));
        }

        // 检查是否为天气查询指令（如 "/北京天气" 或 "/天气北京"）
        if let Some(RegisteredSlashCommand::Weather(command)) = registered_slash_command.as_ref() {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_weather_command(command.clone(), &user_text, &mut session)
                    .await?,
            )));
        }

        // 检查是否为列车时刻查询指令（如 "/火车 G1 明天"）
        if let Some(RegisteredSlashCommand::Train(command)) = registered_slash_command.as_ref() {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_train_command(command.clone(), &user_text, &mut session)
                    .await?,
            )));
        }

        // 检查是否为雷达看板指令（如 "/rader codex" 或 "/雷达"）
        if let Some(RegisteredSlashCommand::Radar(command)) = registered_slash_command.as_ref() {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_radar_command(command.clone(), &user_text, &mut session)
                    .await?,
            )));
        }

        // 检查是否为联网搜索指令（如 "/查 关键词"）。
        if let Some(RegisteredSlashCommand::WebSearch(command)) = registered_slash_command.as_ref()
        {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_web_search_command(command.clone(), &req, &mut session)
                    .await?,
            )));
        }

        // 检查是否为 RSS 订阅指令（如 "/rss add ..." 或 "/订阅"）
        if matches!(
            registered_slash_command.as_ref(),
            Some(RegisteredSlashCommand::Rss)
        ) && let Some(command_text) = command_text
            && let Some(response) = self
                .service
                .handle_rss_flow(&req, command_text, &meta, &mut session)
                .await?
        {
            return Ok(DispatchOutcome::Respond(Box::new(response)));
        }

        // AgentRuntime 下由 Agent 自行决定是否调用 Todo Tool；
        // slash 命令、pending 和确定性 Todo 查询已在前面保持原路径。
        if !force_tool_loop {
            // 检查是否为待办相关操作（新增、查看、完成、编辑、删除等）
            let todo_text = command_text.unwrap_or(&user_text);
            let registered_todo = matches!(
                registered_slash_command.as_ref(),
                Some(RegisteredSlashCommand::Todo)
            );
            if !foreign_command_text
                && (registered_todo || (command_text.is_none() && should_try_todo_flow(todo_text)))
            {
                let mut interaction_session = match active_interaction_session.take() {
                    Some(session) => session,
                    None => self
                        .service
                        .session_store
                        .get_or_create_active(&interaction_meta)
                        .map_err(session_error)?,
                };
                interaction_session.set_turn_actor(turn_actor.clone());
                if let Some(response) = self
                    .service
                    .handle_todo_flow(&req, todo_text, &meta, &mut interaction_session, &session)
                    .await?
                {
                    return Ok(DispatchOutcome::Respond(Box::new(response)));
                }
                active_interaction_session = Some(interaction_session);
            }
        }

        // 检查是否为长期记忆相关操作（记忆新增、查看、更新、删除等）
        let memory_text = command_text.unwrap_or(&user_text);
        let registered_memory = matches!(
            registered_slash_command.as_ref(),
            Some(RegisteredSlashCommand::Memory)
        );
        if !force_tool_loop
            && !foreign_command_text
            && (registered_memory
                || (command_text.is_none()
                    && memory_flow::parse_memory_command(memory_text).is_some()))
        {
            let mut interaction_session = match active_interaction_session.take() {
                Some(session) => session,
                None => self
                    .service
                    .session_store
                    .get_or_create_active(&interaction_meta)
                    .map_err(session_error)?,
            };
            interaction_session.set_turn_actor(turn_actor.clone());
            if let Some(response) = self
                .service
                .handle_memory_flow(&req, memory_text, &meta, &mut interaction_session)
                .await?
            {
                return Ok(DispatchOutcome::Respond(Box::new(response)));
            }
        }

        // 兜底：进入普通 LLM 聊天流程。共享历史 actor 快照已在上方准备；这里把
        // 同一快照的 actor_ref 注入当前 MessageContext，供模型可靠映射当前发言人。
        // Issue #361：确定性 Todo 操作（完成/恢复）在私聊 + 可见快照存在 +
        // 目标唯一时直接在服务端解析真实 ID 执行，不再携带完整聊天历史进入
        // LLM Tool Loop；歧义、缺快照、权限不足或需确认时返回 None 保持现有流程。
        if force_tool_loop
            && let Some(response) = self
                .service
                .try_deterministic_todo_confirm(
                    &req,
                    &user_text,
                    &meta,
                    &mut session,
                    active_interaction_session.as_ref(),
                )
                .await?
        {
            return Ok(DispatchOutcome::Respond(Box::new(response)));
        }
        prepare_message_context_for_model(
            &self.service.display_name_store,
            &meta,
            &mut req,
            turn_actor.as_ref(),
        );
        let respond_route = planned.respond_route().ok_or_else(|| {
            LlmError::new(
                "respond_route_missing",
                "chat execution requires the router decision",
                "router",
            )
        })?;
        let status_hint = planned.classified_status_hint();
        Ok(DispatchOutcome::Chat(Box::new(PreparedChat {
            req,
            user_text,
            meta,
            session,
            respond_route,
            status_hint,
        })))
    }
}
