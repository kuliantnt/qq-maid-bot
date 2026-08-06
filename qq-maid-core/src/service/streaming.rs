use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{sync::mpsc, time::timeout};
use tracing::{debug, warn};

use qq_maid_llm::agent_loop::{
    AgentRunDiagnostics, AgentRunHandle, AgentStopReason, AgentTextDeltaDelivery,
    AgentTextDeltaFuture, AgentTextDeltaSink, ToolLoopProgressEvent, ToolLoopProgressSink,
};
use qq_maid_llm::tool::DEFAULT_TOOL_TIMEOUT;

use crate::{
    error::LlmError,
    runtime::respond::{
        PlannedRespond, RespondPlan, RespondRequest, RespondResponse, RustRespondService,
        StatusAudience, StatusHint, StatusPhase, status_hint_for_tool_name, status_hint_text,
    },
};

use super::{
    CoreDeliveryHint, CoreError, CoreOutputPolicy, CoreRespondFailure, CoreResponse,
    CoreResponseEvent, CoreResponseStatus, CoreResponseStatusKind, CoreResponseStream,
    warn_core_error,
};

const AGENT_RUNNING_STATUS_DELAY: Duration = Duration::from_millis(1500);
const PARTIAL_RESPONSE_TIMEOUT_SUFFIX: &str = "\n\n（处理耗时过长，本次回答未完整完成。）";

#[derive(Debug, Clone)]
pub(crate) struct ProgressStatusConfig {
    pub hint: StatusHint,
    pub audience: StatusAudience,
    pub display_name: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AgentRequestBudget {
    pub request_timeout: Duration,
    pub finalization_reserve: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamDeliveryConfig {
    pub output_policy: CoreOutputPolicy,
    pub provider_stream_enabled: bool,
    pub delivery_hint: Option<CoreDeliveryHint>,
}

#[derive(Clone)]
struct AgentStreamControl {
    cancelled: Arc<AtomicBool>,
    run_handle: Option<AgentRunHandle>,
    visible_text_sent: Arc<AtomicBool>,
    buffered_final_text: Arc<Mutex<String>>,
}

#[derive(Clone)]
struct AgentFinalTextState {
    tool_activity_started: Arc<AtomicBool>,
    visible_text_sent: Arc<AtomicBool>,
    buffered_final_text: Arc<Mutex<String>>,
}

pub(crate) fn start_core_response_stream(
    service: RustRespondService,
    req: RespondRequest,
    planned: PlannedRespond,
    delivery: StreamDeliveryConfig,
    request_budget: AgentRequestBudget,
    progress_status: ProgressStatusConfig,
) -> CoreResponseStream {
    let (tx, receiver) = mpsc::channel(16);
    let cancelled = Arc::new(AtomicBool::new(false));
    let producer_cancelled = cancelled.clone();
    let scope_key = req.scope_key.clone();
    let plan = planned.plan();
    let agent_run_handle = matches!(plan, RespondPlan::AgentRuntime).then(|| {
        AgentRunHandle::with_timeout_and_finalization_reserve(
            request_budget.request_timeout,
            request_budget.finalization_reserve,
        )
    });
    let producer_agent_run_handle = agent_run_handle.clone();
    let visible_text_sent = Arc::new(AtomicBool::new(false));
    let producer_visible_text_sent = visible_text_sent.clone();
    let buffered_final_text = Arc::new(Mutex::new(String::new()));
    let producer_buffered_final_text = buffered_final_text.clone();
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
                    visible_text_sent: producer_visible_text_sent.clone(),
                    buffered_final_text: producer_buffered_final_text.clone(),
                },
                delivery.provider_stream_enabled,
                progress_status,
            ));
            match timeout(request_budget.request_timeout, &mut task).await {
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
                    let needs_side_effect_cleanup = producer_agent_run_handle
                        .as_ref()
                        .is_some_and(|handle| needs_side_effect_cleanup(&handle.snapshot()));
                    // 结果未知的写操作保留有限清理窗口，避免中断后伪装成未执行；
                    // 纯只读调用可立即取消，不额外占用副作用工具的 15 秒预算。
                    cleanup_timed_out_agent_task(&mut task, needs_side_effect_cleanup).await;
                    // 工具活动后的 Provider 文本仍是未验真的最终草稿。超时路径必须
                    // 丢弃而非释放，避免把工具参数或不完整回答升级成用户正文。
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
                    visible_text_sent: producer_visible_text_sent.clone(),
                    buffered_final_text: producer_buffered_final_text.clone(),
                },
                delivery.provider_stream_enabled,
                progress_status,
            )
            .await
        };
        if producer_cancelled.load(Ordering::SeqCst) {
            return;
        }
        let event = match result {
            Ok(response) if response.ok => {
                let response = CoreResponse::from(response)
                    .with_delivery_hint_if_eligible(delivery.delivery_hint);
                CoreResponseEvent::Completed(Box::new(response))
            }
            Ok(response) => {
                let err = response.error.map(CoreError::from).unwrap_or_else(|| {
                    CoreError::new("internal_error", "respond", "处理失败，请稍后再试")
                });
                warn!(
                    scope_key,
                    error_code = err.code,
                    error_stage = err.stage,
                    "Core 流式响应返回业务错误"
                );
                CoreResponseEvent::Failed(CoreRespondFailure::from_core_error(&err))
            }
            Err(err) => {
                warn_core_error(&scope_key, &err);
                if err.code == "timeout" && producer_visible_text_sent.load(Ordering::SeqCst) {
                    let _ = tx
                        .send(CoreResponseEvent::TextDelta(
                            PARTIAL_RESPONSE_TIMEOUT_SUFFIX.to_owned(),
                        ))
                        .await;
                }
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
        output_policy: delivery.output_policy,
        agent_run_handle,
    }
}

fn needs_side_effect_cleanup(diagnostics: &AgentRunDiagnostics) -> bool {
    diagnostics.tools_with_unknown_result.iter().any(|tool| {
        diagnostics
            .side_effecting_tools_started
            .iter()
            .any(|started| started == tool)
    })
}

async fn cleanup_timed_out_agent_task<T>(
    task: &mut tokio::task::JoinHandle<T>,
    needs_side_effect_cleanup: bool,
) {
    if needs_side_effect_cleanup && timeout(DEFAULT_TOOL_TIMEOUT, &mut *task).await.is_ok() {
        return;
    }
    task.abort();
    let _ = task.await;
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
        // WebSearch 不套用 AgentRuntime 整体超时：联网查询由统一搜索流实现维护
        // 首活动、静默和独立绝对上限，持续零碎 delta 也不能绕过绝对超时。
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
            "Core 流已完成，未补充合成的最终增量"
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
        "Core 命令事件流已完成"
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
        "Core 联网搜索流已完成"
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
    let visible_text_sent = control.visible_text_sent;
    let buffered_final_text = control.buffered_final_text;
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
    // 工具结果还要经过领域投影；在投影完成前不能把模型最终草稿直接发给平台，
    // 否则失败回执会被 Gateway 视为第二条回复。工具执行异常时再把暂存草稿原样
    // 释放，保留“已发出部分正文后不能伪造重放”的既有流式语义。
    let final_text_state = AgentFinalTextState {
        tool_activity_started: tool_activity_started.clone(),
        visible_text_sent: visible_text_sent.clone(),
        buffered_final_text: buffered_final_text.clone(),
    };
    let progress_sink = tool_loop_progress_sink(
        tx.clone(),
        cancelled.clone(),
        progress_status.clone(),
        tool_activity_started.clone(),
        eager_agent_status,
    );
    let finalizing_status_sent = Arc::new(AtomicBool::new(false));
    let final_delta_sink = if provider_stream_enabled {
        Some(agent_final_delta_sink(
            tx.clone(),
            cancelled.clone(),
            progress_status.clone(),
            finalizing_status_sent.clone(),
            eager_agent_status,
            final_text_state,
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

    let response = match loop {
        tokio::select! {
            result = &mut respond_future => break result,
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
    } {
        Ok(response) => response,
        Err(error) => {
            // 工具执行后的暂存文本只有在整轮成功并完成领域投影后才能外发。
            // response.incomplete、协议错误或超时都直接丢弃，不能伪装成部分正文。
            let _ = take_buffered_final_text(&buffered_final_text)?;
            return Err(error);
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
    send_postprocessed_agent_response(
        &tx,
        &cancelled,
        &visible_text_sent,
        &buffered_final_text,
        provider_stream_enabled,
        &tool_activity_started,
        &response,
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
        "Core Agent 对话已完成并产生进度状态事件"
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
    final_text_state: AgentFinalTextState,
) -> AgentTextDeltaSink {
    Arc::new(move |delta| {
        let tx = tx.clone();
        let cancelled = cancelled.clone();
        let progress_status = progress_status.clone();
        let finalizing_status_sent = finalizing_status_sent.clone();
        let final_text_state = final_text_state.clone();
        Box::pin(async move {
            send_agent_finalizing_status_once(
                &tx,
                &cancelled,
                &progress_status,
                &finalizing_status_sent,
                eager_agent_status,
                &final_text_state.tool_activity_started,
            )
            .await?;
            if final_text_state
                .tool_activity_started
                .load(Ordering::SeqCst)
            {
                let mut buffered = final_text_state
                    .buffered_final_text
                    .lock()
                    .map_err(|_| agent_final_text_buffer_error("写入"))?;
                buffered.push_str(&delta);
                return Ok(AgentTextDeltaDelivery::Buffered);
            }
            send_core_delta(&tx, &cancelled, delta).await?;
            final_text_state
                .visible_text_sent
                .store(true, Ordering::SeqCst);
            Ok(AgentTextDeltaDelivery::Visible)
        }) as AgentTextDeltaFuture
    })
}

/// 工具调用结束后，`RespondResponse` 已经包含领域可信回执和必要的模型总结，
/// 因此只发送这一份最终正文，让 Gateway 能把累计正文与 Completed 对齐。
async fn send_postprocessed_agent_response(
    tx: &mpsc::Sender<CoreResponseEvent>,
    cancelled: &Arc<AtomicBool>,
    visible_text_sent: &Arc<AtomicBool>,
    buffered_final_text: &Arc<Mutex<String>>,
    provider_stream_enabled: bool,
    tool_activity_started: &Arc<AtomicBool>,
    response: &RespondResponse,
) -> Result<(), LlmError> {
    // `ProgressThenComplete` 的 Provider 没有可用的文本传输通道，最终正文只能
    // 由外层 `Completed` 交付；即使诊断里存在工具结果，也不能在这里伪造 delta。
    if !provider_stream_enabled {
        return Ok(());
    }
    let buffered = take_buffered_final_text(buffered_final_text)?;
    if !tool_activity_started.load(Ordering::SeqCst) {
        if buffered.trim().is_empty() {
            return Ok(());
        }
        send_core_delta(tx, cancelled, buffered).await?;
        visible_text_sent.store(true, Ordering::SeqCst);
        return Ok(());
    }
    if !response.ok {
        return Ok(());
    }
    let Some(content) =
        response_visible_content(response).filter(|content| !content.trim().is_empty())
    else {
        return Ok(());
    };
    // `buffered` 仅用于保留异常路径的草稿；正常完成时必须丢弃，不能把未投影的
    // 模型文本与确定性工具回执再次拼接。
    let _ = buffered;
    send_core_delta(tx, cancelled, content.to_owned()).await?;
    visible_text_sent.store(true, Ordering::SeqCst);
    Ok(())
}

fn take_buffered_final_text(buffered_final_text: &Arc<Mutex<String>>) -> Result<String, LlmError> {
    let mut buffered = buffered_final_text
        .lock()
        .map_err(|_| agent_final_text_buffer_error("读取"))?;
    Ok(std::mem::take(&mut *buffered))
}

fn agent_final_text_buffer_error(action: &str) -> LlmError {
    LlmError::new(
        "internal_error",
        format!("{action} Agent 最终文本缓冲区失败"),
        "stream",
    )
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
    eager_agent_status: bool,
) -> ToolLoopProgressSink {
    let first_tool_event_seen = Arc::new(AtomicBool::new(false));
    std::sync::Arc::new(move |event| {
        let tx = tx.clone();
        let cancelled = cancelled.clone();
        let progress_status = progress_status.clone();
        let tool_activity_started = tool_activity_started.clone();
        let first_tool_event_seen = first_tool_event_seen.clone();
        Box::pin(async move {
            tool_activity_started.store(true, Ordering::SeqCst);
            let is_first_tool_event = !first_tool_event_seen.swap(true, Ordering::SeqCst);
            let (kind, phase, hint) = match event {
                ToolLoopProgressEvent::ToolCallStarted { tool_name } => {
                    let hint = status_hint_for_tool_name(&tool_name)
                        .unwrap_or_else(StatusHint::processing);
                    // 启动阶段已经发送同一条高置信状态时，不重复发送首个 ToolStarted；
                    // 后续工具仍按真实结构化事件更新，避免状态停留在原始文本猜测上。
                    if is_first_tool_event && eager_agent_status && hint == progress_status.hint {
                        return Ok(());
                    }
                    (
                        CoreResponseStatusKind::ToolCallStarted,
                        StatusPhase::Started,
                        hint,
                    )
                }
                ToolLoopProgressEvent::ToolCallFinished { tool_name } => (
                    CoreResponseStatusKind::ToolCallFinished,
                    StatusPhase::Finalizing,
                    status_hint_for_tool_name(&tool_name).unwrap_or_else(StatusHint::processing),
                ),
                ToolLoopProgressEvent::ToolCallFailed { tool_name } => (
                    CoreResponseStatusKind::ToolCallFailed,
                    StatusPhase::Finalizing,
                    status_hint_for_tool_name(&tool_name).unwrap_or_else(StatusHint::processing),
                ),
            };
            send_core_status(
                &tx,
                &cancelled,
                kind,
                status_hint_text(
                    progress_status.audience,
                    hint,
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

#[cfg(test)]
mod cleanup_tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use qq_maid_llm::agent_loop::AgentRunDiagnostics;

    use super::{cleanup_timed_out_agent_task, needs_side_effect_cleanup};

    #[tokio::test]
    async fn read_only_timeout_cleanup_aborts_without_side_effect_window() {
        let diagnostics = AgentRunDiagnostics {
            executed_tools: vec!["web_search".to_owned()],
            ..AgentRunDiagnostics::default()
        };
        assert!(!needs_side_effect_cleanup(&diagnostics));
        let mut task = tokio::spawn(std::future::pending::<()>());

        tokio::time::timeout(
            Duration::from_millis(100),
            cleanup_timed_out_agent_task(&mut task, false),
        )
        .await
        .expect("read-only cleanup must not wait for the side-effect timeout");
    }

    #[tokio::test]
    async fn unknown_side_effect_cleanup_keeps_limited_completion_window() {
        let diagnostics = AgentRunDiagnostics {
            executed_tools: vec!["write_tool".to_owned()],
            side_effecting_tools_started: vec!["write_tool".to_owned()],
            tools_with_unknown_result: vec!["write_tool".to_owned()],
            ..AgentRunDiagnostics::default()
        };
        assert!(needs_side_effect_cleanup(&diagnostics));
        let completed = Arc::new(AtomicBool::new(false));
        let task_completed = completed.clone();
        let mut task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            task_completed.store(true, Ordering::SeqCst);
        });

        cleanup_timed_out_agent_task(&mut task, true).await;

        assert!(completed.load(Ordering::SeqCst));
    }
}
