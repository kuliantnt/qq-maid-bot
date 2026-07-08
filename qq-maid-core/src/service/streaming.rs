use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{sync::mpsc, time::timeout};
use tracing::{debug, warn};

use crate::{
    error::LlmError,
    runtime::respond::{
        RespondPlan, RespondRequest, RespondResponse, RustRespondService, StatusAudience,
        StatusHint, StatusPhase, status_hint_text,
    },
};

use super::{
    CoreError, CoreOutputPolicy, CoreRespondFailure, CoreResponseEvent, CoreResponseStatus,
    CoreResponseStatusKind, CoreResponseStream, warn_core_error,
};

const TOOL_LOOP_RUNNING_STATUS_DELAY: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone)]
pub(crate) struct ProgressStatusConfig {
    pub hint: StatusHint,
    pub audience: StatusAudience,
    pub display_name: String,
}

pub(crate) fn start_core_response_stream(
    service: RustRespondService,
    req: RespondRequest,
    plan: RespondPlan,
    output_policy: CoreOutputPolicy,
    provider_stream_enabled: bool,
    request_timeout: Duration,
    progress_status: ProgressStatusConfig,
) -> CoreResponseStream {
    let (tx, receiver) = mpsc::channel(16);
    let cancelled = Arc::new(AtomicBool::new(false));
    let producer_cancelled = cancelled.clone();
    let scope_key = req.scope_key.clone();
    tokio::spawn(async move {
        if producer_cancelled.load(Ordering::SeqCst) {
            let _ = tx
                .send(CoreResponseEvent::Failed(CoreRespondFailure::cancelled()))
                .await;
            return;
        }
        let respond_future = run_streaming_respond(
            &service,
            req,
            plan,
            tx.clone(),
            producer_cancelled.clone(),
            provider_stream_enabled,
            progress_status,
        );
        let result = if matches!(plan, RespondPlan::CompleteToolLoop) {
            match timeout(request_timeout, respond_future).await {
                Ok(result) => result,
                Err(_) => Err(LlmError::timeout("request")),
            }
        } else {
            respond_future.await
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
    }
}

async fn run_streaming_respond(
    service: &RustRespondService,
    req: RespondRequest,
    plan: RespondPlan,
    tx: mpsc::Sender<CoreResponseEvent>,
    cancelled: Arc<AtomicBool>,
    provider_stream_enabled: bool,
    progress_status: ProgressStatusConfig,
) -> Result<RespondResponse, LlmError> {
    if matches!(plan, RespondPlan::CompleteToolLoop) {
        return run_complete_tool_loop_respond(service, req, tx, cancelled, progress_status).await;
    }
    if matches!(plan, RespondPlan::WebSearch) && provider_stream_enabled {
        // WebSearch 不套用 CompleteToolLoop 整体超时：联网查询复用 `/查` 的流式
        // `WebSearchTool::query_stream`，只要持续有有效片段就不被长等待窗口误杀。
        // provider 不支持流式时改由下面聚合路径走 `respond_with_plan`，
        // dispatcher 会按 WebSearch plan 聚合查询后一次性发送。
        return run_web_search_respond(service, req, tx, cancelled).await;
    }
    if !provider_stream_enabled {
        let response = service.respond_with_plan(req, plan).await?;
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
        .respond_stream(req, |delta| {
            let tx = tx.clone();
            let cancelled = cancelled.clone();
            Box::pin(async move { send_core_delta(&tx, &cancelled, delta).await })
        })
        .await
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

async fn run_complete_tool_loop_respond(
    service: &RustRespondService,
    req: RespondRequest,
    tx: mpsc::Sender<CoreResponseEvent>,
    cancelled: Arc<AtomicBool>,
    progress_status: ProgressStatusConfig,
) -> Result<RespondResponse, LlmError> {
    send_core_status(
        &tx,
        &cancelled,
        CoreResponseStatusKind::ToolLoopStarted,
        status_hint_text(
            progress_status.audience,
            progress_status.hint,
            StatusPhase::Started,
            &progress_status.display_name,
        ),
    )
    .await?;

    let respond_future = service.respond_with_plan(req, RespondPlan::CompleteToolLoop);
    tokio::pin!(respond_future);
    let mut running_status_sent = false;

    let response = loop {
        tokio::select! {
            result = &mut respond_future => break result?,
            _ = tokio::time::sleep(TOOL_LOOP_RUNNING_STATUS_DELAY), if !running_status_sent => {
                running_status_sent = true;
                send_core_status(
                    &tx,
                    &cancelled,
                    CoreResponseStatusKind::ToolLoopRunning,
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

    send_core_status(
        &tx,
        &cancelled,
        CoreResponseStatusKind::ToolLoopFinalizing,
        status_hint_text(
            progress_status.audience,
            progress_status.hint,
            StatusPhase::Finalizing,
            &progress_status.display_name,
        ),
    )
    .await?;

    debug!(
        respond_plan = respond_plan_name(RespondPlan::CompleteToolLoop),
        provider_stream_enabled = false,
        synthetic_final_delta = false,
        response_delivery_mode =
            output_policy_for_stream(RespondPlan::CompleteToolLoop, false).as_str(),
        final_chars = response_visible_content(&response)
            .map(|content| content.chars().count())
            .unwrap_or_default(),
        "core tool loop stream completed with progress status events"
    );

    Ok(response)
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

fn response_visible_content(response: &RespondResponse) -> Option<&str> {
    response.markdown.as_deref().or(response.text.as_deref())
}

fn respond_plan_name(plan: RespondPlan) -> &'static str {
    match plan {
        RespondPlan::Immediate => "immediate",
        RespondPlan::StreamingChat => "streaming_chat",
        RespondPlan::CompleteToolLoop => "complete_tool_loop",
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
        RespondPlan::CompleteToolLoop => CoreOutputPolicy::ProgressThenComplete,
        // WebSearch 复用 `/查` 的流式查询能力：provider 支持流式时直出，
        // 否则聚合后一次性发送，避免长时间非流式阻塞导致业务超时。
        RespondPlan::WebSearch if provider_stream_enabled => CoreOutputPolicy::DirectStream,
        RespondPlan::WebSearch => CoreOutputPolicy::CompleteThenSend,
        RespondPlan::Immediate => CoreOutputPolicy::CompleteThenSend,
    }
}
