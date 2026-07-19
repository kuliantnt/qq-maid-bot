use super::*;

fn agent_tool_trace(
    emitted_tools: Vec<String>,
    tool_results: Vec<ToolExecutionResult>,
) -> AgentRunDiagnostics {
    let executed_tools: Vec<String> = tool_results
        .iter()
        .map(|result| result.name.clone())
        .collect();
    AgentRunDiagnostics {
        model_rounds: 2,
        emitted_tools,
        tool_execution_attempted: true,
        side_effecting_tools_started: executed_tools.clone(),
        executed_tools,
        tool_results,
        tools_with_unknown_result: Vec::new(),
        streaming_fallback_used: false,
        stop_reason: Some(AgentStopReason::ToolUsed),
    }
}

fn agent_direct_trace() -> AgentRunDiagnostics {
    AgentRunDiagnostics {
        model_rounds: 1,
        stop_reason: Some(AgentStopReason::DirectAnswer),
        ..Default::default()
    }
}

#[derive(Clone)]
pub(crate) struct MockProvider {
    calls: Arc<AtomicUsize>,
    tool_calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    tool_requests: Arc<Mutex<Vec<ToolChatRequest>>>,
    stream_enabled: bool,
    tool_protocol: Option<ToolCallingProtocol>,
    tool_actions: Arc<Mutex<Vec<MockToolAction>>>,
    title_replies: Arc<Mutex<Vec<Result<String, LlmError>>>>,
    compact_replies: Arc<Mutex<Vec<Result<String, LlmError>>>>,
    dream_replies: Arc<Mutex<Vec<Result<String, LlmError>>>>,
    dream_delay: Option<std::time::Duration>,
    search_query_rewrite_replies: Arc<Mutex<Vec<Result<String, LlmError>>>>,
    title_delay: Option<std::time::Duration>,
}

#[derive(Clone)]
enum MockToolAction {
    CreateTodo {
        content: String,
    },
    ExecuteTool {
        name: String,
        arguments: String,
        reply: String,
    },
    ExecuteTools {
        calls: Vec<(String, String)>,
        reply: String,
    },
    ExecuteToolsThenFail {
        calls: Vec<(String, String)>,
        error: LlmError,
    },
    ReturnToolResults {
        results: Vec<ToolExecutionResult>,
        reply: String,
    },
    ReplyWithoutTool {
        reply: String,
    },
    RejectedToolCall {
        name: String,
        reply: String,
    },
}

impl MockProvider {
    pub(crate) fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            tool_calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            tool_requests: Arc::new(Mutex::new(Vec::new())),
            stream_enabled: false,
            tool_protocol: None,
            tool_actions: Arc::new(Mutex::new(Vec::new())),
            title_replies: Arc::new(Mutex::new(Vec::new())),
            compact_replies: Arc::new(Mutex::new(Vec::new())),
            dream_replies: Arc::new(Mutex::new(Vec::new())),
            dream_delay: None,
            search_query_rewrite_replies: Arc::new(Mutex::new(Vec::new())),
            title_delay: None,
        }
    }

    pub(crate) fn with_counter(calls: Arc<AtomicUsize>) -> Self {
        Self {
            calls,
            tool_calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            tool_requests: Arc::new(Mutex::new(Vec::new())),
            stream_enabled: false,
            tool_protocol: None,
            tool_actions: Arc::new(Mutex::new(Vec::new())),
            title_replies: Arc::new(Mutex::new(Vec::new())),
            compact_replies: Arc::new(Mutex::new(Vec::new())),
            dream_replies: Arc::new(Mutex::new(Vec::new())),
            dream_delay: None,
            search_query_rewrite_replies: Arc::new(Mutex::new(Vec::new())),
            title_delay: None,
        }
    }

    pub(crate) fn with_title_replies(replies: Vec<Result<&str, LlmError>>) -> Self {
        Self {
            title_replies: Arc::new(Mutex::new(
                replies
                    .into_iter()
                    .map(|result| result.map(str::to_owned))
                    .collect(),
            )),
            title_delay: None,
            ..Self::new()
        }
    }

    pub(crate) fn with_dream_replies(replies: Vec<Result<&str, LlmError>>) -> Self {
        Self {
            dream_replies: Arc::new(Mutex::new(
                replies
                    .into_iter()
                    .map(|result| result.map(str::to_owned))
                    .collect(),
            )),
            ..Self::new()
        }
    }

    pub(crate) fn with_dream_delay(mut self, delay: std::time::Duration) -> Self {
        self.dream_delay = Some(delay);
        self
    }

    pub(crate) fn with_search_query_rewrite_replies(replies: Vec<Result<&str, LlmError>>) -> Self {
        Self {
            search_query_rewrite_replies: Arc::new(Mutex::new(
                replies
                    .into_iter()
                    .map(|result| result.map(str::to_owned))
                    .collect(),
            )),
            ..Self::new()
        }
    }

    pub(crate) fn with_title_delay(mut self, delay: std::time::Duration) -> Self {
        self.title_delay = Some(delay);
        self
    }

    pub(crate) fn push_compact_reply(&self, reply: impl Into<String>) {
        self.compact_replies.lock().unwrap().push(Ok(reply.into()));
    }

    pub(crate) fn with_tool_protocol(mut self, protocol: ToolCallingProtocol) -> Self {
        self.tool_protocol = Some(protocol);
        self
    }

    pub(crate) fn with_stream_enabled(mut self, enabled: bool) -> Self {
        self.stream_enabled = enabled;
        self
    }

    pub(crate) fn with_create_todo_tool_call(self, content: impl Into<String>) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::CreateTodo {
                content: content.into(),
            });
        self
    }

    pub(crate) fn with_tool_call_json(
        self,
        name: impl Into<String>,
        arguments: impl Into<String>,
        reply: impl Into<String>,
    ) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::ExecuteTool {
                name: name.into(),
                arguments: arguments.into(),
                reply: reply.into(),
            });
        self
    }

    pub(crate) fn with_tool_calls_json(
        self,
        calls: Vec<(&str, &str)>,
        reply: impl Into<String>,
    ) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::ExecuteTools {
                calls: calls
                    .into_iter()
                    .map(|(name, arguments)| (name.to_owned(), arguments.to_owned()))
                    .collect(),
                reply: reply.into(),
            });
        self
    }

    pub(crate) fn with_tool_calls_then_error(
        self,
        calls: Vec<(&str, &str)>,
        error: LlmError,
    ) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::ExecuteToolsThenFail {
                calls: calls
                    .into_iter()
                    .map(|(name, arguments)| (name.to_owned(), arguments.to_owned()))
                    .collect(),
                error,
            });
        self
    }

    pub(crate) fn with_raw_tool_results(
        self,
        results: Vec<ToolExecutionResult>,
        reply: impl Into<String>,
    ) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::ReturnToolResults {
                results,
                reply: reply.into(),
            });
        self
    }

    pub(crate) fn with_tool_loop_reply_without_tool(self, reply: impl Into<String>) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::ReplyWithoutTool {
                reply: reply.into(),
            });
        self
    }

    pub(crate) fn with_rejected_tool_call(
        self,
        name: impl Into<String>,
        reply: impl Into<String>,
    ) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::RejectedToolCall {
                name: name.into(),
                reply: reply.into(),
            });
        self
    }

    pub(crate) fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub(crate) fn tool_call_count(&self) -> usize {
        self.tool_calls.load(Ordering::SeqCst)
    }

    pub(crate) fn tool_requests(&self) -> Vec<ToolChatRequest> {
        self.tool_requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req.clone());
        if req.metadata.get("purpose").map(String::as_str) == Some("session_title") {
            if let Some(delay) = self.title_delay {
                tokio::time::sleep(delay).await;
            }
            let reply = {
                let mut replies = self.title_replies.lock().unwrap();
                if replies.is_empty() {
                    crate::runtime::session::DEFAULT_SESSION_TITLE.to_owned()
                } else {
                    replies.remove(0)?
                }
            };
            return Ok(ChatOutcome {
                reply,
                output_parts: Vec::new(),
                metrics: LlmMetrics {
                    provider: "mock".to_owned(),
                    model: req.model.unwrap_or_else(|| "mock-model".to_owned()),
                    stream: false,
                    ttfe_ms: None,
                    ttft_ms: None,
                    total_latency_ms: 1,
                },
                usage: Some(TokenUsage {
                    input_tokens: None,
                    cached_input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                }),
                fallback_used: false,
                agent: Default::default(),
            });
        }
        if req.metadata.get("purpose").map(String::as_str) == Some("search_query_rewrite") {
            let reply = self
                .search_query_rewrite_replies
                .lock()
                .unwrap()
                .remove(0)?;
            return Ok(ChatOutcome {
                reply,
                output_parts: Vec::new(),
                metrics: LlmMetrics {
                    provider: "mock".to_owned(),
                    model: req.model.unwrap_or_else(|| "mock-model".to_owned()),
                    stream: false,
                    ttfe_ms: None,
                    ttft_ms: None,
                    total_latency_ms: 1,
                },
                usage: Some(TokenUsage {
                    input_tokens: None,
                    cached_input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                }),
                fallback_used: false,
                agent: Default::default(),
            });
        }
        if req.metadata.get("purpose").map(String::as_str) == Some("compact") {
            let reply = {
                let mut replies = self.compact_replies.lock().unwrap();
                if replies.is_empty() {
                    None
                } else {
                    Some(replies.remove(0)?)
                }
            };
            if let Some(reply) = reply {
                return Ok(ChatOutcome {
                    reply,
                    output_parts: Vec::new(),
                    metrics: LlmMetrics {
                        provider: "mock".to_owned(),
                        model: req.model.unwrap_or_else(|| "mock-model".to_owned()),
                        stream: false,
                        ttfe_ms: None,
                        ttft_ms: None,
                        total_latency_ms: 1,
                    },
                    usage: Some(TokenUsage {
                        input_tokens: None,
                        cached_input_tokens: None,
                        output_tokens: None,
                        total_tokens: None,
                    }),
                    fallback_used: false,
                    agent: Default::default(),
                });
            }
        }
        if req.metadata.get("purpose").map(String::as_str) == Some("memory_dream") {
            if let Some(delay) = self.dream_delay {
                tokio::time::sleep(delay).await;
            }
            let reply = self.dream_replies.lock().unwrap().remove(0)?;
            return Ok(ChatOutcome {
                reply,
                output_parts: Vec::new(),
                metrics: LlmMetrics {
                    provider: "mock".to_owned(),
                    model: req.model.unwrap_or_else(|| "mock-model".to_owned()),
                    stream: false,
                    ttfe_ms: None,
                    ttft_ms: None,
                    total_latency_ms: 1,
                },
                usage: None,
                fallback_used: false,
                agent: Default::default(),
            });
        }
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|message| message.role == ChatRole::User)
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let metrics_model = req.model.clone().unwrap_or_else(|| "mock-model".to_owned());
        let reply = match req.metadata.get("purpose").map(String::as_str) {
            Some("todo_parse") => mock_todo_parse_reply(&last_user),
            Some("memory_draft") => mock_memory_draft_reply(
                &last_user,
                req.metadata.get("memory_operation").map(String::as_str),
            ),
            _ if last_user.contains("给 codex") => "# 标题\n- hello".to_owned(),
            _ => format!("回复：{last_user}"),
        };
        Ok(ChatOutcome {
            reply,
            output_parts: Vec::new(),
            metrics: LlmMetrics {
                provider: "mock".to_owned(),
                model: metrics_model,
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: Some(TokenUsage {
                input_tokens: None,
                cached_input_tokens: None,
                output_tokens: None,
                total_tokens: None,
            }),
            fallback_used: false,
            agent: Default::default(),
        })
    }

    fn tool_calling_protocol(&self, _model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.tool_protocol
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        self.tool_requests.lock().unwrap().push(req.clone());
        let action = {
            let mut actions = self.tool_actions.lock().unwrap();
            if actions.is_empty() {
                None
            } else {
                Some(actions.remove(0))
            }
        };
        if let Some(action) = action {
            match action {
                MockToolAction::CreateTodo { content } => {
                    let arguments = json!({
                        "content": content,
                        "title": null,
                        "detail": null,
                        "due_date": null,
                        "due_at": null,
                        "reminder_at": null,
                        "time_precision": null,
                    })
                    .to_string();
                    let output = req
                        .tools
                        .execute_json(&req.tool_context, "create_todo", &arguments)
                        .await?;
                    let output = serde_json::from_str::<Value>(&output).unwrap_or_else(|_| {
                        json!({
                            "raw": output,
                        })
                    });
                    let tool_results = vec![qq_maid_llm::provider::ToolExecutionResult {
                        name: "create_todo".to_owned(),
                        output,
                        succeeded: true,
                    }];
                    return Ok(ChatOutcome {
                        reply: format!("工具回复：{}", last_user_from_tool_request(&req)),
                        output_parts: Vec::new(),
                        metrics: LlmMetrics {
                            provider: "mock".to_owned(),
                            model: req
                                .chat
                                .model
                                .clone()
                                .unwrap_or_else(|| "mock-model".to_owned()),
                            stream: false,
                            ttfe_ms: None,
                            ttft_ms: None,
                            total_latency_ms: 1,
                        },
                        usage: Some(TokenUsage {
                            input_tokens: None,
                            cached_input_tokens: None,
                            output_tokens: None,
                            total_tokens: None,
                        }),
                        fallback_used: false,
                        agent: agent_tool_trace(vec!["create_todo".to_owned()], tool_results),
                    });
                }
                MockToolAction::ExecuteTool {
                    name,
                    arguments,
                    reply,
                } => {
                    let output = req
                        .tools
                        .execute_json(&req.tool_context, &name, &arguments)
                        .await?;
                    let output = serde_json::from_str::<Value>(&output).unwrap_or_else(|_| {
                        json!({
                            "raw": output,
                        })
                    });
                    let succeeded = output.get("ok").and_then(Value::as_bool) != Some(false);
                    let emitted_tool = name.clone();
                    let tool_results = vec![qq_maid_llm::provider::ToolExecutionResult {
                        name,
                        output,
                        succeeded,
                    }];
                    return Ok(ChatOutcome {
                        reply,
                        output_parts: Vec::new(),
                        metrics: LlmMetrics {
                            provider: "mock".to_owned(),
                            model: req
                                .chat
                                .model
                                .clone()
                                .unwrap_or_else(|| "mock-model".to_owned()),
                            stream: false,
                            ttfe_ms: None,
                            ttft_ms: None,
                            total_latency_ms: 1,
                        },
                        usage: Some(TokenUsage {
                            input_tokens: None,
                            cached_input_tokens: None,
                            output_tokens: None,
                            total_tokens: None,
                        }),
                        fallback_used: false,
                        agent: agent_tool_trace(vec![emitted_tool], tool_results),
                    });
                }
                MockToolAction::ExecuteTools { calls, reply } => {
                    let mut executed_tools = Vec::new();
                    let mut tool_results = Vec::new();
                    for (name, arguments) in calls {
                        let output = req
                            .tools
                            .execute_json(&req.tool_context, &name, &arguments)
                            .await?;
                        let output = serde_json::from_str::<Value>(&output).unwrap_or_else(|_| {
                            json!({
                                "raw": output,
                            })
                        });
                        let succeeded = output.get("ok").and_then(Value::as_bool) != Some(false);
                        executed_tools.push(name.clone());
                        tool_results.push(qq_maid_llm::provider::ToolExecutionResult {
                            name,
                            output,
                            succeeded,
                        });
                    }
                    let emitted_tools = executed_tools.clone();
                    return Ok(ChatOutcome {
                        reply,
                        output_parts: Vec::new(),
                        metrics: LlmMetrics {
                            provider: "mock".to_owned(),
                            model: req
                                .chat
                                .model
                                .clone()
                                .unwrap_or_else(|| "mock-model".to_owned()),
                            stream: false,
                            ttfe_ms: None,
                            ttft_ms: None,
                            total_latency_ms: 1,
                        },
                        usage: Some(TokenUsage {
                            input_tokens: None,
                            cached_input_tokens: None,
                            output_tokens: None,
                            total_tokens: None,
                        }),
                        fallback_used: false,
                        agent: agent_tool_trace(emitted_tools, tool_results),
                    });
                }
                MockToolAction::ExecuteToolsThenFail { calls, error } => {
                    let mut executed_tools = Vec::new();
                    let mut tool_results = Vec::new();
                    for (name, arguments) in calls {
                        let output = req
                            .tools
                            .execute_json(&req.tool_context, &name, &arguments)
                            .await?;
                        let output = serde_json::from_str::<Value>(&output).unwrap_or_else(|_| {
                            json!({
                                "raw": output,
                            })
                        });
                        let succeeded = output.get("ok").and_then(Value::as_bool) != Some(false);
                        executed_tools.push(name.clone());
                        tool_results.push(qq_maid_llm::provider::ToolExecutionResult {
                            name,
                            output,
                            succeeded,
                        });
                    }
                    let mut diagnostics = agent_tool_trace(executed_tools, tool_results);
                    diagnostics.model_rounds = 4;
                    diagnostics.stop_reason = Some(AgentStopReason::Failed);
                    return Err(error.with_agent(diagnostics));
                }
                MockToolAction::ReturnToolResults { results, reply } => {
                    let emitted_tools = results
                        .iter()
                        .map(|result| result.name.clone())
                        .collect::<Vec<_>>();
                    return Ok(ChatOutcome {
                        reply,
                        output_parts: Vec::new(),
                        metrics: LlmMetrics {
                            provider: "mock".to_owned(),
                            model: req
                                .chat
                                .model
                                .clone()
                                .unwrap_or_else(|| "mock-model".to_owned()),
                            stream: false,
                            ttfe_ms: None,
                            ttft_ms: None,
                            total_latency_ms: 1,
                        },
                        usage: Some(TokenUsage {
                            input_tokens: None,
                            cached_input_tokens: None,
                            output_tokens: None,
                            total_tokens: None,
                        }),
                        fallback_used: false,
                        agent: agent_tool_trace(emitted_tools, results),
                    });
                }
                MockToolAction::ReplyWithoutTool { reply } => {
                    return Ok(ChatOutcome {
                        reply,
                        output_parts: Vec::new(),
                        metrics: LlmMetrics {
                            provider: "mock".to_owned(),
                            model: req
                                .chat
                                .model
                                .clone()
                                .unwrap_or_else(|| "mock-model".to_owned()),
                            stream: false,
                            ttfe_ms: None,
                            ttft_ms: None,
                            total_latency_ms: 1,
                        },
                        usage: Some(TokenUsage {
                            input_tokens: None,
                            cached_input_tokens: None,
                            output_tokens: None,
                            total_tokens: None,
                        }),
                        fallback_used: false,
                        agent: agent_direct_trace(),
                    });
                }
                MockToolAction::RejectedToolCall { name, reply } => {
                    return Ok(ChatOutcome {
                        reply,
                        output_parts: Vec::new(),
                        metrics: LlmMetrics {
                            provider: "mock".to_owned(),
                            model: req
                                .chat
                                .model
                                .clone()
                                .unwrap_or_else(|| "mock-model".to_owned()),
                            stream: false,
                            ttfe_ms: None,
                            ttft_ms: None,
                            total_latency_ms: 1,
                        },
                        usage: None,
                        fallback_used: false,
                        agent: AgentRunDiagnostics {
                            model_rounds: 1,
                            emitted_tools: vec![name],
                            tool_execution_attempted: true,
                            executed_tools: Vec::new(),
                            side_effecting_tools_started: Vec::new(),
                            tool_results: Vec::new(),
                            tools_with_unknown_result: Vec::new(),
                            streaming_fallback_used: false,
                            stop_reason: Some(AgentStopReason::Rejected),
                        },
                    });
                }
            }
        }
        let last_user = last_user_from_tool_request(&req);
        Ok(ChatOutcome {
            reply: format!("工具回复：{last_user}"),
            output_parts: Vec::new(),
            metrics: LlmMetrics {
                provider: "mock".to_owned(),
                model: req
                    .chat
                    .model
                    .clone()
                    .unwrap_or_else(|| "mock-model".to_owned()),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: Some(TokenUsage {
                input_tokens: None,
                cached_input_tokens: None,
                output_tokens: None,
                total_tokens: None,
            }),
            fallback_used: false,
            agent: agent_direct_trace(),
        })
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock-model"
    }

    fn stream_enabled(&self) -> bool {
        self.stream_enabled
    }
}

fn last_user_from_tool_request(req: &ToolChatRequest) -> String {
    req.chat
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::User)
        .map(|message| message.content.clone())
        .unwrap_or_default()
}
