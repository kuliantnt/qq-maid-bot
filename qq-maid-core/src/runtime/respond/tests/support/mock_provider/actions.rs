//! Mock Provider 的 Tool action 定义与测试 builder。
//!
//! 主模块只负责执行 Provider 协议；这里集中描述测试要模拟的 Tool Loop 轨迹，
//! 避免新增失败链路夹具后继续扩大 Provider 执行文件的职责和行数。

use super::*;

#[derive(Clone)]
pub(super) enum MockToolAction {
    CreateTodo {
        content: String,
    },
    ExecuteTool {
        name: String,
        arguments: String,
        reply: String,
    },
    ExecuteTodoListRetry {
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
    ExecuteToolsThenFailWithPendingCall {
        calls: Vec<(String, String)>,
        pending_tools: Vec<String>,
        error: LlmError,
    },
    ReturnToolResults {
        results: Vec<ToolExecutionResult>,
        attempts: Vec<ToolExecutionAttempt>,
        reply: String,
    },
    ReturnToolResultsThenFail {
        results: Vec<ToolExecutionResult>,
        error: LlmError,
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

    pub(crate) fn with_todo_list_retry(
        self,
        arguments: impl Into<String>,
        reply: impl Into<String>,
    ) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::ExecuteTodoListRetry {
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

    pub(crate) fn with_tool_calls_then_error_with_pending_call(
        self,
        calls: Vec<(&str, &str)>,
        pending_tools: Vec<&str>,
        error: LlmError,
    ) -> Self {
        self.tool_actions.lock().unwrap().push(
            MockToolAction::ExecuteToolsThenFailWithPendingCall {
                calls: calls
                    .into_iter()
                    .map(|(name, arguments)| (name.to_owned(), arguments.to_owned()))
                    .collect(),
                pending_tools: pending_tools.into_iter().map(str::to_owned).collect(),
                error,
            },
        );
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
                attempts: Vec::new(),
                reply: reply.into(),
            });
        self
    }

    pub(crate) fn with_raw_tool_results_and_attempts(
        self,
        results: Vec<ToolExecutionResult>,
        attempts: Vec<ToolExecutionAttempt>,
        reply: impl Into<String>,
    ) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::ReturnToolResults {
                results,
                attempts,
                reply: reply.into(),
            });
        self
    }

    pub(crate) fn with_raw_tool_results_then_error(
        self,
        results: Vec<ToolExecutionResult>,
        error: LlmError,
    ) -> Self {
        self.tool_actions
            .lock()
            .unwrap()
            .push(MockToolAction::ReturnToolResultsThenFail { results, error });
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
}
