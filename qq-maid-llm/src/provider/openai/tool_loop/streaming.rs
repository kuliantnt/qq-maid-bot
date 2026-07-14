//! OpenAI Responses SSE 收集、状态跟踪与最终 AgentStep 生成。

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::{Value, json};

use crate::{
    agent_loop::{AgentStep, AgentStreamingDiagnostics, AgentTextDeltaSink, AgentToolCall},
    error::LlmError,
    metrics::MetricsRecorder,
    sse::{SseFrame, parse_sse_frame, take_sse_frame},
};

use crate::provider::openai::{
    extract::{extract_response_output_text, extract_response_usage},
    responses::{incomplete_stream_eof_error, stream_transport_error},
    stream::{
        handle_openai_chat_stream_event, is_openai_responses_done_sentinel,
        responses_stream_is_complete,
    },
};

use super::{
    diagnostics::{
        set_streaming_fallback_reason, sync_responses_stream_diagnostics,
        update_streaming_diagnostics,
    },
    response::{append_response_output_items, extract_function_calls},
};

pub(super) async fn collect_responses_tool_loop_stream(
    mut response: reqwest::Response,
    input: &mut Vec<Value>,
    allow_tool_calls: bool,
    text_delta_sink: AgentTextDeltaSink,
    diagnostics: Arc<Mutex<AgentStreamingDiagnostics>>,
    activity_counter: Arc<AtomicUsize>,
) -> Result<AgentStep, LlmError> {
    let mut frame_buffer = Vec::new();
    let mut recorder = MetricsRecorder::start();
    let mut answer = String::new();
    let mut buffered_deltas = Vec::new();
    let mut completed_response = None;
    let mut saw_completed = false;
    let mut active_function_calls = HashSet::new();
    let mut completed_output_items = Vec::new();
    loop {
        while let Some(frame) = take_sse_frame(&mut frame_buffer) {
            let Some(event) = parse_sse_frame(&frame).inspect_err(|_| {
                set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
            })?
            else {
                continue;
            };
            update_streaming_diagnostics(&diagnostics, |item| item.sse_event_count += 1);
            activity_counter.fetch_add(1, Ordering::SeqCst);
            if is_openai_responses_done_sentinel(&event.data) {
                update_streaming_diagnostics(&diagnostics, |item| item.saw_done = true);
                if responses_stream_is_complete(saw_completed, &completed_response) {
                    sync_responses_stream_diagnostics(
                        &diagnostics,
                        saw_completed,
                        buffered_deltas.len(),
                        active_function_calls.len(),
                    );
                    return finalize_responses_tool_loop_stream(
                        input,
                        allow_tool_calls,
                        text_delta_sink,
                        answer,
                        buffered_deltas,
                        completed_response,
                        saw_completed,
                    )
                    .await;
                }
                if active_function_calls.is_empty()
                    && (!completed_output_items.is_empty() || !answer.trim().is_empty())
                {
                    completed_response = Some(json!({
                        "output_text": answer.clone(),
                        "output": completed_output_items.clone(),
                    }));
                    saw_completed = true;
                    sync_responses_stream_diagnostics(
                        &diagnostics,
                        saw_completed,
                        buffered_deltas.len(),
                        active_function_calls.len(),
                    );
                    return finalize_responses_tool_loop_stream(
                        input,
                        allow_tool_calls,
                        text_delta_sink,
                        answer,
                        buffered_deltas,
                        completed_response,
                        saw_completed,
                    )
                    .await;
                }
                continue;
            }
            observe_responses_function_call_event(
                &event,
                &mut active_function_calls,
                &mut completed_output_items,
            )
            .inspect_err(|_| {
                set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
            })?;
            recorder.mark_event();
            match handle_openai_chat_stream_event(
                event,
                &mut recorder,
                &mut answer,
                &mut completed_response,
                &mut saw_completed,
            )
            .inspect_err(|err| {
                if err.stage == "sse" && err.message.starts_with("invalid ") {
                    set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
                }
            })? {
                Some(delta) if allow_tool_calls => buffered_deltas.push(delta),
                Some(delta) => text_delta_sink(delta).await?,
                None => {}
            }
            sync_responses_stream_diagnostics(
                &diagnostics,
                saw_completed,
                buffered_deltas.len(),
                active_function_calls.len(),
            );
            if responses_stream_is_complete(saw_completed, &completed_response) {
                return finalize_responses_tool_loop_stream(
                    input,
                    allow_tool_calls,
                    text_delta_sink,
                    answer,
                    buffered_deltas,
                    completed_response,
                    saw_completed,
                )
                .await;
            }
        }

        match response.chunk().await {
            Ok(Some(chunk)) => {
                update_streaming_diagnostics(&diagnostics, |item| item.chunk_count += 1);
                frame_buffer.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(err) => {
                set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
                return Err(stream_transport_error(
                    format!("OpenAI tool loop stream failed: {err}"),
                    &answer,
                ));
            }
        }
    }

    if !frame_buffer.is_empty() {
        let Some(event) = parse_sse_frame(&frame_buffer).inspect_err(|_| {
            set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
        })?
        else {
            frame_buffer.clear();
            return finalize_responses_tool_loop_stream(
                input,
                allow_tool_calls,
                text_delta_sink,
                answer,
                buffered_deltas,
                completed_response,
                saw_completed,
            )
            .await;
        };
        update_streaming_diagnostics(&diagnostics, |item| item.sse_event_count += 1);
        activity_counter.fetch_add(1, Ordering::SeqCst);
        if is_openai_responses_done_sentinel(&event.data) {
            update_streaming_diagnostics(&diagnostics, |item| item.saw_done = true);
        }
        if !is_openai_responses_done_sentinel(&event.data) {
            recorder.mark_event();
            match handle_openai_chat_stream_event(
                event,
                &mut recorder,
                &mut answer,
                &mut completed_response,
                &mut saw_completed,
            )? {
                Some(delta) if allow_tool_calls => buffered_deltas.push(delta),
                Some(delta) => text_delta_sink(delta).await?,
                None => {}
            }
        }
    }

    sync_responses_stream_diagnostics(
        &diagnostics,
        saw_completed,
        buffered_deltas.len(),
        active_function_calls.len(),
    );

    finalize_responses_tool_loop_stream(
        input,
        allow_tool_calls,
        text_delta_sink,
        answer,
        buffered_deltas,
        completed_response,
        saw_completed,
    )
    .await
}

pub(super) fn observe_responses_function_call_event(
    event: &SseFrame,
    active_function_calls: &mut HashSet<u64>,
    completed_output_items: &mut Vec<Value>,
) -> Result<(), LlmError> {
    let value = serde_json::from_str::<Value>(&event.data).map_err(|err| {
        LlmError::provider(
            format!("invalid OpenAI tool loop stream JSON: {err}"),
            "sse",
        )
    })?;
    let event_type = event
        .event
        .as_deref()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");
    let output_index = value.get("output_index").and_then(Value::as_u64);
    match event_type {
        "response.output_item.added" => {
            if value
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(Value::as_str)
                == Some("function_call")
                && let Some(index) = output_index
            {
                active_function_calls.insert(index);
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(index) = output_index {
                active_function_calls.insert(index);
            }
        }
        "response.output_item.done" => {
            if let Some(item) = value.get("item")
                && item.get("type").and_then(Value::as_str) == Some("function_call")
            {
                completed_output_items.push(item.clone());
                if let Some(index) = output_index {
                    active_function_calls.remove(&index);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) async fn finalize_responses_tool_loop_stream(
    input: &mut Vec<Value>,
    allow_tool_calls: bool,
    text_delta_sink: AgentTextDeltaSink,
    mut answer: String,
    buffered_deltas: Vec<String>,
    completed_response: Option<Value>,
    saw_completed: bool,
) -> Result<AgentStep, LlmError> {
    if !saw_completed {
        return Err(incomplete_stream_eof_error(
            "OpenAI Responses tool loop stream ended before response.completed",
            &answer,
        ));
    }
    let body = completed_response.ok_or_else(|| {
        LlmError::provider(
            "OpenAI Responses tool loop stream completed without response body",
            "sse",
        )
    })?;
    let step_usage = extract_response_usage(&body);
    let calls = extract_function_calls(&body)?;
    if !calls.is_empty() {
        if !allow_tool_calls {
            return Err(LlmError::new(
                "tool_loop_limit",
                "tool loop returned tool calls when tool calls are disabled",
                "tool_loop",
            ));
        }
        append_response_output_items(input, &body)?;
        return Ok(AgentStep::ToolCalls {
            calls: calls
                .into_iter()
                .map(|call| AgentToolCall {
                    name: call.name,
                    call_id: call.call_id,
                    arguments: call.arguments,
                })
                .collect(),
            usage: step_usage,
        });
    }

    if answer.trim().is_empty()
        && let Some(completed_answer) = extract_response_output_text(&body)
        && !completed_answer.trim().is_empty()
    {
        answer = completed_answer;
    }
    if answer.trim().is_empty() {
        return Err(LlmError::provider(
            "OpenAI tool loop returned empty final text output",
            "provider",
        ));
    }
    if allow_tool_calls {
        if buffered_deltas.is_empty() {
            text_delta_sink(answer.clone()).await?;
        } else {
            for delta in buffered_deltas {
                text_delta_sink(delta).await?;
            }
        }
    }
    Ok(AgentStep::FinalAnswer {
        reply: answer,
        usage: step_usage,
    })
}
