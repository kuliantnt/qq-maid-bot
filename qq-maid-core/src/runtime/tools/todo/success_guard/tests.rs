//! Todo 成功声明验真规则的单元测试。

use qq_maid_llm::provider::ToolExecutionResult;
use serde_json::json;

use crate::runtime::tools::todo::success_guard::todo_success_not_verified_reply_for_tool_results;

use super::{TodoSuccessValidation, TodoSuccessVerificationScope};

struct TestOutput {
    reply: String,
    tool_results: Vec<ToolExecutionResult>,
}

fn output(reply: &str, tool_results: Vec<ToolExecutionResult>) -> TestOutput {
    TestOutput {
        reply: reply.to_owned(),
        tool_results,
    }
}

fn validate_todo_success_reply(output: &TestOutput) -> TodoSuccessValidation {
    super::validate_todo_success_reply(
        &output.reply,
        &output.tool_results,
        TodoSuccessVerificationScope::ExplicitMutation,
    )
}

fn todo_success_not_verified_reply_for_output(output: &TestOutput) -> String {
    todo_success_not_verified_reply_for_tool_results(&output.tool_results)
}

fn tool_result(name: &str, value: serde_json::Value, succeeded: bool) -> ToolExecutionResult {
    ToolExecutionResult {
        name: name.to_owned(),
        output: value,
        succeeded,
    }
}

#[test]
fn non_success_chat_passes_without_tool_result() {
    assert_eq!(
        validate_todo_success_reply(&output("晚上好，今天想聊点什么？", Vec::new())),
        TodoSuccessValidation::Passed {
            claimed_success: false
        }
    );
}

#[test]
fn implicit_todo_success_candidates_require_time_and_task_behavior() {
    for input in [
        "明天开会",
        "下午整理一下",
        "晚上交水电费",
        "明天买菜",
        "下午买东西",
        "晚上买药",
        "明天跟进项目",
        "下午复盘",
        "明天验收",
        "晚上发送邮件",
        "明天发布公告",
        "下午发版",
        "晚上完成初稿",
        "明天完成草稿",
    ] {
        assert_eq!(
            super::todo_success_verification_scope(input, false),
            TodoSuccessVerificationScope::ImplicitCreate,
            "省略式任务应进入成功验真：{input}"
        );
    }

    for input in [
        "开会时怎么活跃气氛",
        "整理 Rust 所有权知识点的思路",
        "明天陪我聊聊",
        "晚上分析一下架构",
        "下午写一段文案",
        "明天解释这段日志",
    ] {
        assert_eq!(
            super::todo_success_verification_scope(input, false),
            TodoSuccessVerificationScope::None,
            "非省略式任务不应进入成功验真：{input}"
        );
    }
}

#[test]
fn explicit_and_implicit_candidates_use_distinct_success_claim_scopes() {
    assert_eq!(
        super::todo_success_verification_scope("帮我记一个待办，明天开会", false),
        TodoSuccessVerificationScope::ExplicitMutation
    );

    for reply in ["已完成检查，根因是配置错误", "已更新分析结果，结论如下"]
    {
        assert_eq!(
            super::validate_todo_success_reply(
                reply,
                &[],
                TodoSuccessVerificationScope::ImplicitCreate,
            ),
            TodoSuccessValidation::Passed {
                claimed_success: false
            },
            "省略式创建不应拦截普通执行文案：{reply}"
        );
    }
    for reply in [
        "已记录为待办",
        "已经创建待办：明天开会",
        "已经发起新增确认",
        "杭州明天晴，另外已记录明天买菜。",
        "天气已查询，也已经创建买菜提醒。",
    ] {
        assert_eq!(
            super::validate_todo_success_reply(
                reply,
                &[],
                TodoSuccessVerificationScope::ImplicitCreate,
            ),
            TodoSuccessValidation::Blocked,
            "省略式创建仍应拦截创建成功声明：{reply}"
        );
    }
    for marker in super::TODO_CREATE_SUCCESS_MARKERS {
        let reply = format!("天气已查询，另外{marker}买菜提醒。");
        assert_eq!(
            super::validate_todo_success_reply(
                &reply,
                &[],
                TodoSuccessVerificationScope::ImplicitCreate,
            ),
            TodoSuccessValidation::Blocked,
            "省略式创建应在任意位置拦截全部创建标记：{marker}"
        );
    }

    for reply in [
        "天气已查询，也已完成出行分析。",
        "搜索完成，已修改分析结论。",
        "资料已更新，建议明天再确认。",
        "旧记录已删除，可以重新分析。",
    ] {
        assert_eq!(
            super::validate_todo_success_reply(
                reply,
                &[],
                TodoSuccessVerificationScope::ImplicitCreate,
            ),
            TodoSuccessValidation::Passed {
                claimed_success: false
            },
            "省略式创建不应检查其他写操作标记：{reply}"
        );
    }
}

#[test]
fn explicit_non_success_explanation_passes_without_tool_result() {
    assert_eq!(
        validate_todo_success_reply(&output(
            "这次没有确认改动成功。请先查看最新待办列表，再按编号操作一次。",
            Vec::new()
        )),
        TodoSuccessValidation::Passed {
            claimed_success: false
        }
    );
}

#[test]
fn capability_and_status_explanations_pass_without_tool_result() {
    for reply in [
        "可以删除已完成待办，但需要先列出并选择具体条目。",
        "暂不支持批量清理全部已完成待办；可以先查看已完成列表。",
        "请提供要删除的已完成待办编号；我还不能确认已经删除任何待办。",
        "请提供要删除的已完成待办编号；已删除项目不可恢复。",
        "已完成待办可以查看，也可以选择具体条目删除。",
    ] {
        assert_eq!(
            validate_todo_success_reply(&output(reply, Vec::new())),
            TodoSuccessValidation::Passed {
                claimed_success: false
            },
            "{reply}"
        );
    }
}

#[test]
fn todo_success_reply_without_tool_result_is_blocked() {
    assert_eq!(
        validate_todo_success_reply(&output("已新增待办：明天接老公", Vec::new())),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output("已新增：明天接老公", Vec::new())),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output("第二条待办已删除。", Vec::new())),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output("刚才那个待办已完成。", Vec::new())),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output("已删除待办，还要继续吗？", Vec::new())),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "已删除第一条待办，请先用 /todo 查看确认。",
            Vec::new()
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "暂不支持批量清理，但已删除第一条待办。",
            Vec::new()
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output("已完成待办已删除。", Vec::new())),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "请提供要删除的已完成待办编号；已删除第一条待办。",
            Vec::new()
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "请提供要删除的已完成待办编号；已删除项目不可恢复。第一条待办已删除。",
            Vec::new()
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output("第三条详情以后不会显示了。", Vec::new())),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output("第二条备注已清除。", Vec::new())),
        TodoSuccessValidation::Blocked
    );
}

#[test]
fn todo_success_reply_requires_successful_structured_result() {
    assert_eq!(
        validate_todo_success_reply(&output(
            "第二条待办已删除。",
            vec![tool_result(
                "delete_todos",
                json!({"ok": false, "message": "failed"}),
                false,
            )],
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "第二条待办已删除。",
            vec![tool_result(
                "delete_todos",
                json!({"ok": true, "requires_confirmation": true, "pending_action": "delete"}),
                true,
            )],
        )),
        TodoSuccessValidation::Passed {
            claimed_success: true
        }
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "已新增待办：明天接老公",
            vec![tool_result(
                "create_todo",
                json!({"ok": true, "created": {"title": "明天接老公"}}),
                true,
            )],
        )),
        TodoSuccessValidation::Passed {
            claimed_success: true
        }
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "已新增待办：明天接老公",
            vec![tool_result(
                "create_todo",
                json!({"ok": true, "requires_confirmation": true, "pending_action": "create"}),
                true,
            )],
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "第三条详情已清除。",
            vec![tool_result(
                "create_todo",
                json!({"ok": true, "created": {"title": "明天接老公"}}),
                true,
            )],
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "第三条详情已清除。",
            vec![tool_result(
                "edit_todo",
                json!({"ok": true, "updated": {"detail": "原详情仍然存在"}}),
                true,
            )],
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "第三条详情已清除。",
            vec![tool_result(
                "edit_todo",
                json!({"ok": true, "updated": {"title": "新标题", "detail": "旧详情"}}),
                true,
            )],
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "第三条详情已清除。",
            vec![tool_result(
                "edit_todo",
                json!({"ok": true, "updated": {"title": "新标题"}}),
                true,
            )],
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "第三条详情已清除。",
            vec![tool_result(
                "edit_todo",
                json!({"ok": false, "updated": {"title": "检查日志", "detail": null}}),
                true,
            )],
        )),
        TodoSuccessValidation::Blocked
    );
    assert_eq!(
        validate_todo_success_reply(&output(
            "第三条详情已清除。",
            vec![tool_result(
                "edit_todo",
                json!({"ok": true, "updated": {"title": "检查日志", "detail": null}}),
                true,
            )],
        )),
        TodoSuccessValidation::Passed {
            claimed_success: true
        }
    );
}

#[test]
fn tool_failure_reply_prefers_business_error_over_dependency_skip() {
    let output = output(
        "已删除待办。",
        vec![
            tool_result(
                "delete_todos",
                json!({
                    "ok": false,
                    "error_code": "todo_selection_not_found",
                    "message": "no completed or cancelled todo matched query",
                }),
                false,
            ),
            tool_result(
                "complete_todos",
                json!({
                    "ok": false,
                    "skipped": true,
                    "reason": "dependency_previous_call_failed",
                }),
                false,
            ),
        ],
    );

    let reply = todo_success_not_verified_reply_for_output(&output);
    assert!(reply.contains("没有找到可删除的已完成待办"));
}
