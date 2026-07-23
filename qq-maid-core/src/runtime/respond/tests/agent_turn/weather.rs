//! 天气与 Todo 混合 Agent Turn 的结果编排回归测试。

use super::*;

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
async fn weather_only_outcome_does_not_bypass_implicit_todo_success_verification() {
    for reply in [
        "杭州明天晴，另外已记录明天买菜。",
        "天气已查询，也已记录买菜提醒。",
    ] {
        let inspector = MockProvider::new()
            .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
            .with_tool_call_json("get_weather", r#"{"city":"杭州","forecast_days":3}"#, reply);
        let service = test_service_with_provider_and_tool_calling(inspector, true);
        let owner = TodoStore::owner(Some("u1"), "private:u1");

        let response = service
            .respond(private_message("查一下明天天气，顺便明天买菜"))
            .await
            .unwrap();

        let text = response.text.as_deref().unwrap();
        assert!(text.contains("这次没有确认改动成功"), "{reply}: {text}");
        let diagnostics = response.diagnostics.unwrap();
        assert_eq!(
            diagnostics["error_code"], "todo_success_not_verified",
            "{reply}"
        );
        assert_eq!(diagnostics["todo_success_claimed"], true, "{reply}");
        assert_eq!(diagnostics["todo_success_verified"], false, "{reply}");
        assert_eq!(
            diagnostics["agent_tool_results"][0]["name"], "get_weather",
            "{reply}"
        );
        assert_eq!(
            diagnostics["agent_tool_results"][0]["succeeded"], true,
            "{reply}"
        );
        assert_eq!(
            diagnostics["tool_outcomes"][0]["domain"], "weather",
            "{reply}"
        );
        assert!(
            service.task_store.list_all(&owner).unwrap().is_empty(),
            "{reply}"
        );
    }
}

#[tokio::test]
async fn weather_only_outcome_preserves_non_todo_analysis_reply() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "get_weather",
            r#"{"city":"杭州","forecast_days":3}"#,
            "杭州明天晴，已完成分析，建议携带雨具。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);

    let response = service
        .respond(private_message("查一下明天天气，晚上分析一下出行方案"))
        .await
        .unwrap();

    let text = response.text.as_deref().unwrap();
    assert!(text.contains("杭州天气"), "{text}");
    assert!(
        text.contains("杭州明天晴，已完成分析，建议携带雨具。"),
        "{text}"
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["error_code"], Value::Null);
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "weather");
}

#[tokio::test]
async fn weather_and_real_todo_create_keep_both_trusted_results() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                ("get_weather", r#"{"city":"杭州","forecast_days":3}"#),
                (
                    "create_todo",
                    r#"{"content":"买菜","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "天气已查询，也已记录买菜提醒。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let response = service
        .respond(private_message("查一下明天天气，顺便提醒我买菜"))
        .await
        .unwrap();

    let text = response.text.as_deref().unwrap();
    assert!(text.contains("杭州天气"), "{text}");
    assert!(text.contains("✅ 已新增待办"), "{text}");
    assert!(text.contains("买菜"), "{text}");
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["error_code"], Value::Null);
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "weather");
    assert_eq!(diagnostics["tool_outcomes"][1]["domain"], "todo");
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "trusted");
    assert_eq!(diagnostics["tool_outcomes"][1]["presentation"], "trusted");
    let todos = service.task_store.list_all(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "买菜");
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
