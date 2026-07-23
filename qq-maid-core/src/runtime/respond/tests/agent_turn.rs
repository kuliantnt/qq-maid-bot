//! 通用 Agent Tool Turn 的跨工具结果编排测试。
//!
//! 这里只验证 Respond 对可信 Tool outcome 的组合、顺序、状态和用户可见结果；
//! 具体工具参数、存储和领域状态机由各工具域测试负责。

use qq_maid_llm::provider::{ToolCallingProtocol, ToolExecutionAttempt};
use serde_json::Value;

use crate::runtime::tools::todo::{TodoItemDraft, TodoStore, TodoTimePrecision};

use super::support::*;

#[tokio::test]
async fn multi_entity_web_search_fact_card_preserves_model_summary_without_empty_hint() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![raw_tool_result(
                "web_search",
                serde_json::json!({
                    "ok": true,
                    "mode": "multi_entity_research",
                    "results": [{
                        "entity": "项目甲",
                        "status": "success",
                        "facts": "项目甲适合场景 A",
                        "sources": [{
                            "title": "项目甲文档",
                            "url": "https://example.test/project-a",
                            "snippet": "公开资料摘要"
                        }]
                    }, {
                        "entity": "项目乙",
                        "status": "success",
                        "facts": "项目乙适合场景 B",
                        "sources": []
                    }]
                }),
                true,
            )],
            "综合来看，项目甲偏向场景 A，项目乙偏向场景 B。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);

    let response = service
        .respond(private_message("联网对比项目甲和项目乙"))
        .await
        .unwrap();

    let text = response.text.as_deref().unwrap();
    assert!(text.contains("项目甲适合场景 A"));
    assert!(text.contains("综合来看，项目甲偏向场景 A"));
    assert!(!text.contains("没查到明确结果"));
    assert_eq!(response.command.as_deref(), Some("web_search"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "trusted");
}

#[tokio::test]
async fn duplicate_web_search_keeps_first_card_and_model_reply_without_counting_cache_hit() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "web_search",
                    serde_json::json!({
                        "ok": true,
                        "mode": "multi_entity_research",
                        "successful": 1,
                        "failed": 0,
                        "results": [{
                            "entity": "项目甲",
                            "status": "success",
                            "facts": "首次搜索的可信答案",
                            "sources": [{
                                "title": "首次搜索来源",
                                "url": "https://example.test/first",
                                "snippet": "首次搜索摘要"
                            }]
                        }]
                    }),
                    true,
                ),
                raw_tool_result(
                    "web_search",
                    serde_json::json!({
                        "ok": true,
                        "deduplicated": true,
                        "message": "已使用本次请求中相同检索的已有证据。"
                    }),
                    true,
                ),
            ],
            "模型基于首次搜索证据生成的最终回答。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);

    let response = service
        .respond(private_message("联网搜索项目甲两次后总结"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert_eq!(text.matches("【联网查询】").count(), 1);
    assert!(text.contains("首次搜索的可信答案"));
    assert!(text.contains("首次搜索来源"));
    assert!(text.contains("模型基于首次搜索证据生成的最终回答"));
    assert!(!text.contains("没查到明确结果"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_tool_results"].as_array().unwrap().len(),
        2
    );
    let outcomes = diagnostics["tool_outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["status"], "succeeded");
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["error_code"], Value::Null);
}

#[tokio::test]
async fn first_empty_web_search_still_renders_empty_result_hint() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![raw_tool_result(
                "web_search",
                serde_json::json!({"ok": true, "answer": "", "sources": []}),
                true,
            )],
            "模型确认当前没有更明确的公开结果。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);

    let response = service
        .respond(private_message("搜索一个没有结果的公开项目"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert_eq!(text.matches("【联网查询】").count(), 1);
    assert_eq!(text.matches("没查到明确结果").count(), 1);
    assert!(text.contains("模型确认当前没有更明确的公开结果"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["tool_outcomes"].as_array().unwrap().len(), 1);
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
}

#[tokio::test]
async fn web_search_retry_renders_only_final_empty_result_and_keeps_attempt_trace() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results_and_attempts(
            vec![
                raw_tool_result(
                    "web_search",
                    serde_json::json!({
                        "ok": false,
                        "error": {"code": "empty_result", "stage": "web_search"}
                    }),
                    false,
                ),
                raw_tool_result(
                    "web_search",
                    serde_json::json!({"ok": true, "answer": ""}),
                    true,
                ),
            ],
            vec![
                ToolExecutionAttempt {
                    result_index: 0,
                    call_id: "call-1".to_owned(),
                    round: 0,
                    retry_of: None,
                },
                ToolExecutionAttempt {
                    result_index: 1,
                    call_id: "call-2".to_owned(),
                    round: 1,
                    retry_of: Some(0),
                },
            ],
            "模型最终回复",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);

    let response = service
        .respond(private_message("搜索公开项目"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert_eq!(text.matches("【联网查询】").count(), 1);
    assert_eq!(text.matches("没查到明确结果").count(), 1);
    assert!(!text.contains("联网查询服务暂时不可用"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_tool_results"].as_array().unwrap().len(),
        2
    );
    assert_eq!(diagnostics["tool_outcomes"].as_array().unwrap().len(), 1);
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
    assert_eq!(diagnostics["tool_retry_count"], 1);
    assert!(diagnostics.get("tool_attempts").is_none());
}

#[tokio::test]
async fn web_search_retry_renders_final_success_without_previous_failure() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results_and_attempts(
            vec![
                raw_tool_result(
                    "web_search",
                    serde_json::json!({
                        "ok": false,
                        "error": {"code": "empty_result", "stage": "web_search"}
                    }),
                    false,
                ),
                raw_tool_result(
                    "web_search",
                    serde_json::json!({"ok": true, "answer": "项目最终结果"}),
                    true,
                ),
            ],
            vec![
                ToolExecutionAttempt {
                    result_index: 0,
                    call_id: "call-1".to_owned(),
                    round: 0,
                    retry_of: None,
                },
                ToolExecutionAttempt {
                    result_index: 1,
                    call_id: "call-2".to_owned(),
                    round: 1,
                    retry_of: Some(0),
                },
            ],
            "模型最终回复",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);

    let response = service
        .respond(private_message("搜索公开项目"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert_eq!(text.matches("【联网查询】").count(), 1);
    assert!(text.contains("项目最终结果"));
    assert!(!text.contains("联网查询服务暂时不可用"));
}

#[tokio::test]
async fn cross_candidate_retry_projection_keeps_prior_result_and_hides_only_retried_failure() {
    // 模拟累计 diagnostics：候选 A 成功一次，候选 B 失败后重试成功。
    // retry_of 使用全局下标 1，不得把候选 A 的结果 0 误隐藏。
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results_and_attempts(
            vec![
                raw_tool_result(
                    "web_search",
                    serde_json::json!({"ok": true, "answer": "候选A结果"}),
                    true,
                ),
                raw_tool_result(
                    "web_search",
                    serde_json::json!({
                        "ok": false,
                        "error": {"code": "empty_result", "stage": "web_search"}
                    }),
                    false,
                ),
                raw_tool_result(
                    "web_search",
                    serde_json::json!({"ok": true, "answer": "候选B最终结果"}),
                    true,
                ),
            ],
            vec![
                ToolExecutionAttempt {
                    result_index: 0,
                    call_id: "a1".to_owned(),
                    round: 0,
                    retry_of: None,
                },
                ToolExecutionAttempt {
                    result_index: 1,
                    call_id: "b1".to_owned(),
                    round: 0,
                    retry_of: None,
                },
                ToolExecutionAttempt {
                    result_index: 2,
                    call_id: "b2".to_owned(),
                    round: 1,
                    retry_of: Some(1),
                },
            ],
            "模型最终回复",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);

    let response = service
        .respond(private_message("搜索公开项目"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert_eq!(text.matches("【联网查询】").count(), 2);
    assert!(text.contains("候选A结果"));
    assert!(text.contains("候选B最终结果"));
    assert!(!text.contains("联网查询服务暂时不可用"));
    assert!(!text.contains("没查到明确结果"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_tool_results"].as_array().unwrap().len(),
        3
    );
    assert_eq!(diagnostics["tool_outcomes"].as_array().unwrap().len(), 2);
    assert_eq!(diagnostics["tool_retry_count"], 1);
}

#[tokio::test]
async fn independent_web_search_results_are_rendered_separately() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results_and_attempts(
            vec![
                raw_tool_result(
                    "web_search",
                    serde_json::json!({"ok": true, "answer": "项目甲结果"}),
                    true,
                ),
                raw_tool_result(
                    "web_search",
                    serde_json::json!({"ok": true, "answer": "项目乙结果"}),
                    true,
                ),
            ],
            vec![
                ToolExecutionAttempt {
                    result_index: 0,
                    call_id: "call-1".to_owned(),
                    round: 0,
                    retry_of: None,
                },
                ToolExecutionAttempt {
                    result_index: 1,
                    call_id: "call-2".to_owned(),
                    round: 0,
                    retry_of: None,
                },
            ],
            "模型最终回复",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);

    let response = service
        .respond(private_message("搜索两个项目"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert_eq!(text.matches("【联网查询】").count(), 2);
    assert!(text.contains("项目甲结果"));
    assert!(text.contains("项目乙结果"));
    assert_eq!(response.diagnostics.unwrap()["tool_retry_count"], 0);
}

#[tokio::test]
async fn todo_tool_ok_false_without_error_code_is_failed_outcome() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![raw_tool_result(
                "edit_todo",
                serde_json::json!({
                    "ok": false,
                    "message": "没有成功修改待办"
                }),
                false,
            )],
            "已修改待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("把第一条待办改成新标题"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_tool_error"));
    let text = response.text.unwrap();
    assert!(text.contains("没有成功修改待办"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "failed");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "failed");
    assert_eq!(diagnostics["tool_outcomes"][0]["error_code"], Value::Null);
    assert_eq!(diagnostics["todo_success_verified"], false);
}

#[tokio::test]
async fn todo_clarification_is_not_marked_as_write_success() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![raw_tool_result(
                "complete_todos",
                serde_json::json!({
                    "ok": false,
                    "requires_clarification": true,
                    "question": "请说明要完成哪条待办。"
                }),
                false,
            )],
            "已完成待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service.respond(private_message("完成待办")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_wait"));
    let text = response.text.unwrap();
    assert!(text.contains("请说明要完成哪条待办"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "requires_clarification");
    assert_eq!(
        diagnostics["tool_outcomes"][0]["status"],
        "requires_clarification"
    );
    assert_eq!(diagnostics["todo_success_verified"], false);
}

#[tokio::test]
async fn todo_business_failure_keeps_root_error_before_dependency_skip() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "delete_todos",
                    serde_json::json!({
                        "ok": false,
                        "error_code": "todo_selection_not_found",
                        "message": "没有找到符合条件的待办"
                    }),
                    false,
                ),
                raw_tool_result(
                    "complete_todos",
                    serde_json::json!({
                        "ok": false,
                        "skipped": true,
                        "reason": "dependency_previous_call_failed"
                    }),
                    false,
                ),
            ],
            "已处理",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("删除第一条再完成第二条"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_tool_error"));
    let text = response.text.unwrap();
    assert!(text.contains("没有找到符合条件的待办"));
    assert!(text.contains("前序工具没有成功"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "failed");
    assert_eq!(diagnostics["error_code"], "todo_selection_not_found");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "failed");
    assert_eq!(diagnostics["tool_outcomes"][1]["status"], "skipped");
}

#[tokio::test]
async fn todo_success_then_failure_is_partial_success_and_keeps_database_change() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                (
                    "create_todo",
                    r#"{"content":"新增后保留","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
                (
                    "edit_todo",
                    r#"{"number":99,"reference":null,"raw_text":"不应成功","title":"不应成功","detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "已处理",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let response = service
        .respond(private_message("新增一个待办再编辑不存在的待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("新增后保留"));
    assert!(text.contains("我现在没有可用的待办列表编号"));
    assert!(text.contains("可选待办"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "partial_success");
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
    assert_eq!(
        diagnostics["tool_outcomes"][1]["status"],
        "requires_clarification"
    );
    let todos = service.task_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "新增后保留");
}

#[tokio::test]
async fn multiple_successful_todo_writes_share_one_background_snapshot() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                (
                    "create_todo",
                    r#"{"content":"第一条新增","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
                (
                    "create_todo",
                    r#"{"content":"第二条新增","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "已新增最后一条",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let response = service
        .respond(private_message("新增两条待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert_eq!(text.matches("✅ 已新增待办").count(), 2);
    assert_eq!(text.matches("🚧 当前进行中").count(), 0);
    assert!(text.contains("第一条新增"));
    assert!(text.contains("第二条新增"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["tool_outcomes"].as_array().unwrap().len(), 2);
    let todos = service.task_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 2);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session
        .last_todo_query
        .expect("missing background refreshed snapshot");
    assert_eq!(snapshot.result_ids.len(), 2);
}

#[tokio::test]
async fn weather_success_and_todo_success_are_both_rendered_in_order() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                ("get_weather", r#"{"city":"杭州","forecast_days":3}"#),
                (
                    "create_todo",
                    r#"{"content":"出门带伞","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "杭州小雨，已新增带伞待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("查一下杭州天气，顺便加一个带伞待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    let weather_pos = text.find("杭州天气").expect("missing weather fact card");
    let todo_pos = text.find("✅ 已新增待办").expect("missing todo receipt");
    assert!(weather_pos < todo_pos);
    assert!(text.contains("当前 20:15"));
    assert!(text.contains("出门带伞"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["error_code"], Value::Null);
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "weather");
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "trusted");
    assert_eq!(diagnostics["tool_outcomes"][1]["domain"], "todo");
    assert_eq!(diagnostics["tool_outcomes"][1]["presentation"], "trusted");
}

#[tokio::test]
async fn readonly_weather_result_preserves_model_advice() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "get_weather",
            r#"{"city":"杭州","forecast_days":3}"#,
            "湿度偏高，户外运动建议降低强度，优先选清晨或室内。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("杭州天气怎么样，是不是要运动"))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("杭州天气"));
    assert!(text.contains("湿度偏高，户外运动建议降低强度"));
    assert_eq!(response.command.as_deref(), Some("weather"));
}

#[tokio::test]
async fn conditional_weather_and_todo_request_uses_tool_loop() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                ("get_weather", r#"{"city":"杭州","forecast_days":3}"#),
                (
                    "create_todo",
                    r#"{"content":"明天带伞","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "明天可能有雨，已新增带伞待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("如果明天下雨，帮我加个带伞的待办"))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    assert_ne!(response.command.as_deref(), Some("todo_due_date"));
    let text = response.text.as_deref().unwrap();
    assert!(!text.contains("这一天暂无未完成待办"));
    assert!(text.contains("杭州天气"));
    assert!(text.contains("✅ 已新增待办"));
}

#[tokio::test]
async fn weather_success_and_todo_failure_keep_fact_and_error() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                ("get_weather", r#"{"city":"杭州","forecast_days":3}"#),
                (
                    "edit_todo",
                    r#"{"number":99,"reference":null,"raw_text":"带伞","title":"带伞","detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "杭州天气已查，待办已修改",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("查杭州天气，再把不存在的待办改成带伞"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("杭州天气"));
    assert!(text.contains("我现在没有可用的待办列表编号"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "partial_success");
    assert_eq!(
        diagnostics["error_code"],
        "todo_visible_numbers_unavailable"
    );
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
    assert_eq!(
        diagnostics["tool_outcomes"][1]["status"],
        "requires_clarification"
    );
}

#[tokio::test]
async fn weather_failure_and_todo_success_keep_error_and_side_effect() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "get_weather",
                    serde_json::json!({
                        "ok": false,
                        "error": {
                            "code": "timeout",
                            "message": "upstream timed out",
                            "stage": "tool"
                        }
                    }),
                    false,
                ),
                raw_tool_result(
                    "create_todo",
                    serde_json::json!({
                        "ok": true,
                        "created": {
                            "title": "出门带伞",
                            "detail": null,
                            "display_time": "无时间"
                        }
                    }),
                    true,
                ),
            ],
            "已新增待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("查杭州天气，顺便加带伞待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("天气服务超时了"));
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("出门带伞"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "partial_success");
    assert_eq!(diagnostics["error_code"], "timeout");
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "trusted");
    assert_eq!(diagnostics["tool_outcomes"][1]["presentation"], "trusted");
}

#[tokio::test]
async fn weather_failure_and_dependency_skipped_todo_keep_root_cause() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "get_weather",
                    serde_json::json!({
                        "ok": false,
                        "error_code": "not_found",
                        "message": "city not found"
                    }),
                    false,
                ),
                raw_tool_result(
                    "create_todo",
                    serde_json::json!({
                        "ok": false,
                        "skipped": true,
                        "reason": "dependency_previous_call_failed"
                    }),
                    false,
                ),
            ],
            "已处理",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("查不存在城市天气后新增待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没找到这个城市"));
    assert!(text.contains("前序工具没有成功"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "failed");
    assert_eq!(diagnostics["error_code"], "not_found");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "failed");
    assert_eq!(diagnostics["tool_outcomes"][1]["status"], "skipped");
}

#[tokio::test]
async fn unadapted_success_with_todo_success_is_not_silently_dropped() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "unknown_tool",
                    serde_json::json!({
                        "ok": true,
                        "summary": "未知工具成功"
                    }),
                    true,
                ),
                raw_tool_result(
                    "create_todo",
                    serde_json::json!({
                        "ok": true,
                        "created": {
                            "title": "确认副作用",
                            "detail": null,
                            "display_time": "无时间"
                        }
                    }),
                    true,
                ),
            ],
            "未知工具成功，待办也已新增",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("新增待办并执行两个工具"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("确认副作用"));
    assert!(text.contains("部分工具结果未生成确定性展示"));
    assert!(text.contains("unknown_tool"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["error_code"], "tool_outcome_unhandled");
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "unhandled");
    assert_eq!(diagnostics["tool_outcomes"][1]["presentation"], "trusted");
}

#[tokio::test]
async fn unadapted_failure_with_todo_success_is_user_visible() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "unknown_tool",
                    serde_json::json!({
                        "ok": false,
                        "error_code": "unknown_failed",
                        "message": "internal detail should not be rendered"
                    }),
                    false,
                ),
                raw_tool_result(
                    "create_todo",
                    serde_json::json!({
                        "ok": true,
                        "created": {
                            "title": "仍然新增成功",
                            "detail": null,
                            "display_time": "无时间"
                        }
                    }),
                    true,
                ),
            ],
            "待办成功",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("执行未知工具并新增待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("仍然新增成功"));
    assert!(text.contains("unknown_tool"));
    assert!(text.contains("执行失败，当前没有可信错误展示适配器"));
    assert!(!text.contains("internal detail should not be rendered"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "partial_success");
    assert_eq!(diagnostics["error_code"], "unknown_failed");
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "unhandled");
}

#[tokio::test]
async fn only_weather_tool_renders_fact_card() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "get_weather",
            r#"{"city":"杭州","forecast_days":3}"#,
            "杭州天气如下",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service.respond(private_message("杭州天气")).await.unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("杭州天气"));
    assert!(text.contains("未来 3 天"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["todo_tool_results"], serde_json::json!([]));
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "weather");
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "trusted");
}

#[tokio::test]
async fn only_list_todos_success_does_not_claim_todo_write_success() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json("list_todos", r#"{"status":"pending"}"#, "当前待办列表");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "只读查询不算写入".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();

    let response = service
        .respond(private_message("检查待办状态"))
        .await
        .unwrap();

    let visible_snapshot = response
        .visible_entity_snapshot
        .as_ref()
        .expect("visible list response should carry snapshot");
    assert_eq!(visible_snapshot.items.len(), 1);
    assert_eq!(visible_snapshot.items[0].visible_number, 1);
    assert_eq!(visible_snapshot.items[0].domain, "todo");

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "todo");
    assert_eq!(diagnostics["tool_outcomes"][0]["effect"], "read_only");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
}
