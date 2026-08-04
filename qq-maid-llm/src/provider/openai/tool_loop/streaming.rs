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
    responses::{incomplete_stream_eof_error, is_connection_reset_error, stream_transport_error},
    stream::{
        handle_openai_chat_stream_event, is_openai_responses_done_sentinel,
        responses_stream_is_complete,
    },
    tool_calls_disabled_error,
};

use super::{
    diagnostics::{
        set_streaming_fallback_reason, sync_responses_stream_diagnostics,
        update_streaming_diagnostics,
    },
    response::{append_response_output_items, extract_function_calls},
};

pub(super) struct StreamFinalization {
    pub(super) allow_tool_calls: bool,
    pub(super) answer: String,
    pub(super) buffered_deltas: Vec<String>,
    pub(super) completed_response: Option<Value>,
    pub(super) completion_confirmed: bool,
    pub(super) diagnostics: Arc<Mutex<AgentStreamingDiagnostics>>,
}

pub(super) async fn collect_responses_tool_loop_stream(
    mut response: reqwest::Response,
    input: &mut Vec<Value>,
    allow_tool_calls: bool,
    text_delta_sink: AgentTextDeltaSink,
    diagnostics: Arc<Mutex<AgentStreamingDiagnostics>>,
    activity_counter: Arc<AtomicUsize>,
) -> Result<AgentStep, LlmError> {
    update_streaming_diagnostics(&diagnostics, |item| {
        item.http_status = Some(response.status().as_u16());
    });
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
                mark_stream_parse_error(&diagnostics);
            })?
            else {
                continue;
            };
            observe_sse_event(&diagnostics, &event);
            activity_counter.fetch_add(1, Ordering::SeqCst);
            if is_openai_responses_done_sentinel(&event.data) {
                update_streaming_diagnostics(&diagnostics, |item| {
                    item.saw_done = true;
                    item.stream_end_kind = Some("done_sentinel".to_owned());
                });
                if responses_stream_is_complete(saw_completed, &completed_response) {
                    sync_responses_stream_diagnostics(
                        &diagnostics,
                        saw_completed,
                        buffered_deltas.len(),
                        buffered_text_chars(&buffered_deltas),
                        active_function_calls.len(),
                    );
                    return finalize_responses_tool_loop_stream(
                        input,
                        text_delta_sink,
                        StreamFinalization {
                            allow_tool_calls,
                            answer,
                            buffered_deltas,
                            completed_response,
                            completion_confirmed: true,
                            diagnostics,
                        },
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
                    sync_responses_stream_diagnostics(
                        &diagnostics,
                        saw_completed,
                        buffered_deltas.len(),
                        buffered_text_chars(&buffered_deltas),
                        active_function_calls.len(),
                    );
                    return finalize_responses_tool_loop_stream(
                        input,
                        text_delta_sink,
                        StreamFinalization {
                            allow_tool_calls,
                            answer,
                            buffered_deltas,
                            completed_response,
                            completion_confirmed: true,
                            diagnostics,
                        },
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
                mark_stream_parse_error(&diagnostics);
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
                    mark_stream_parse_error(&diagnostics);
                }
            })? {
                Some(delta) if allow_tool_calls => buffered_deltas.push(delta),
                Some(delta) => {
                    update_streaming_diagnostics(&diagnostics, |item| {
                        item.visible_text_chars += delta.chars().count();
                    });
                    text_delta_sink(delta).await?;
                }
                None => {}
            }
            sync_responses_stream_diagnostics(
                &diagnostics,
                saw_completed,
                buffered_deltas.len(),
                buffered_text_chars(&buffered_deltas),
                active_function_calls.len(),
            );
            if responses_stream_is_complete(saw_completed, &completed_response) {
                update_streaming_diagnostics(&diagnostics, |item| {
                    item.stream_end_kind = Some("response_completed".to_owned());
                });
                return finalize_responses_tool_loop_stream(
                    input,
                    text_delta_sink,
                    StreamFinalization {
                        allow_tool_calls,
                        answer,
                        buffered_deltas,
                        completed_response,
                        completion_confirmed: true,
                        diagnostics,
                    },
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
                let connection_reset = is_connection_reset_error(&err);
                let stream_end_kind = if err.is_timeout() {
                    "http_stream_timeout"
                } else if connection_reset {
                    "connection_reset"
                } else {
                    "http_stream_error"
                };
                update_streaming_diagnostics(&diagnostics, |item| {
                    item.connection_reset = connection_reset;
                    item.stream_end_kind = Some(stream_end_kind.to_owned());
                });
                return Err(stream_transport_error(
                    err,
                    "OpenAI tool loop stream failed",
                    &answer,
                ));
            }
        }
    }

    // `Response::chunk()` 返回 Ok(None) 才表示 HTTP body 正常 EOF。先记录传输层
    // 事实，再解析尾部残帧；若尾帧损坏，normal_eof 与 parse_error 会同时为 true，
    // stream_end_kind 则明确标记 SSE 截断，且绝不会进入兼容完成。
    update_streaming_diagnostics(&diagnostics, |item| item.normal_eof = true);

    if !frame_buffer.is_empty() {
        let Some(event) = parse_sse_frame(&frame_buffer).inspect_err(|_| {
            mark_stream_parse_error(&diagnostics);
        })?
        else {
            frame_buffer.clear();
            update_streaming_diagnostics(&diagnostics, |item| {
                item.parse_error = true;
                item.stream_end_kind = Some("sse_incomplete_frame".to_owned());
            });
            set_streaming_fallback_reason(&diagnostics, "sse_incomplete_frame");
            sync_responses_stream_diagnostics(
                &diagnostics,
                saw_completed,
                buffered_deltas.len(),
                buffered_text_chars(&buffered_deltas),
                active_function_calls.len(),
            );
            return Err(incomplete_stream_eof_error(
                "OpenAI Responses tool loop stream ended with an incomplete SSE frame",
                &answer,
            ));
        };
        observe_sse_event(&diagnostics, &event);
        activity_counter.fetch_add(1, Ordering::SeqCst);
        if is_openai_responses_done_sentinel(&event.data) {
            update_streaming_diagnostics(&diagnostics, |item| {
                item.saw_done = true;
                item.stream_end_kind = Some("done_sentinel".to_owned());
            });
        }
        if !is_openai_responses_done_sentinel(&event.data) {
            observe_responses_function_call_event(
                &event,
                &mut active_function_calls,
                &mut completed_output_items,
            )
            .inspect_err(|_| mark_stream_parse_error(&diagnostics))?;
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
                    mark_stream_parse_error(&diagnostics);
                }
            })? {
                Some(delta) if allow_tool_calls => buffered_deltas.push(delta),
                Some(delta) => {
                    update_streaming_diagnostics(&diagnostics, |item| {
                        item.visible_text_chars += delta.chars().count();
                    });
                    text_delta_sink(delta).await?;
                }
                None => {}
            }
        }
    }

    sync_responses_stream_diagnostics(
        &diagnostics,
        saw_completed,
        buffered_deltas.len(),
        buffered_text_chars(&buffered_deltas),
        active_function_calls.len(),
    );

    let explicit_completion = responses_stream_is_complete(saw_completed, &completed_response);
    let stream_diagnostics = diagnostics
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if explicit_completion {
        update_streaming_diagnostics(&diagnostics, |item| {
            item.stream_end_kind = Some("response_completed".to_owned());
        });
    }
    let done_completion = !explicit_completion
        && stream_diagnostics.saw_done
        && active_function_calls.is_empty()
        && (!answer.trim().is_empty() || !completed_output_items.is_empty());
    if done_completion {
        completed_response = Some(json!({
            "output_text": answer.clone(),
            "output": completed_output_items.clone(),
        }));
    }
    let compatible_eof_completion = !explicit_completion
        && !done_completion
        && !answer.trim().is_empty()
        && active_function_calls.is_empty()
        && !stream_diagnostics.explicit_failure_event
        && !stream_diagnostics.parse_error
        && !stream_diagnostics.connection_reset;
    if compatible_eof_completion {
        completed_response = Some(json!({
            "output_text": answer.clone(),
            "output": [],
        }));
        update_streaming_diagnostics(&diagnostics, |item| {
            item.stream_end_kind = Some("normal_eof_compatible_completion".to_owned());
        });
        tracing::warn!(
            http_status = response.status().as_u16(),
            saw_text_delta = !answer.trim().is_empty(),
            buffered_text_chars = buffered_text_chars(&buffered_deltas),
            active_function_call_count = active_function_calls.len(),
            last_sse_event_type = ?stream_diagnostics.last_sse_event_type,
            "OpenAI Responses stream used compatible completion after normal HTTP EOF"
        );
    } else if !explicit_completion {
        update_streaming_diagnostics(&diagnostics, |item| {
            item.stream_end_kind = Some(if active_function_calls.is_empty() {
                "normal_eof_no_content".to_owned()
            } else {
                "normal_eof_active_function_call".to_owned()
            });
        });
    }

    finalize_responses_tool_loop_stream(
        input,
        text_delta_sink,
        StreamFinalization {
            allow_tool_calls,
            answer,
            buffered_deltas,
            completed_response,
            completion_confirmed: explicit_completion
                || done_completion
                || compatible_eof_completion,
            diagnostics,
        },
    )
    .await
}

fn observe_sse_event(diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>, event: &SseFrame) {
    let event_type = if is_openai_responses_done_sentinel(&event.data) {
        Some("[DONE]".to_owned())
    } else {
        event.event.clone().or_else(|| {
            serde_json::from_str::<Value>(&event.data)
                .ok()
                .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        })
    };
    update_streaming_diagnostics(diagnostics, |item| {
        item.sse_event_count += 1;
        if let Some(event_type) = event_type {
            item.explicit_failure_event |= matches!(
                event_type.as_str(),
                "response.failed" | "response.incomplete" | "error"
            );
            if item.explicit_failure_event {
                item.stream_end_kind = Some("explicit_failure_event".to_owned());
            }
            item.last_sse_event_type = Some(event_type);
        }
    });
}

fn buffered_text_chars(buffered_deltas: &[String]) -> usize {
    buffered_deltas
        .iter()
        .map(|delta| delta.chars().count())
        .sum()
}

fn mark_stream_parse_error(diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>) {
    update_streaming_diagnostics(diagnostics, |item| {
        item.parse_error = true;
        item.stream_end_kind = Some("sse_parse_error".to_owned());
    });
    set_streaming_fallback_reason(diagnostics, "sse_parse_error");
}

pub(super) fn observe_responses_function_call_event(
    event: &SseFrame,
    active_function_calls: &mut HashSet<String>,
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
    let call_key = function_call_key(&value);
    match event_type {
        "response.output_item.added" => {
            if value
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(Value::as_str)
                == Some("function_call")
            {
                active_function_calls
                    .insert(call_key.unwrap_or_else(|| "unindexed_function_call".to_owned()));
            }
        }
        "response.function_call_arguments.delta" => {
            active_function_calls
                .insert(call_key.unwrap_or_else(|| "unindexed_function_call".to_owned()));
        }
        "response.output_item.done" => {
            if let Some(item) = value.get("item")
                && item.get("type").and_then(Value::as_str) == Some("function_call")
            {
                completed_output_items.push(item.clone());
                if let Some(call_key) = call_key {
                    active_function_calls.remove(&call_key);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn function_call_key(value: &Value) -> Option<String> {
    value
        .get("output_index")
        .and_then(Value::as_u64)
        .map(|index| format!("output_index:{index}"))
        .or_else(|| {
            value
                .get("item_id")
                .and_then(Value::as_str)
                .map(|id| format!("item_id:{id}"))
        })
        .or_else(|| {
            value
                .get("item")
                .and_then(|item| item.get("id").or_else(|| item.get("call_id")))
                .and_then(Value::as_str)
                .map(|id| format!("item:{id}"))
        })
}

pub(super) async fn finalize_responses_tool_loop_stream(
    input: &mut Vec<Value>,
    text_delta_sink: AgentTextDeltaSink,
    finalization: StreamFinalization,
) -> Result<AgentStep, LlmError> {
    let StreamFinalization {
        allow_tool_calls,
        mut answer,
        buffered_deltas,
        completed_response,
        completion_confirmed,
        diagnostics,
    } = finalization;
    if !completion_confirmed {
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
            return Err(tool_calls_disabled_error());
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
    let output_parts = crate::provider::openai::extract::extract_response_output_parts(&body);
    if answer.trim().is_empty() && output_parts.is_empty() {
        return Err(LlmError::provider(
            "OpenAI tool loop returned empty final text output",
            "provider",
        ));
    }
    if allow_tool_calls {
        if buffered_deltas.is_empty() {
            update_streaming_diagnostics(&diagnostics, |item| {
                item.visible_text_chars += answer.chars().count();
            });
            text_delta_sink(answer.clone()).await?;
        } else {
            for delta in buffered_deltas {
                update_streaming_diagnostics(&diagnostics, |item| {
                    item.visible_text_chars += delta.chars().count();
                });
                text_delta_sink(delta).await?;
            }
        }
    }
    Ok(AgentStep::FinalAnswer {
        reply: answer,
        output_parts,
        usage: step_usage,
    })
}
