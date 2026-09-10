//! 脚本化模型驱动真实 Loop 和业务 Tool，再通过 Respond 投影/拼装验证用户回复。
use super::*;
use crate::{
    error::LlmError,
    runtime::tools::{
        TrainScheduleTool, WebSearchTool,
        train::{TrainExecutor, TrainSchedule, TrainScheduleRequest},
    },
};
use async_trait::async_trait;
use qq_maid_common::identity_context::{
    ConversationKind, ExecutionActorContext, ExecutionConversationContext,
};
use qq_maid_llm::{
    agent_loop::{AgentStep, AgentStepSession, AgentToolCall, AgentToolResult, run_agent_loop},
    tool::{ToolContext, ToolRegistry},
    web_search::{WebSearchBackend, WebSearchExecutor, WebSearchOutcome, WebSearchRequest},
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

struct RecordingSession {
    script: VecDeque<AgentStep>,
    inputs: Arc<Mutex<Vec<Vec<AgentToolResult>>>>,
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
        _: bool,
    ) -> Result<AgentStep, LlmError> {
        self.inputs.lock().unwrap().push(results.to_vec());
        Ok(self.script.pop_front().expect("完整复现脚本"))
    }
}

#[derive(Clone, Default)]
struct Executors {
    requests: Arc<Mutex<Vec<String>>>,
}
#[async_trait]
impl TrainExecutor for Executors {
    async fn query_train_schedule(
        &self,
        req: TrainScheduleRequest,
    ) -> Result<TrainSchedule, LlmError> {
        self.requests.lock().unwrap().push(req.train_code.clone());
        if req.train_code == "K350" {
            return Err(LlmError::timeout("train"));
        }
        MockTrainExecutor::new().query_train_schedule(req).await
    }
    fn provider_name(&self) -> &'static str {
        "mock-train"
    }
}
#[async_trait]
impl WebSearchExecutor for Executors {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        self.requests.lock().unwrap().push(req.query.clone());
        if req.query == "必要的第二步" {
            return Err(LlmError::timeout("web_search"));
        }
        MockWebSearchExecutor.query(req).await
    }
    fn provider_name(&self) -> &'static str {
        "mock-search"
    }
}

fn context() -> ToolContext {
    ToolContext {
        task_id: "continuation-regression".into(),
        actor: ExecutionActorContext {
            user_id: Some("test-user".into()),
            group_member_role: None,
        },
        conversation: ExecutionConversationContext {
            platform: "test".into(),
            account_id: None,
            kind: ConversationKind::Private,
            target_id: Some("test-user".into()),
            scope_id: "private:test-user".into(),
            interaction_scope_id: "private:test-user".into(),
        },
        tool_call_id: None,
        tool_round: None,
        retry_of: None,
        execution_deadline: None,
    }
}

async fn reproduce(
    name: &str,
    arguments: &[Value],
    redundant: bool,
    expected_status: &str,
) -> String {
    reproduce_with_finalization(name, arguments, redundant, expected_status, None).await
}

// None 沿用模型正文；Ok 模拟空正文，Err 模拟拿到真实工具轨迹后的最终生成失败。
async fn reproduce_with_finalization(
    name: &str,
    arguments: &[Value],
    redundant: bool,
    expected_status: &str,
    finalization: Option<Result<String, LlmError>>,
) -> String {
    let executors = Executors::default();
    let registry = ToolRegistry::new()
        .register(TrainScheduleTool::new(Arc::new(executors.clone())))
        .unwrap()
        .register(
            WebSearchTool::new(Arc::new(executors.clone()))
                .with_backend_override(WebSearchBackend::Tavily),
        )
        .unwrap();
    let mut script = arguments
        .iter()
        .enumerate()
        .map(|(i, args)| AgentStep::ToolCalls {
            calls: vec![AgentToolCall {
                name: name.into(),
                call_id: format!("call-{i}"),
                arguments: args.to_string(),
            }],
            usage: None,
        })
        .collect::<VecDeque<_>>();
    let model_reply = model_reply(name).await;
    script.push_back(AgentStep::FinalAnswer {
        reply: model_reply,
        output_parts: vec![],
        usage: None,
    });
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let result = run_agent_loop(
        Box::new(RecordingSession {
            script,
            inputs: inputs.clone(),
        }),
        registry,
        context(),
        4,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.agent.model_rounds, arguments.len() + 1);
    assert_eq!(result.agent.emitted_tools.len(), arguments.len());
    assert_eq!(result.agent.tool_results.len(), arguments.len());
    assert_eq!(inputs.lock().unwrap().len(), arguments.len() + 1);
    let last = result.agent.tool_attempts.last().unwrap();
    assert_eq!(last.redundant_of, redundant.then_some(0));
    if redundant && arguments.last().unwrap() != &arguments[0] {
        assert!(
            !result.agent.tool_results.last().unwrap().succeeded,
            "原始缺参失败不可伪造为成功"
        );
        assert!(
            inputs.lock().unwrap().last().unwrap()[0]
                .output
                .contains("continuation_hint")
        );
    }
    if redundant {
        assert_eq!(executors.requests.lock().unwrap().len(), 1);
    }
    if redundant && name == "get_train_schedule" {
        verify_unassociated_train_trace(&result).await;
    }
    let finalization_failed = matches!(finalization, Some(Err(_)));
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let provider = match finalization {
        Some(Err(error)) => provider.with_raw_tool_results_and_attempts_then_error(
            result.agent.tool_results,
            result.agent.tool_attempts,
            error,
        ),
        reply => provider.with_raw_tool_results_and_attempts(
            result.agent.tool_results,
            result.agent.tool_attempts,
            reply.and_then(Result::ok).unwrap_or(result.reply),
        ),
    };
    let response = test_service_with_provider_and_tool_calling(provider, true)
        .respond(private_message("执行查询任务"))
        .await
        .unwrap();
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], expected_status);
    if finalization_failed {
        assert_eq!(diagnostics["agent_finalization_fallback_used"], true);
        assert_eq!(
            diagnostics["agent_finalization_error_code"],
            "context_budget_exceeded"
        );
        assert_eq!(diagnostics["error_code"], Value::Null);
    }
    let attempts = diagnostics["agent_tool_attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), arguments.len());
    assert_eq!(
        attempts.last().unwrap()["redundant_of"],
        serde_json::json!(redundant.then_some(0))
    );
    assert!(
        attempts
            .iter()
            .all(|attempt| attempt.get("call_id").is_none() && attempt.get("arguments").is_none())
    );
    response.text.unwrap()
}

#[tokio::test]
async fn train_success_then_missing_arguments_does_not_append_error() {
    let text = reproduce(
        "get_train_schedule",
        &[
            serde_json::json!({"train_code":"K349", "travel_date":"2026-09-10"}),
            serde_json::json!({"travel_date":"2026-09-10"}),
        ],
        true,
        "succeeded",
    )
    .await;
    assert!(!text.contains("参数不完整"));
    assert!(text.contains("K349"));
    assert!(text.contains("北京南"));
    assert!(text.contains("上海虹桥"));
    assert!(text.contains("11:24"));
    assert!(text.contains("查询全部完成"));
}

#[tokio::test]
async fn first_train_call_missing_arguments_still_reports_error() {
    let text = reproduce(
        "get_train_schedule",
        &[serde_json::json!({})],
        false,
        "failed",
    )
    .await;
    assert!(text.contains("参数不完整"));
}

#[tokio::test]
async fn necessary_second_train_timeout_is_partial_failure() {
    let text = reproduce(
        "get_train_schedule",
        &[
            serde_json::json!({"train_code":"K349"}),
            serde_json::json!({"train_code":"K350"}),
        ],
        false,
        "partial_success",
    )
    .await;
    assert!(text.contains("K349"));
    assert!(text.contains("超时"));
    assert!(!text.contains("查询全部完成"));
}

#[tokio::test]
async fn changed_train_date_with_missing_code_is_not_suppressed() {
    let text = reproduce(
        "get_train_schedule",
        &[
            serde_json::json!({"train_code":"K349", "travel_date":"2026-09-10"}),
            serde_json::json!({"travel_date":"2026-09-11"}),
        ],
        false,
        "partial_success",
    )
    .await;
    assert!(text.contains("参数不完整"));
}

#[tokio::test]
async fn train_exact_cached_repeat_is_not_a_malformed_schedule() {
    let args = serde_json::json!({"train_code":"K349"});
    let text = reproduce(
        "get_train_schedule",
        &[args.clone(), args],
        true,
        "succeeded",
    )
    .await;
    assert!(!text.contains("失败"));
}

#[tokio::test]
async fn search_success_then_missing_arguments_does_not_append_error() {
    let text = reproduce(
        "web_search",
        &[
            serde_json::json!({"query":"公开铁路资料"}),
            serde_json::json!({}),
        ],
        true,
        "succeeded",
    )
    .await;
    assert!(!text.contains("参数不完整"));
}

#[tokio::test]
async fn first_search_call_missing_arguments_still_reports_error() {
    let text = reproduce("web_search", &[serde_json::json!({})], false, "failed").await;
    assert!(!text.contains("查询全部完成"));
    assert!(text.contains("本次联网查询的参数无效，查询未执行。"));
}

#[tokio::test]
async fn necessary_second_search_timeout_is_partial_failure() {
    let text = reproduce(
        "web_search",
        &[
            serde_json::json!({"query":"公开铁路资料"}),
            serde_json::json!({"query":"必要的第二步"}),
        ],
        false,
        "partial_success",
    )
    .await;
    assert!(text.contains("超时"));
    assert!(!text.contains("查询全部完成"));
}

async fn verify_unassociated_train_trace(result: &qq_maid_llm::provider::ChatOutcome) {
    let mut old_attempts = result.agent.tool_attempts.clone();
    for attempt in &mut old_attempts {
        attempt.redundant_of = None;
    }
    let old_provider = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results_and_attempts(
            result.agent.tool_results.clone(),
            old_attempts,
            result.reply.clone(),
        );
    let old_response = test_service_with_provider_and_tool_calling(old_provider, true)
        .respond(private_message("执行查询任务"))
        .await
        .unwrap();
    assert_eq!(
        old_response.diagnostics.unwrap()["agent_turn_status"],
        "partial_success"
    );
    let old_text = old_response.text.unwrap();
    assert!(old_text.contains("K349"));
    assert!(old_text.contains("【火车】"), "未关联原始轨迹会追加错误块");
}

async fn model_reply(name: &str) -> String {
    // 故意让模型宣称全部完成；真实第二步失败时必须由可信结果纠正。
    if name == "get_train_schedule" {
        let schedule = MockTrainExecutor::new()
            .query_train_schedule(TrainScheduleRequest {
                train_code: "K349".into(),
                travel_date: chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
            })
            .await
            .unwrap();
        format!(
            "查询全部完成\n{}",
            crate::runtime::tools::train::format_train_schedule_reply(&schedule).text
        )
    } else {
        "查询全部完成：已取得公开铁路资料".into()
    }
}

#[tokio::test]
async fn train_redundant_missing_arguments_keeps_fallback_when_finalization_fails_or_is_empty() {
    for finalization in [
        Err(LlmError::new(
            "context_budget_exceeded",
            "final answer generation failed",
            "tool_loop",
        )),
        Ok(String::new()),
    ] {
        let text = reproduce_with_finalization(
            "get_train_schedule",
            &[
                serde_json::json!({"train_code":"K349", "travel_date":"2026-09-10"}),
                serde_json::json!({"travel_date":"2026-09-10"}),
            ],
            true,
            "succeeded",
            Some(finalization),
        )
        .await;
        for fact in ["K349", "北京南", "上海虹桥", "11:24"] {
            assert!(text.contains(fact), "确定性回退必须保留时刻表：{fact}");
        }
        assert!(!text.contains("参数不完整"));
        assert!(!text.contains("【火车】"));
        assert!(!text.contains("final answer generation failed"));
        assert!(!text.contains("查询全部完成"));
    }
}
