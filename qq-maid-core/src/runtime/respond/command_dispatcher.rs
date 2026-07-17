//! Respond 确定性命令分派。
//!
//! 本模块只处理 pending、session command、slash/确定性命令和进入聊天前的状态准备。
//! 若没有命中确定性路径，则返回 `PreparedChat` 交给 Chat flow 继续处理。

use crate::{error::LlmError, runtime::command::is_slash_command_candidate};

use super::{
    PlannedRespond, RespondRequest, RespondResponse, RustRespondService,
    chat_flow::PreparedChat,
    common::{session_error, suppressed_response},
    interaction_state::{
        command_bypasses_pending, prepare_message_context_for_model, respond_interaction_meta,
        respond_meta, session_pending_visible_to_user, shared_session_turn_actor,
        should_try_todo_flow,
    },
    memory_flow, radar_flow, search_flow, session_flow,
    set_flow::{parse_set_command, parse_unset_command},
    train_flow, translation_flow, weather_flow,
};

pub(super) enum DispatchOutcome {
    Respond(Box<RespondResponse>),
    Chat(Box<PreparedChat>),
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
        mut req: RespondRequest,
        planned: PlannedRespond,
    ) -> Result<DispatchOutcome, LlmError> {
        let user_text = req.effective_user_text();
        let meta = respond_meta(&req);
        let interaction_meta = respond_interaction_meta(&req);

        // `/ops` 必须在任何 session、pending 或模型路径之前确定性收口。
        if let Some(command) = crate::runtime::tools::ops::parse_ops_command(&user_text) {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service.handle_ops_command(command, &req),
            )));
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
        let bypass_pending_for_session_command = command_bypasses_pending(&user_text);
        if !bypass_pending_for_session_command {
            if let Some(session) = active_interaction_session
                .as_mut()
                .filter(|session| session.pending_operation.is_some())
                && let Some(response) = self
                    .service
                    .handle_pending_operation(&req, &user_text, &meta, session)
                    .await?
            {
                return Ok(DispatchOutcome::Respond(Box::new(response)));
            }
            if let Some(session) = active_session
                .as_mut()
                .filter(|session| session_pending_visible_to_user(session, meta.user_id.as_deref()))
                && let Some(response) = self
                    .service
                    .handle_pending_operation(&req, &user_text, &meta, session)
                    .await?
            {
                return Ok(DispatchOutcome::Respond(Box::new(response)));
            }
        }

        // 检查是否为会话管理指令（/new, /clear, /state 等）
        if let Some(command) = session_flow::parse_session_command(&user_text) {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service.handle_session_command(command, &meta).await?,
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
        if let Some(command) = translation_flow::parse_translation_command(&user_text) {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_translation_command(command, &meta, &user_text, &mut session)
                    .await?,
            )));
        }

        // 检查是否为用户偏好设置指令（如 "/set 昵称 脸脸"、"/unset 昵称"）
        if let Some(command) = parse_set_command(&user_text) {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_set_command(
                        command,
                        &req,
                        &user_text,
                        meta.user_id.as_deref(),
                        &mut session,
                    )
                    .await?,
            )));
        }
        if let Some(command) = parse_unset_command(&user_text) {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_unset_command(
                        command,
                        &req,
                        &user_text,
                        meta.user_id.as_deref(),
                        &mut session,
                    )
                    .await?,
            )));
        }

        // 检查是否为天气查询指令（如 "/北京天气" 或 "/天气北京"）
        if let Some(command) = weather_flow::parse_weather_command(&user_text) {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_weather_command(command, &user_text, &mut session)
                    .await?,
            )));
        }

        // 检查是否为列车时刻查询指令（如 "/火车 G1 明天"）
        if let Some(command) = train_flow::parse_train_command(&user_text) {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_train_command(command, &user_text, &mut session)
                    .await?,
            )));
        }

        // 检查是否为雷达看板指令（如 "/rader codex" 或 "/雷达"）
        if let Some(command) = radar_flow::parse_radar_command(&user_text) {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_radar_command(command, &user_text, &mut session)
                    .await?,
            )));
        }

        // 检查是否为联网搜索指令（如 "/查 关键词"）。
        if let Some(command) = search_flow::parse_web_search_command(&user_text) {
            return Ok(DispatchOutcome::Respond(Box::new(
                self.service
                    .handle_web_search_command(command, &req, &mut session)
                    .await?,
            )));
        }

        // 检查是否为 RSS 订阅指令（如 "/rss add ..." 或 "/订阅"）
        if let Some(response) = self
            .service
            .handle_rss_flow(&req, &user_text, &meta, &mut session)
            .await?
        {
            return Ok(DispatchOutcome::Respond(Box::new(response)));
        }

        // AgentRuntime 下由 Agent 自行决定是否调用 Todo Tool；
        // slash 命令、pending 和确定性 Todo 查询已在前面保持原路径。
        if !force_tool_loop {
            // 检查是否为待办相关操作（新增、查看、完成、编辑、删除等）
            if should_try_todo_flow(&user_text) {
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
                    .handle_todo_flow(&user_text, &meta, &mut interaction_session)
                    .await?
                {
                    return Ok(DispatchOutcome::Respond(Box::new(response)));
                }
                active_interaction_session = Some(interaction_session);
            }
        }

        // 检查是否为长期记忆相关操作（记忆新增、查看、更新、删除等）
        if !force_tool_loop && memory_flow::parse_memory_command(&user_text).is_some() {
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
                .handle_memory_flow(&req, &user_text, &meta, &mut interaction_session)
                .await?
            {
                return Ok(DispatchOutcome::Respond(Box::new(response)));
            }
        }

        // 所有已注册命令解析器都已尝试；群聊中的剩余斜杠候选属于未知命令。
        // Core 在这里明确静默收口，避免把命令文本当普通聊天交给模型。
        if req
            .group_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty())
            && is_slash_command_candidate(&user_text)
        {
            return Ok(DispatchOutcome::Respond(Box::new(suppressed_response(
                "unknown_group_slash_command",
            ))));
        }

        // 兜底：进入普通 LLM 聊天流程。共享历史 actor 快照已在上方准备；这里把
        // 同一快照的 actor_ref 注入当前 MessageContext，供模型可靠映射当前发言人。
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
