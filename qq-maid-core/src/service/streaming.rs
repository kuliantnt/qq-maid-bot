use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{sync::mpsc, time::timeout};
use tracing::{debug, warn};

use qq_maid_llm::agent_loop::{
    AgentRunHandle, AgentStopReason, AgentTextDeltaFuture, AgentTextDeltaSink,
    ToolLoopProgressEvent, ToolLoopProgressSink,
};
use qq_maid_llm::tool::DEFAULT_TOOL_TIMEOUT;

use crate::{
    error::LlmError,
    runtime::respond::{
        PlannedRespond, RespondPlan, RespondRequest, RespondResponse, RustRespondService,
        StatusAudience, StatusHint, StatusPhase, status_hint_text,
    },
};

use super::{
    CoreError, CoreOutputPolicy, CoreRespondFailure, CoreResponseEvent, CoreResponseStatus,
    CoreResponseStatusKind, CoreResponseStream, warn_core_error,
};

const AGENT_RUNNING_STATUS_DELAY: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone)]
pub(crate) struct ProgressStatusConfig {
    pub hint: StatusHint,
    pub audience: StatusAudience,
    pub display_name: String,
}

#[derive(Clone)]
struct AgentStreamControl {
    cancelled: Arc<AtomicBool>,
    run_handle: Option<AgentRunHandle>,
}

pub(crate) fn start_core_response_stream(
    service: RustRespondService,
    req: RespondRequest,
    planned: PlannedRespond,
    output_policy: CoreOutputPolicy,
    provider_stream_enabled: bool,
    request_timeout: Duration,
    progress_status: ProgressStatusConfig,
) -> CoreResponseStream {
    let (tx, receiver) = mpsc::channel(16);
    let cancelled = Arc::new(AtomicBool::new(false));
    let producer_cancelled = cancelled.clone();
    let scope_key = req.scope_key.clone();
    let plan = planned.plan();
    let agent_run_handle = matches!(plan, RespondPlan::AgentRuntime).then(AgentRunHandle::default);
    let producer_agent_run_handle = agent_run_handle.clone();
    tokio::spawn(async move {
        if producer_cancelled.load(Ordering::SeqCst) {
            let _ = tx
                .send(CoreResponseEvent::Failed(CoreRespondFailure::cancelled(
                    producer_agent_run_handle.as_ref(),
                )))
                .await;
            return;
        }
        let result = if matches!(plan, RespondPlan::AgentRuntime) {
            let mut task = tokio::spawn(run_streaming_respond(
                service,
                req,
                planned,
                tx.clone(),
                AgentStreamControl {
                    cancelled: producer_cancelled.clone(),
                    run_handle: producer_agent_run_handle.clone(),
                },
                provider_stream_enabled,
                progress_status,
            ));
            match timeout(request_timeout, &mut task).await {
                Ok(result) => result.unwrap_or_else(|err| {
                    Err(LlmError::new(
                        "internal_error",
                        format!("agent respond task failed: {err}"),
                        "respond",
                    ))
                }),
                Err(_) => {
                    let err = LlmError::timeout("request");
                    if let Some(handle) = &producer_agent_run_handle {
                        handle.cancel(AgentStopReason::Timeout);
                    }
                    // 取消只阻止后续模型轮次和尚未启动的工具；已启动工具保留其自身
                    // timeout 完成可信结果。清理预算耗尽后 abort，unknown 轨迹继续保留。
                    if timeout(DEFAULT_TOOL_TIMEOUT, &mut task).await.is_err() {
                        task.abort();
                    }
                    Err(producer_agent_run_handle
                        .as_ref()
                        .map(|handle| err.clone().with_agent(handle.snapshot()))
                        .unwrap_or(err))
                }
            }
        } else {
            run_streaming_respond(
                service,
                req,
                planned,
                tx.clone(),
                AgentStreamControl {
                    cancelled: producer_cancelled.clone(),
                    run_handle: producer_agent_run_handle.clone(),
                },
                provider_stream_enabled,
                progress_status,
            )
            .await
        };
        if producer_cancelled.load(Ordering::SeqCst) {
            return;
        }
        let event = match result {
            Ok(response) if response.ok => CoreResponseEvent::Completed(Box::new(response.into())),
            Ok(response) => {
                let err = response.error.map(CoreError::from).unwrap_or_else(|| {
                    CoreError::new("internal_error", "respond", "处理失败，请稍后再试")
                });
                warn!(
                    scope_key,
                    error_code = err.code,
                    error_stage = err.stage,
                    "streaming core respond returned business error"
                );
                CoreResponseEvent::Failed(CoreRespondFailure::from_core_error(&err))
            }
            Err(err) => {
                warn_core_error(&scope_key, &err);
                CoreResponseEvent::Failed(CoreRespondFailure::from_llm_error(&err))
            }
        };
        if !producer_cancelled.load(Ordering::SeqCst) {
            let _ = tx.send(event).await;
        }
    });
    CoreResponseStream {
        receiver,
        cancelled,
        output_policy,
        agent_run_handle,
    }
}

async fn run_streaming_respond(
    service: RustRespondService,
    req: RespondRequest,
    planned: PlannedRespond,
    tx: mpsc::Sender<CoreResponseEvent>,
    control: AgentStreamControl,
    provider_stream_enabled: bool,
    progress_status: ProgressStatusConfig,
) -> Result<RespondResponse, LlmError> {
    let plan = planned.plan();
    if matches!(plan, RespondPlan::AgentRuntime) {
        return run_agent_runtime_respond(
            &service,
            req,
            planned,
            tx,
            control,
            progress_status,
            provider_stream_enabled,
        )
        .await;
    }
    let cancelled = control.cancelled;
    if matches!(plan, RespondPlan::CommandEvent) {
        return run_command_event_respond(&service, req, planned, tx, cancelled).await;
    }
    if matches!(plan, RespondPlan::WebSearch) && provider_stream_enabled {
        // WebSearch 不套用 AgentRuntime 整体超时：联网查询复用 `/查` 的流式
        // `WebSearchTool::query_stream`，只要持续有有效片段就不被长等待窗口误杀。
        // provider 不支持流式时改由下面聚合路径走 `respond_with_plan`，
        // dispatcher 会按 WebSearch plan 聚合查询后一次性发送。
        return run_web_search_respond(&service, req, tx, cancelled).await;
    }
    if !provider_stream_enabled {
        let response = service.respond_with_plan(req, planned).await?;
        debug!(
            respond_plan = respond_plan_name(plan),
            provider_stream_enabled,
            synthetic_final_delta = false,
            response_delivery_mode =
                output_policy_for_stream(plan, provider_stream_enabled).as_str(),
            final_chars = response_visible_content(&response)
                .map(|content| content.chars().count())
                .unwrap_or_default(),
            "core stream completed without synthetic final delta"
        );
        return Ok(response);
    }
    service
        .respond_stream_with_plan(req, planned, |delta| {
            let tx = tx.clone();
            let cancelled = cancelled.clone();
            Box::pin(async move { send_core_delta(&tx, &cancelled, delta).await })
        })
        .await
}

async fn run_command_event_respond(
    service: &RustRespondService,
    req: RespondRequest,
    planned: PlannedRespond,
    tx: mpsc::Sender<CoreResponseEvent>,
    cancelled: Arc<AtomicBool>,
) -> Result<RespondResponse, LlmError> {
    send_core_status(
        &tx,
        &cancelled,
        CoreResponseStatusKind::CommandStarted,
        "正在处理命令…".to_owned(),
    )
    .await?;
    let plan = planned.plan();
    let response = service.respond_with_plan(req, planned).await?;
    if !response.ok {
        return Ok(response);
    }
    send_core_status(
        &tx,
        &cancelled,
        CoreResponseStatusKind::CommandFinished,
        "命令处理完成。".to_owned(),
    )
    .await?;
    debug!(
        respond_plan = respond_plan_name(plan),
        final_chars = response_visible_content(&response)
            .map(|content| content.chars().count())
            .unwrap_or_default(),
        "core command event stream completed"
    );
    Ok(response)
}

async fn run_web_search_respond(
    service: &RustRespondService,
    req: RespondRequest,
    tx: mpsc::Sender<CoreResponseEvent>,
    cancelled: Arc<AtomicBool>,
) -> Result<RespondResponse, LlmError> {
    let response = service
        .respond_web_search_stream(req, |delta| {
            let tx = tx.clone();
            let cancelled = cancelled.clone();
            Box::pin(async move { send_core_delta(&tx, &cancelled, delta).await })
        })
        .await?;
    debug!(
        respond_plan = respond_plan_name(RespondPlan::WebSearch),
        synthetic_final_delta = false,
        final_chars = response_visible_content(&response)
            .map(|content| content.chars().count())
            .unwrap_or_default(),
        "core web search stream completed"
    );
    Ok(response)
}

async fn run_agent_runtime_respond(
    service: &RustRespondService,
    req: RespondRequest,
    planned: PlannedRespond,
    tx: mpsc::Sender<CoreResponseEvent>,
    control: AgentStreamControl,
    progress_status: ProgressStatusConfig,
    provider_stream_enabled: bool,
) -> Result<RespondResponse, LlmError> {
    let cancelled = control.cancelled;
    let agent_run_handle = control.run_handle;
    let eager_agent_status = planned.should_emit_eager_agent_status();
    if eager_agent_status {
        send_core_status(
            &tx,
            &cancelled,
            CoreResponseStatusKind::AgentStarted,
            status_hint_text(
                progress_status.audience,
                progress_status.hint,
                StatusPhase::Started,
                &progress_status.display_name,
            ),
        )
        .await?;
    }

    let tool_activity_started = Arc::new(AtomicBool::new(false));
    let progress_sink = tool_loop_progress_sink(
        tx.clone(),
        cancelled.clone(),
        progress_status.clone(),
        tool_activity_started.clone(),
    );
    let finalizing_status_sent = Arc::new(AtomicBool::new(false));
    let final_delta_sink = if provider_stream_enabled {
        Some(agent_final_delta_sink(
            tx.clone(),
            cancelled.clone(),
            progress_status.clone(),
            finalizing_status_sent.clone(),
            eager_agent_status,
            tool_activity_started.clone(),
        ))
    } else {
        None
    };
    let respond_future = service.respond_with_plan_and_progress(
        req,
        planned,
        Some(progress_sink),
        final_delta_sink,
        agent_run_handle,
    );
    tokio::pin!(respond_future);
    let mut running_status_sent = false;

    let response = loop {
        tokio::select! {
            result = &mut respond_future => break result?,
            _ = tokio::time::sleep(AGENT_RUNNING_STATUS_DELAY), if eager_agent_status && !running_status_sent => {
                running_status_sent = true;
                send_core_status(
                    &tx,
                    &cancelled,
                    CoreResponseStatusKind::AgentRunning,
                    status_hint_text(
                        progress_status.audience,
                        progress_status.hint,
                        StatusPhase::Running,
                        &progress_status.display_name,
                    ),
                ).await?;
            }
        }
    };

    send_agent_finalizing_status_once(
        &tx,
        &cancelled,
        &progress_status,
        &finalizing_status_sent,
        eager_agent_status,
        &tool_activity_started,
    )
    .await?;

    debug!(
        respond_plan = respond_plan_name(RespondPlan::AgentRuntime),
        provider_stream_enabled,
        synthetic_final_delta = false,
        response_delivery_mode =
            output_policy_for_stream(RespondPlan::AgentRuntime, provider_stream_enabled).as_str(),
        final_chars = response_visible_content(&response)
            .map(|content| content.chars().count())
            .unwrap_or_default(),
        "core agent chat completed with progress status events"
    );

    Ok(response)
}

async fn send_agent_finalizing_status_once(
    tx: &mpsc::Sender<CoreResponseEvent>,
    cancelled: &Arc<AtomicBool>,
    progress_status: &ProgressStatusConfig,
    finalizing_status_sent: &Arc<AtomicBool>,
    eager_agent_status: bool,
    tool_activity_started: &Arc<AtomicBool>,
) -> Result<(), LlmError> {
    if !eager_agent_status && !tool_activity_started.load(Ordering::SeqCst) {
        return Ok(());
    }
    if finalizing_status_sent
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(());
    }
    send_core_status(
        tx,
        cancelled,
        CoreResponseStatusKind::AgentFinalizing,
        status_hint_text(
            progress_status.audience,
            progress_status.hint,
            StatusPhase::Finalizing,
            &progress_status.display_name,
        ),
    )
    .await
}

fn agent_final_delta_sink(
    tx: mpsc::Sender<CoreResponseEvent>,
    cancelled: Arc<AtomicBool>,
    progress_status: ProgressStatusConfig,
    finalizing_status_sent: Arc<AtomicBool>,
    eager_agent_status: bool,
    tool_activity_started: Arc<AtomicBool>,
) -> AgentTextDeltaSink {
    Arc::new(move |delta| {
        let tx = tx.clone();
        let cancelled = cancelled.clone();
        let progress_status = progress_status.clone();
        let finalizing_status_sent = finalizing_status_sent.clone();
        let tool_activity_started = tool_activity_started.clone();
        Box::pin(async move {
            send_agent_finalizing_status_once(
                &tx,
                &cancelled,
                &progress_status,
                &finalizing_status_sent,
                eager_agent_status,
                &tool_activity_started,
            )
            .await?;
            send_core_delta(&tx, &cancelled, delta).await
        }) as AgentTextDeltaFuture
    })
}

async fn send_core_delta(
    tx: &mpsc::Sender<CoreResponseEvent>,
    cancelled: &Arc<AtomicBool>,
    delta: String,
) -> Result<(), LlmError> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(LlmError::new("cancelled", "stream cancelled", "stream"));
    }
    tx.send(CoreResponseEvent::TextDelta(delta))
        .await
        .map_err(|_| LlmError::new("cancelled", "stream receiver dropped", "stream"))
}

async fn send_core_status(
    tx: &mpsc::Sender<CoreResponseEvent>,
    cancelled: &Arc<AtomicBool>,
    kind: CoreResponseStatusKind,
    text: String,
) -> Result<(), LlmError> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(LlmError::new("cancelled", "stream cancelled", "stream"));
    }
    tx.send(CoreResponseEvent::Status(CoreResponseStatus { kind, text }))
        .await
        .map_err(|_| LlmError::new("cancelled", "stream receiver dropped", "stream"))
}

fn tool_loop_progress_sink(
    tx: mpsc::Sender<CoreResponseEvent>,
    cancelled: Arc<AtomicBool>,
    progress_status: ProgressStatusConfig,
    tool_activity_started: Arc<AtomicBool>,
) -> ToolLoopProgressSink {
    std::sync::Arc::new(move |event| {
        let tx = tx.clone();
        let cancelled = cancelled.clone();
        let progress_status = progress_status.clone();
        let tool_activity_started = tool_activity_started.clone();
        Box::pin(async move {
            tool_activity_started.store(true, Ordering::SeqCst);
            let (kind, phase) = match event {
                ToolLoopProgressEvent::ToolCallStarted { .. } => (
                    CoreResponseStatusKind::ToolCallStarted,
                    StatusPhase::Started,
                ),
                ToolLoopProgressEvent::ToolCallFinished { .. } => (
                    CoreResponseStatusKind::ToolCallFinished,
                    StatusPhase::Finalizing,
                ),
                ToolLoopProgressEvent::ToolCallFailed { .. } => (
                    CoreResponseStatusKind::ToolCallFailed,
                    StatusPhase::Finalizing,
                ),
            };
            send_core_status(
                &tx,
                &cancelled,
                kind,
                status_hint_text(
                    progress_status.audience,
                    progress_status.hint,
                    phase,
                    &progress_status.display_name,
                ),
            )
            .await
        })
    })
}

fn response_visible_content(response: &RespondResponse) -> Option<&str> {
    response.markdown.as_deref().or(response.text.as_deref())
}

fn respond_plan_name(plan: RespondPlan) -> &'static str {
    match plan {
        RespondPlan::Immediate => "immediate",
        RespondPlan::CommandEvent => "command_event",
        RespondPlan::StreamingChat => "streaming_chat",
        RespondPlan::AgentRuntime => "agent_runtime",
        RespondPlan::WebSearch => "web_search",
    }
}

pub(crate) fn output_policy_for_stream(
    plan: RespondPlan,
    provider_stream_enabled: bool,
) -> CoreOutputPolicy {
    match plan {
        RespondPlan::StreamingChat if provider_stream_enabled => CoreOutputPolicy::DirectStream,
        RespondPlan::StreamingChat => CoreOutputPolicy::CompleteThenSend,
        RespondPlan::AgentRuntime if provider_stream_enabled => {
            CoreOutputPolicy::ProgressThenStream
        }
        RespondPlan::AgentRuntime => CoreOutputPolicy::ProgressThenComplete,
        // WebSearch 复用 `/查` 的流式查询能力：provider 支持流式时直出，
        // 否则聚合后一次性发送，避免长时间非流式阻塞导致业务超时。
        RespondPlan::WebSearch if provider_stream_enabled => CoreOutputPolicy::DirectStream,
        RespondPlan::WebSearch => CoreOutputPolicy::CompleteThenSend,
        RespondPlan::CommandEvent => CoreOutputPolicy::CompleteThenSend,
        RespondPlan::Immediate => CoreOutputPolicy::CompleteThenSend,
    }
}
