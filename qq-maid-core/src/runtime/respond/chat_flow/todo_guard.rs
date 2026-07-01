//! 普通 Chat flow 的 Todo 成功文案守卫。
//!
//! 本模块只做“输出验真”：当模型回复声称已新增、已修改、已完成或已删除
//! Todo 时，必须能在本轮 Tool Loop 结果里看到真实成功的 Todo 写工具输出。
//! 这里不再根据用户输入猜测本轮应该调用哪个工具，避免路由、模型和守卫三套
//! 意图判断互相冲突。

use serde_json::Value;

use super::super::llm_service::RespondOutput;
use crate::provider::ToolExecutionResult;

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

fn has_successful_todo_write_result(output: &RespondOutput) -> bool {
    output.tool_results.iter().any(successful_todo_write_result)
}

fn successful_todo_write_result(result: &ToolExecutionResult) -> bool {
    if !result.succeeded || result_has_explicit_failure(&result.output) {
        return false;
    }
    match result.name.as_str() {
        "create_todo" => pending_action_matches(&result.output, "create"),
        "cancel_todo" => pending_action_matches(&result.output, "cancel"),
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

fn reply_claims_todo_write_success(reply: &str) -> bool {
    let text = reply.trim();
    if text.is_empty() || looks_like_non_success_explanation(text) {
        return false;
    }
    let normalized: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    let success_markers = [
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
    // 不读取用户输入、不推断“本轮必须调用哪个工具”；这里只从模型最终回复
    // 本身识别高风险成功文案，避免无 Tool 结果时透传“已新增/已删除”。
    if success_markers
        .iter()
        .any(|marker| normalized.starts_with(marker))
    {
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
    contains_any(&normalized, &success_markers)
}

fn looks_like_non_success_explanation(text: &str) -> bool {
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

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

pub(super) fn todo_success_not_verified_reply() -> String {
    "我这次没有收到待办工具的成功回执，所以不能确认已经完成该待办操作。请再说一次，或使用 /todo 查看当前待办状态。".to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        provider::ToolExecutionResult,
        runtime::respond::{llm_service::RespondOutput, types::ChatResponse},
        util::metrics::LlmMetrics,
    };

    use super::{TodoSuccessValidation, validate_todo_success_reply};

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
    }
}
