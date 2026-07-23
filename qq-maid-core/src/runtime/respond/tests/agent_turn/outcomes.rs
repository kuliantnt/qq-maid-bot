//! 未适配工具 outcome 与 Todo outcome 的组合编排测试。

use super::*;

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
