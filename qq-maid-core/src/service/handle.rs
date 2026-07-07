use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::time::timeout;
use tracing::warn;

use crate::{
    app::CoreRuntimeState,
    config::AppConfig,
    error::LlmError,
    provider::types::{ChatMessage, ChatRequest, ChatRole},
    runtime::respond::{
        RespondExecutors, RespondPlan, RespondRequest, RespondResponse, RespondServiceOptions,
        RespondStores, RustRespondService, StatusAudience,
    },
    util::metrics::MetricsRecorder,
};

use super::{
    CoreActor, CoreConversation, CoreError, CoreGroupMemberRole, CoreHealthSnapshot,
    CoreInboundClassification, CoreRequest, CoreRespondOutput, CoreResponse, CoreService, Platform,
    ProgressStatusConfig, error_core_error, output_policy_for_stream, start_core_response_stream,
    warn_core_error,
};

#[derive(Clone)]
pub struct CoreHandle {
    state: Arc<CoreRuntimeState>,
}

impl CoreHandle {
    pub fn new(state: CoreRuntimeState) -> Self {
        Self {
            state: Arc::new(state),
        }
    }

    pub(super) fn respond_service(&self) -> RustRespondService {
        let state = self.state.as_ref();
        RustRespondService::new(
            state.provider.clone(),
            RespondExecutors {
                query_executor: state.executors.query_executor.clone(),
                weather_executor: state.executors.weather_executor.clone(),
                train_executor: state.executors.train_executor.clone(),
                radar_executor: state.executors.radar_executor.clone(),
            },
            RespondStores {
                memory_store: state.stores.memory_store.clone(),
                session_store: state.stores.session_store.clone(),
                todo_store: state.stores.todo_store.clone(),
                notification_store: state.stores.notification_store.clone(),
                rss_store: state.stores.rss_store.clone(),
                display_name_store: state.stores.display_name_store.clone(),
            },
            state.rss_fetcher.clone(),
            state.knowledge_index.clone(),
            state.prompt_config.clone(),
            respond_options(&state.config),
        )
    }
}

#[async_trait]
impl CoreService for CoreHandle {
    async fn respond(&self, request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
        let force_complete_sync = matches!(request.platform, Platform::WechatService);
        let req: RespondRequest = request.into();
        let service = self.respond_service();
        let recorder = MetricsRecorder::start();
        let scope_key = req.scope_key.clone();
        let state = self.state.as_ref();
        let respond_plan = service.plan_core_respond(&req).map_err(CoreError::from)?;
        if matches!(
            respond_plan,
            RespondPlan::StreamingChat | RespondPlan::CompleteToolLoop
        ) {
            // 微信服务号同步 XML 回包无法承载直出流式；这里仅对微信禁用 direct stream，
            // 让 Gateway 消费 Completed 后再渲染 XML，QQ 官方流式行为保持不变。
            let provider_stream_enabled = state.provider.stream_enabled() && !force_complete_sync;
            let output_policy = output_policy_for_stream(respond_plan, provider_stream_enabled);
            let status_hint = service
                .status_hint_for_plan(&req, respond_plan)
                .map_err(CoreError::from)?;
            let status_display_name = service.status_display_name().to_owned();
            let status_audience = if req
                .group_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            {
                StatusAudience::Group
            } else {
                StatusAudience::Private
            };
            let result = timeout(
                Duration::from_secs(state.config.request_timeout_seconds),
                async {
                    Ok::<_, LlmError>(start_core_response_stream(
                        service,
                        req,
                        respond_plan,
                        output_policy,
                        provider_stream_enabled,
                        Duration::from_secs(state.config.request_timeout_seconds),
                        ProgressStatusConfig {
                            hint: status_hint,
                            audience: status_audience,
                            display_name: status_display_name,
                        },
                    ))
                },
            )
            .await;
            return match result {
                Ok(Ok(stream)) => Ok(CoreRespondOutput::Stream(stream)),
                Ok(Err(err)) => {
                    warn_core_error(&scope_key, &err);
                    Err(err.into())
                }
                Err(_) => {
                    let err = LlmError::timeout("stream_init");
                    error_core_error(&scope_key, &err);
                    let _metrics = recorder.fail(
                        state.provider.name(),
                        state.provider.model(),
                        state.provider.stream_enabled(),
                    );
                    Err(err.into())
                }
            };
        }
        let result = timeout(
            Duration::from_secs(state.config.request_timeout_seconds),
            service.respond_with_plan(req, respond_plan),
        )
        .await;

        match result {
            Ok(Ok(response)) if response.ok => {
                Ok(CoreRespondOutput::Complete(Box::new(response.into())))
            }
            Ok(Ok(response)) => {
                let err = response.error.map(CoreError::from).unwrap_or_else(|| {
                    CoreError::new("internal_error", "respond", "处理失败，请稍后再试")
                });
                warn!(
                    scope_key,
                    error_code = err.code,
                    error_stage = err.stage,
                    "core respond returned business error"
                );
                Err(err)
            }
            Ok(Err(err)) => {
                warn_core_error(&scope_key, &err);
                Err(err.into())
            }
            Err(_) => {
                let err = LlmError::timeout("request");
                error_core_error(&scope_key, &err);
                let _metrics = recorder.fail(
                    state.provider.name(),
                    state.provider.model(),
                    state.provider.stream_enabled(),
                );
                Err(err.into())
            }
        }
    }

    async fn classify_inbound(
        &self,
        request: CoreRequest,
    ) -> Result<CoreInboundClassification, CoreError> {
        let req: RespondRequest = request.into();
        let service = self.respond_service();
        service.classify_inbound(req).map_err(CoreError::from)
    }

    async fn upstream_check(&self) -> Result<(), CoreError> {
        let state = self.state.as_ref();
        let request = ChatRequest {
            session_id: "diagnostic:upstream_check".to_owned(),
            model: None,
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "这是连通性检查。请只回复 OK。".to_owned(),
                content_parts: Vec::new(),
            }],
            context_budget: None,
            max_output_tokens: None,
            reasoning_effort: None,
            metadata: HashMap::from([("purpose".to_owned(), "upstream_check".to_owned())]),
        };

        match timeout(
            Duration::from_secs(state.config.request_timeout_seconds),
            state.provider.chat(request),
        )
        .await
        {
            Ok(Ok(outcome)) if !outcome.reply.trim().is_empty() => Ok(()),
            Ok(Ok(_)) => {
                let error = LlmError::provider("upstream returned empty response", "diagnostic");
                // 空正文不能证明响应解析可用，必须显式覆盖为失败状态。
                state.upstream_status.record_failure(&error);
                Err(CoreError::new(
                    "provider_error",
                    "diagnostic",
                    "上游返回空响应",
                ))
            }
            Ok(Err(error)) => Err(error.into()),
            Err(_) => {
                let error = LlmError::timeout("upstream_check");
                // timeout 会取消被观测 provider 的 future，因此在入口补记失败状态。
                state.upstream_status.record_failure(&error);
                Err(error.into())
            }
        }
    }

    fn health_snapshot(&self) -> CoreHealthSnapshot {
        let state = self.state.as_ref();
        CoreHealthSnapshot {
            ok: true,
            provider: state.provider.name().to_owned(),
            model: state.provider.model().to_owned(),
            stream: state.provider.stream_enabled(),
            upstream: state.upstream_status.snapshot(),
        }
    }
}

impl From<CoreRequest> for RespondRequest {
    fn from(value: CoreRequest) -> Self {
        let scope_key = value.scope_key();
        // 先在发生字段移动前派生 message_context（#319 收敛：由权威字段派生）。
        let message_context = value.message_context();
        let (group_id, channel_id, event_type) = match &value.conversation {
            CoreConversation::Private { .. } => (None, None, "c2c_message"),
            CoreConversation::Group { group_id } => (Some(group_id.clone()), None, "group_message"),
            CoreConversation::ServiceAccount { .. } => (None, None, "service_account_message"),
        };
        Self {
            content: value.text,
            input_parts: value.input_parts,
            quoted: value.quoted,
            message_context: Some(message_context),
            visible_entity_snapshot: value.visible_entity_snapshot,
            scope_key,
            user_id: value.actor.user_id,
            group_member_role: value
                .actor
                .group_member_role
                .map(|role| role.as_str().to_owned()),
            group_id,
            guild_id: None,
            channel_id,
            platform: value.platform.as_str().to_owned(),
            account_id: value.account_id,
            event_type: event_type.to_owned(),
            ..Default::default()
        }
    }
}

impl From<RespondResponse> for CoreResponse {
    fn from(value: RespondResponse) -> Self {
        Self {
            text: value.text,
            markdown: value.markdown,
            handled: value.handled,
            session_id: value.session_id,
            command: value.command,
            diagnostics: value.diagnostics,
            visible_entity_snapshot: value.visible_entity_snapshot,
        }
    }
}

fn respond_options(config: &AppConfig) -> RespondServiceOptions {
    RespondServiceOptions {
        title_model: config.title_model.clone(),
        todo_model: config.todo_model.clone(),
        memory_model: config.memory_model.clone(),
        compact_model: config.compact_model.clone(),
        translation_model: config.translation_model.clone(),
        rss_summary_max_chars: config.rss_summary_max_chars as usize,
        rss_seen_retention: config.rss_seen_retention as usize,
        tool_calling_enabled: config.tool_calling_enabled,
        tool_calling_group_enabled: config.tool_calling_group_enabled,
        tool_calling_max_rounds: config.tool_calling_max_rounds as usize,
        context_budget: config.context_budget,
        tool_result_max_chars: config.tool_result_max_chars,
        status_display_name: config.status_display_name.clone(),
        agent_config: config.agent_config.clone(),
    }
}

#[allow(dead_code)]
fn _keep_type_imports_used(_: Option<(CoreActor, CoreGroupMemberRole, Platform)>) {}
