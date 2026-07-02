//! 普通 Chat flow 的 Todo 成功文案守卫。
//!
//! 本模块只做“输出验真”：当模型回复声称已新增、已修改、已完成或已删除
//! Todo 时，必须能在本轮 Tool Loop 结果里看到真实成功的 Todo 写工具输出。
//! 这里不再根据用户输入猜测本轮应该调用哪个工具，避免路由、模型和守卫三套
//! 意图判断互相冲突。

use serde_json::Value;

use super::super::llm_service::RespondOutput;
use crate::provider::ToolExecutionResult;

const TODO_WRITE_SUCCESS_MARKERS: &[&str] = &[
    "已新增",
    "已新建",
    "已创建",
    "已添加",
    "已记录",
    "已生成待确认",
    "已发起",
    "已完成",
    "已修改",
    "已更新",
    "已取消",
    "已恢复",
    "已删除",
    "已经新增",
    "已经新建",
    "已经创建",
    "已经添加",
    "已经记录",
    "已经完成",
    "已经修改",
    "已经更新",
    "已经取消",
    "已经恢复",
    "已经删除",
];

/// 判定模型是否可以安全透传 Todo 成功文案。
///
/// - 未声称 Todo 写入成功：直接放行。
/// - 声称成功：必须存在本轮真实成功的 Todo 写工具结果。
pub(super) fn validate_todo_success_reply(output: &RespondOutput) -> TodoSuccessValidation {
    if !reply_claims_todo_write_success(&output.reply) {
        return TodoSuccessValidation::Passed {
            claimed_success: false,
        };
    }
    if has_successful_todo_write_result(output) {
        TodoSuccessValidation::Passed {
            claimed_success: true,
        }
    } else {
        TodoSuccessValidation::Blocked
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TodoSuccessValidation {
    Passed { claimed_success: bool },
    Blocked,
}

impl TodoSuccessValidation {
    pub(super) fn claimed_success(self) -> bool {
        matches!(
            self,
            Self::Passed {
                claimed_success: true
            } | Self::Blocked
        )
    }

    pub(super) fn passed(self) -> bool {
        matches!(self, Self::Passed { .. })
    }
}

pub(super) fn todo_tool_result_summaries(output: &RespondOutput) -> Vec<TodoToolResultSummary> {
    output
        .tool_results
        .iter()
        .filter(|result| is_todo_tool(&result.name))
        .map(TodoToolResultSummary::from)
        .collect()
}

fn has_successful_todo_write_result(output: &RespondOutput) -> bool {
    output.tool_results.iter().any(successful_todo_write_result)
}

fn successful_todo_write_result(result: &ToolExecutionResult) -> bool {
    if !result.succeeded || result_has_explicit_failure(&result.output) {
        return false;
    }
    match result.name.as_str() {
        "create_todo" => {
            result.output.get("created").is_some()
                || non_empty_array_field(&result.output, "created_items")
        }
        "cancel_todo" => non_empty_array_field(&result.output, "cancelled"),
        "delete_todos" => pending_action_matches(&result.output, "delete"),
        "edit_todo" => result.output.get("updated").is_some(),
        "complete_todos" => non_empty_array_field(&result.output, "completed"),
        "restore_todos" => non_empty_array_field(&result.output, "restored"),
        _ => false,
    }
}

fn result_has_explicit_failure(output: &Value) -> bool {
    output.get("ok").and_then(Value::as_bool) == Some(false)
}

fn pending_action_matches(output: &Value, action: &str) -> bool {
    output.get("requires_confirmation").and_then(Value::as_bool) == Some(true)
        && output.get("pending_action").and_then(Value::as_str) == Some(action)
}

fn non_empty_array_field(output: &Value, field: &str) -> bool {
    output
        .get(field)
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TodoToolResultSummary {
    pub(super) tool: String,
    pub(super) succeeded: bool,
    pub(super) error_code: Option<String>,
    pub(super) requires_confirmation: bool,
    pub(super) requires_clarification: bool,
    pub(super) pending_action: Option<String>,
    pub(super) exception: bool,
    pub(super) skipped: bool,
    pub(super) skip_reason: Option<String>,
}

impl From<&ToolExecutionResult> for TodoToolResultSummary {
    fn from(result: &ToolExecutionResult) -> Self {
        Self {
            tool: result.name.clone(),
            succeeded: result.succeeded && !result_has_explicit_failure(&result.output),
            error_code: structured_error_code(&result.output),
            requires_confirmation: result
                .output
                .get("requires_confirmation")
                .and_then(Value::as_bool)
                == Some(true),
            requires_clarification: result
                .output
                .get("requires_clarification")
                .and_then(Value::as_bool)
                == Some(true),
            pending_action: result
                .output
                .get("pending_action")
                .and_then(Value::as_str)
                .map(str::to_owned),
            exception: result.output.get("error").is_some(),
            skipped: result.output.get("skipped").and_then(Value::as_bool) == Some(true),
            skip_reason: result
                .output
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }
}

fn structured_error_code(output: &Value) -> Option<String> {
    output
        .get("error_code")
        .and_then(Value::as_str)
        .or_else(|| {
            output
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn is_todo_tool(name: &str) -> bool {
    matches!(
        name,
        "create_todo"
            | "cancel_todo"
            | "delete_todos"
            | "edit_todo"
            | "complete_todos"
            | "restore_todos"
    )
}

fn reply_claims_todo_write_success(reply: &str) -> bool {
    let text = reply.trim();
    if text.is_empty() || explicitly_denies_todo_success(text) {
        return false;
    }
    let normalized: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    if looks_like_todo_status_or_capability_explanation(&normalized) {
        return false;
    }
    // 不读取用户输入、不推断“本轮必须调用哪个工具”；这里只从模型最终回复
    // 本身识别高风险成功文案，避免无 Tool 结果时透传“已新增/已删除”。
    if starts_with_todo_success_marker(&normalized) {
        return true;
    }

    let has_todo_context = contains_any(
        &normalized,
        &[
            "待办",
            "任务",
            "todo",
            "Todo",
            "草稿",
            "确认",
            "第一条",
            "第二条",
            "第三条",
            "第1条",
            "第2条",
            "第3条",
            "刚才那个",
            "刚刚那条",
            "那个",
            "它",
        ],
    );
    if !has_todo_context {
        return false;
    }
    contains_todo_success_marker(&normalized)
}

fn explicitly_denies_todo_success(text: &str) -> bool {
    contains_any(
        text,
        &[
            "没有真正执行",
            "没有执行",
            "未执行",
            "无法确认",
            "不能确认",
            "没有收到",
            "没有调用",
            "不能算",
            "不算",
        ],
    )
}

fn looks_like_todo_status_or_capability_explanation(text: &str) -> bool {
    // “已完成待办 / 已取消待办”常用于列表状态、能力说明或规则解释，
    // 不能等同于“我已经把某条待办完成/取消”。真正的动作成功文案仍会被
    // 后续“待办 + 已完成/已删除”等组合拦截；但风险提示里的“已删除项目不可恢复”
    // 不应反向覆盖前面的缺参 / 能力说明。
    let starts_with_status = [
        "已完成待办",
        "已取消待办",
        "已完成的待办",
        "已取消的待办",
        "已完成列表",
        "已取消列表",
    ]
    .iter()
    .any(|marker| text.starts_with(marker));
    if starts_with_status
        && allowlist_marker_without_action_success(
            text,
            &[
                "可以删除",
                "可以查看",
                "可以查询",
                "可以恢复",
                "不能删除",
                "不能直接删除",
                "不支持删除",
                "暂不支持",
                "查看",
                "查询",
            ],
        )
    {
        return true;
    }
    allowlist_marker_without_action_success(
        text,
        &[
            "请提供要删除的已完成待办",
            "请提供要删除的已完成的待办",
            "请先查看已完成列表",
            "请先查询已完成列表",
            "需要先列出",
            "需要先查看",
            "需要先查询",
            "可以删除已完成待办",
            "可以删除已完成的待办",
            "可以查看已完成待办",
            "可以查看已完成的待办",
            "可以查询已完成待办",
            "可以查询已完成的待办",
            "当前不支持一句话批量清理全部已完成待办",
            "暂不支持批量清理全部已完成待办",
            "暂不支持一句话批量清理全部已完成待办",
            "支持删除已完成待办",
            "支持删除已完成的待办",
        ],
    )
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn contains_todo_success_marker(text: &str) -> bool {
    contains_any(text, TODO_WRITE_SUCCESS_MARKERS)
}

fn starts_with_todo_success_marker(text: &str) -> bool {
    TODO_WRITE_SUCCESS_MARKERS
        .iter()
        .any(|marker| text.starts_with(marker))
}

fn allowlist_marker_without_action_success(text: &str, allowlist_markers: &[&str]) -> bool {
    contains_any(text, allowlist_markers) && !contains_clear_todo_action_success_marker(text)
}

fn contains_clear_todo_action_success_marker(text: &str) -> bool {
    [
        "已新增",
        "已新建",
        "已创建",
        "已添加",
        "已记录",
        "已生成待确认",
        "已发起",
        "已修改",
        "已更新",
        "已恢复",
        "已删除",
        "已经新增",
        "已经新建",
        "已经创建",
        "已经添加",
        "已经记录",
        "已经修改",
        "已经更新",
        "已经恢复",
        "已经删除",
    ]
    .iter()
    .any(|marker| {
        text.match_indices(marker)
            .any(|(pos, _)| !is_explanatory_clear_success_usage(text, pos, marker))
    })
}

fn is_explanatory_clear_success_usage(text: &str, pos: usize, marker: &str) -> bool {
    // “已删除项目不可恢复”是缺参/能力说明里的风险提示，不表示本轮已经删除待办。
    marker == "已删除" && text[pos..].starts_with("已删除项目不可恢复")
}

pub(super) fn todo_success_not_verified_reply() -> String {
    "我这次没有收到待办工具的成功回执，所以不能确认已经完成该待办操作。请再说一次，或使用 /todo 查看当前待办状态。".to_owned()
}

pub(super) fn todo_success_not_verified_reply_for_output(output: &RespondOutput) -> String {
    let summaries = todo_tool_result_summaries(output);
    if summaries.is_empty() {
        return todo_success_not_verified_reply();
    }
    if let Some(summary) = summaries.iter().find(|summary| {
        summary.succeeded && summary.requires_confirmation && summary.pending_action.is_some()
    }) {
        let action = match summary.pending_action.as_deref() {
            Some("delete") => "删除",
            Some("cancel") => "取消",
            Some("create") => "新增",
            Some("edit") => "修改",
            Some("complete") => "完成",
            Some("restore") => "恢复",
            _ => "待办",
        };
        return format!("已发起{action}待办确认，请回复“确认”继续，或回复“取消”放弃。");
    }
    if let Some(summary) = best_failure_summary(&summaries) {
        return todo_tool_failure_reply(summary);
    }
    "待办工具已返回结果，但这次回复没有通过成功验真。请使用 /todo 查看当前待办状态。".to_owned()
}

fn best_failure_summary(summaries: &[TodoToolResultSummary]) -> Option<&TodoToolResultSummary> {
    let failures = summaries
        .iter()
        .filter(|summary| !summary.succeeded)
        .collect::<Vec<_>>();
    failures
        .iter()
        .copied()
        .find(|summary| summary.error_code.is_some() && !summary.exception)
        .or_else(|| failures.iter().copied().find(|summary| summary.exception))
        .or_else(|| {
            failures
                .iter()
                .copied()
                .find(|summary| summary.requires_clarification)
        })
        .or_else(|| failures.iter().copied().find(|summary| summary.skipped))
        .or_else(|| failures.first().copied())
}

fn todo_tool_failure_reply(summary: &TodoToolResultSummary) -> String {
    match summary.error_code.as_deref() {
        Some("todo_delete_invalid_state") => {
            "目标待办当前无法永久删除，请查看最新列表后再试。".to_owned()
        }
        Some("todo_selection_not_found") if summary.tool == "delete_todos" => {
            "没有找到可删除的已完成或已取消待办，请先查看对应列表后再选择。".to_owned()
        }
        Some("todo_selection_not_found") => "没有找到匹配的待办，请先查看列表后再选择。".to_owned(),
        Some("todo_reference_unavailable") | Some("todo_visible_numbers_unavailable") => {
            "目标不明确，请先查看待办列表，再选择具体编号。".to_owned()
        }
        Some("todo_reference_invalid_state") => {
            "当前状态的待办不能执行这项操作，请先查看列表确认目标状态。".to_owned()
        }
        Some("pending_operation_exists") => {
            "当前已有待确认操作，请先回复“确认”或“取消”，再继续新的待办操作。".to_owned()
        }
        Some("bad_tool_arguments") if summary.requires_clarification => {
            "目标不明确，请选择具体待办。".to_owned()
        }
        Some("bad_tool_arguments") => "这次待办工具参数不完整，请换个说法说明目标。".to_owned(),
        Some(_) if summary.exception => {
            "待办工具执行时发生异常，未确认完成操作。请稍后重试或先查看当前待办状态。".to_owned()
        }
        Some(_) => "待办工具返回业务失败，未确认完成操作。请先查看当前待办状态。".to_owned(),
        None if summary.requires_clarification => "目标不明确，请选择具体待办。".to_owned(),
        None if summary.skipped => {
            "前一个待办工具没有成功，后续待办工具已跳过；本轮未确认完成操作。".to_owned()
        }
        None => todo_success_not_verified_reply(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        provider::ToolExecutionResult,
        runtime::respond::{llm_service::RespondOutput, types::ChatResponse},
        util::metrics::LlmMetrics,
    };

    use super::{
        TodoSuccessValidation, todo_success_not_verified_reply_for_output,
        validate_todo_success_reply,
    };

    fn output(reply: &str, tool_results: Vec<ToolExecutionResult>) -> RespondOutput {
        RespondOutput {
            reply: reply.to_owned(),
            text: reply.to_owned(),
            markdown: None,
            chat: ChatResponse::ok(
                reply.to_owned(),
                LlmMetrics {
                    provider: "test".to_owned(),
                    model: "test".to_owned(),
                    stream: false,
                    ttfe_ms: None,
                    ttft_ms: None,
                    total_latency_ms: 1,
                },
                None,
            ),
            executed_tools: tool_results
                .iter()
                .map(|result| result.name.clone())
                .collect(),
            tool_results,
        }
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
    fn explicit_non_success_explanation_passes_without_tool_result() {
        assert_eq!(
            validate_todo_success_reply(&output(
                "没有收到待办工具的成功回执，不能确认已经新增待办。",
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
        assert!(reply.contains("没有找到可删除的已完成或已取消待办"));
    }
}
