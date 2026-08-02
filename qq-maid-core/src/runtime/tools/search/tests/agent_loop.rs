use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use qq_maid_llm::{
    LlmError,
    agent_loop::{AgentStep, AgentStepSession, AgentToolCall, AgentToolResult, run_agent_loop},
    tool::ToolRegistry,
    web_search::{WebSearchBackend, WebSearchRequest},
};
use serde_json::Value;

use super::{
    WEB_SEARCH_TOOL_NAME, WebSearchTool,
    support::{MockWebSearchExecutor, test_context},
};

/// 记录每次模型请求收到的工具结果，并在模型请求前观察搜索执行次数。
///
/// 该夹具只模拟 Provider 的协议适配层；实际参数校验、ToolRegistry 去重和
/// Web Search executor 均走生产实现，以覆盖一次完整的 Agent Loop。
struct RecordingSession {
    script: VecDeque<AgentStep>,
    observed_results: Arc<Mutex<Vec<Vec<AgentToolResult>>>>,
    observed_request_counts: Arc<Mutex<Vec<usize>>>,
    requests: Arc<Mutex<Vec<WebSearchRequest>>>,
}

#[async_trait]
impl AgentStepSession for RecordingSession {
    fn provider(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock-model"
    }

    async fn advance(
        &mut self,
        results: &[AgentToolResult],
        _allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        self.observed_results.lock().unwrap().push(results.to_vec());
        self.observed_request_counts
            .lock()
            .unwrap()
            .push(self.requests.lock().unwrap().len());
        self.script
            .pop_front()
            .ok_or_else(|| LlmError::provider("missing scripted Agent step", "test"))
    }
}

fn web_search_call(call_id: &str, arguments: &str) -> AgentStep {
    AgentStep::ToolCalls {
        calls: vec![AgentToolCall {
            name: WEB_SEARCH_TOOL_NAME.to_owned(),
            call_id: call_id.to_owned(),
            arguments: arguments.to_owned(),
        }],
        usage: None,
    }
}

fn final_answer(reply: &str) -> AgentStep {
    AgentStep::FinalAnswer {
        reply: reply.to_owned(),
        output_parts: Vec::new(),
        usage: None,
    }
}

#[tokio::test]
async fn agent_web_search_self_corrects_invalid_topic_before_one_real_call() {
    let executor = MockWebSearchExecutor::default();
    let requests = executor.requests.clone();
    let stream_calls = executor.stream_calls.clone();
    let tool =
        WebSearchTool::new(Arc::new(executor)).with_backend_override(WebSearchBackend::Tavily);
    let registry = ToolRegistry::new().register(tool).unwrap();
    let observed_results = Arc::new(Mutex::new(Vec::new()));
    let observed_request_counts = Arc::new(Mutex::new(Vec::new()));

    let session = RecordingSession {
        script: VecDeque::from([
            web_search_call(
                "invalid-topic",
                r#"{"query":"Rust news","raw_question":"private question","topic":"medical"}"#,
            ),
            web_search_call(
                "corrected-topic",
                r#"{"query":"Rust news","raw_question":"private question","topic":"general"}"#,
            ),
            final_answer("已根据搜索结果整理：Rust news 的公开资料已核实。"),
        ]),
        observed_results: observed_results.clone(),
        observed_request_counts: observed_request_counts.clone(),
        requests: requests.clone(),
    };

    let outcome = run_agent_loop(Box::new(session), registry, test_context(), 3, None, None)
        .await
        .unwrap();

    let observed_results = observed_results.lock().unwrap();
    assert_eq!(observed_results.len(), 3);
    assert!(observed_results[0].is_empty());

    // 第一轮参数错误在第二轮模型请求前已回填；此时 executor 仍未收到任何请求。
    assert_eq!(observed_results[1].len(), 1);
    let invalid_output: Value = serde_json::from_str(&observed_results[1][0].output).unwrap();
    assert_eq!(invalid_output["error"]["argument"], "topic");
    assert_eq!(invalid_output["error"]["retryable_by_model"], true);

    // 修正后的真实搜索结果继续回填给最终回答轮。
    assert_eq!(observed_results[2].len(), 1);
    let success_output: Value = serde_json::from_str(&observed_results[2][0].output).unwrap();
    assert_eq!(success_output["ok"], true);
    assert_eq!(success_output["execution_succeeded"], true);
    assert_eq!(success_output["answer"], "answer: Rust news");

    assert_eq!(
        *observed_request_counts.lock().unwrap(),
        vec![0, 0, 1],
        "invalid arguments must not reach Tavily/mock executor"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].topic.as_deref(), Some("general"));
    assert_eq!(stream_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    assert_eq!(
        outcome.reply,
        "已根据搜索结果整理：Rust news 的公开资料已核实。"
    );
    assert_eq!(outcome.agent.model_rounds, 3);
    assert_eq!(outcome.agent.tool_results.len(), 2);
    assert_eq!(
        outcome.agent.tool_results[0].output["error"]["argument"],
        "topic"
    );
    assert_eq!(outcome.agent.tool_results[1].output["ok"], true);
    // Tool Loop 已两次进入 Web Search Tool；第一次在参数边界返回，只有第二次
    // 越过参数校验后才真正触发 executor。
    assert_eq!(
        outcome.agent.executed_tools,
        [WEB_SEARCH_TOOL_NAME, WEB_SEARCH_TOOL_NAME]
    );
    assert_eq!(outcome.agent.tool_attempts[0].retry_of, None);
    assert_eq!(
        outcome.agent.tool_attempts[1].retry_of, None,
        "changed arguments must not be treated as a cached duplicate"
    );
}
