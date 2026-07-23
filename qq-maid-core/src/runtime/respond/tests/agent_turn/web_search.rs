//! Web Search Tool outcome 的 Respond 编排测试。

use super::*;

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
