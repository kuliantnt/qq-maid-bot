//! Todo Tool Loop 成功文案守卫。
//!
//! 本模块只维护“成功边界”：独立判断用户输入是否需要启用验真，并在模型回复
//! 声称已新增、已修改、已完成或已删除 Todo 时，要求本轮 Tool Loop 存在真实成功
//! 的 Todo 写工具输出。候选判断不决定本轮应该调用哪个工具，也不参与 Tool 暴露、
//! 路由或执行，避免与状态提示和业务流程耦合。

use serde_json::Value;

use qq_maid_llm::provider::ToolExecutionResult;

use super::route::{self, TodoIntentAction};

// 省略式待办没有“待办/提醒”等显式对象，只能用时间线索与明确、可执行的任务行为
// 共同识别。这里维护 Todo 域正向行为，不通过公共闲聊/创作/解释排除词猜测意图。
const IMPLICIT_TODO_TASK_ACTION_MARKERS: &[&str] = &[
    "盯一下",
    "盯下",
    "看一下",
    "看下",
    "开会",
    "参加会议",
    "买菜",
    "买东西",
    "买药",
    "整理",
    "跟进",
    "出一版",
    "复盘",
    "验收",
    "发送",
    "发给",
    "发一下",
    "发布",
    "发版",
    "完成初稿",
    "完成草稿",
    "交水电费",
    "缴费",
    "交房租",
    "还款",
    "还书",
    "采购",
    "取件",
    "拿快递",
    "寄件",
    "送材料",
    "接人",
    "提交",
    "交作业",
    "打电话",
    "回电话",
    "回邮件",
    "回复邮件",
    "预约",
    "报名",
    "续费",
    "报销",
    "体检",
    "复诊",
    "吃药",
    "服药",
    "锻炼",
    "跑步",
    "检查",
    "复查",
    "维修",
    "打印",
    "备份",
];

const TODO_CREATE_SUCCESS_MARKERS: &[&str] = &[
    "已新增",
    "已新建",
    "已创建",
    "已添加",
    "已记录",
    "已生成待确认",
    "已发起",
    "已经新增",
    "已经新建",
    "已经创建",
    "已经添加",
    "已经记录",
    "已经生成待确认",
    "已经发起",
];

const TODO_OTHER_WRITE_SUCCESS_MARKERS: &[&str] = &[
    "已完成",
    "已修改",
    "已更新",
    "已取消",
    "已恢复",
    "已删除",
    "已跳过",
    "已关闭",
    "已经完成",
    "已经修改",
    "已经更新",
    "已经取消",
    "已经恢复",
    "已经删除",
    "已经跳过",
    "已经关闭",
];

/// Todo 成功声明需要验真的范围。
///
/// 该范围只供 Tool Turn 最终文案验真使用，不参与 Tool 暴露、路由或执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoSuccessVerificationScope {
    None,
    ExplicitMutation,
    ImplicitCreate,
}

/// 判定用户输入是否需要启用 Todo 成功文案验真。
///
/// 这是成功边界，不是用户状态提示分类：显式 Todo 写意图沿用既有强信号，另行覆盖
/// “时间表达 + 明确任务行为”的省略式创建请求。结果只决定是否核验模型最终文案，
/// 不参与 Tool 暴露、路由、参数解析或执行。
pub(crate) fn todo_success_verification_scope(
    text: &str,
    has_recent_todo_context: bool,
) -> TodoSuccessVerificationScope {
    let lower = text.to_ascii_lowercase();
    let intent = route::classify_todo_intent(text, &lower, has_recent_todo_context);
    if intent.is_confident() && !matches!(route::todo_intent_action(text), TodoIntentAction::Query)
    {
        return TodoSuccessVerificationScope::ExplicitMutation;
    }

    if route::looks_like_temporal_expression(text)
        && contains_any(text, IMPLICIT_TODO_TASK_ACTION_MARKERS)
    {
        return TodoSuccessVerificationScope::ImplicitCreate;
    }

    TodoSuccessVerificationScope::None
}

/// 判定模型是否可以安全透传 Todo 成功文案。
///
/// - 未声称 Todo 写入成功：直接放行。
/// - 声称成功：必须存在本轮真实成功的 Todo 写工具结果。
pub(crate) fn validate_todo_success_reply(
    reply: &str,
    tool_results: &[ToolExecutionResult],
    scope: TodoSuccessVerificationScope,
) -> TodoSuccessValidation {
    if matches!(scope, TodoSuccessVerificationScope::ExplicitMutation)
        && reply_claims_todo_detail_clear_success(reply)
    {
        return if tool_results.iter().any(successful_todo_detail_clear_result) {
            TodoSuccessValidation::Passed {
                claimed_success: true,
            }
        } else {
            TodoSuccessValidation::Blocked
        };
    }
    let claims_write_success = match scope {
        TodoSuccessVerificationScope::None => false,
        TodoSuccessVerificationScope::ExplicitMutation => reply_claims_todo_write_success(reply),
        TodoSuccessVerificationScope::ImplicitCreate => reply_claims_todo_create_success(reply),
    };
    if !claims_write_success {
        return TodoSuccessValidation::Passed {
            claimed_success: false,
        };
    }
    if has_successful_todo_write_result(tool_results) {
        TodoSuccessValidation::Passed {
            claimed_success: true,
        }
    } else {
        TodoSuccessValidation::Blocked
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoSuccessValidation {
    Passed { claimed_success: bool },
    Blocked,
}

impl TodoSuccessValidation {
    pub(crate) fn claimed_success(self) -> bool {
        matches!(
            self,
            Self::Passed {
                claimed_success: true
            } | Self::Blocked
        )
    }

    pub(crate) fn passed(self) -> bool {
        matches!(self, Self::Passed { .. })
    }
}

pub(crate) fn todo_tool_result_summaries(
    tool_results: &[ToolExecutionResult],
) -> Vec<TodoToolResultSummary> {
    tool_results
        .iter()
        .filter(|result| is_todo_tool(&result.name))
        .map(TodoToolResultSummary::from)
        .collect()
}

fn has_successful_todo_write_result(tool_results: &[ToolExecutionResult]) -> bool {
    tool_results.iter().any(successful_todo_write_result)
}

pub(crate) fn has_todo_write_tool_result(tool_results: &[ToolExecutionResult]) -> bool {
    tool_results
        .iter()
        .any(|result| is_todo_write_tool(&result.name))
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
        "delete_todos" => pending_action_matches(&result.output, "delete"),
        "edit_todo" => result.output.get("updated").is_some(),
        "merge_todos" => result.output.get("merged").is_some(),
        "complete_todos" => {
            non_empty_array_field(&result.output, "completed")
                || non_empty_array_field(&result.output, "advanced")
        }
        "restore_todos" => non_empty_array_field(&result.output, "restored"),
        "manage_recurring_reminder" => {
            non_empty_array_field(&result.output, "advanced")
                || non_empty_array_field(&result.output, "disabled")
        }
        _ => false,
    }
}

fn successful_todo_detail_clear_result(result: &ToolExecutionResult) -> bool {
    result.name == "edit_todo"
        && result.succeeded
        && !result_has_explicit_failure(&result.output)
        && result
            .output
            .get("updated")
            .and_then(|updated| updated.get("detail"))
            .is_some_and(Value::is_null)
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
pub(crate) struct TodoToolResultSummary {
    pub(crate) tool: String,
    pub(crate) succeeded: bool,
    pub(crate) error_code: Option<String>,
    pub(crate) requires_confirmation: bool,
    pub(crate) requires_clarification: bool,
    pub(crate) pending_action: Option<String>,
    pub(crate) exception: bool,
    pub(crate) skipped: bool,
    pub(crate) skip_reason: Option<String>,
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
            | "delete_todos"
            | "merge_todos"
            | "edit_todo"
            | "complete_todos"
            | "restore_todos"
            | "manage_recurring_reminder"
    )
}

pub(crate) fn is_todo_write_tool(name: &str) -> bool {
    matches!(
        name,
        "create_todo"
            | "delete_todos"
            | "merge_todos"
            | "edit_todo"
            | "complete_todos"
            | "restore_todos"
            | "manage_recurring_reminder"
    )
}

fn reply_claims_todo_write_success(reply: &str) -> bool {
    reply_claims_todo_success(reply, true)
}

fn reply_claims_todo_create_success(reply: &str) -> bool {
    let text = reply.trim();
    if text.is_empty() || explicitly_denies_todo_success(text) {
        return false;
    }
    let normalized: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    if looks_like_todo_status_or_capability_explanation(&normalized) {
        return false;
    }

    // 省略式创建请求可能与天气、搜索等结果混合，创建声明不一定在回复开头，
    // 也不一定再次出现“待办”等对象词；只要任意位置声称创建成功就必须验真。
    contains_any(&normalized, TODO_CREATE_SUCCESS_MARKERS)
}

fn reply_claims_todo_success(reply: &str, include_other_writes: bool) -> bool {
    let text = reply.trim();
    if text.is_empty() || explicitly_denies_todo_success(text) {
        return false;
    }
    let normalized: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    if include_other_writes && claims_todo_detail_clear_success(&normalized) {
        return true;
    }
    if looks_like_todo_status_or_capability_explanation(&normalized) {
        return false;
    }
    // 不读取用户输入、不推断“本轮必须调用哪个工具”；这里只从模型最终回复
    // 本身识别高风险成功文案，避免无 Tool 结果时透传“已新增/已删除”。
    if starts_with_todo_success_marker(&normalized, include_other_writes) {
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
    contains_todo_success_marker(&normalized, include_other_writes)
}

fn reply_claims_todo_detail_clear_success(reply: &str) -> bool {
    let text = reply.trim();
    if text.is_empty() || explicitly_denies_todo_success(text) {
        return false;
    }
    let normalized: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    claims_todo_detail_clear_success(&normalized)
}

fn claims_todo_detail_clear_success(text: &str) -> bool {
    let has_detail_context = contains_any(text, &["详情", "备注", "内容", "说明", "正文"]);
    let claims_clear = contains_any(
        text,
        &[
            "已清除",
            "已经清除",
            "已清空",
            "已经清空",
            "已去掉",
            "已经去掉",
            "已移除",
            "已经移除",
            "删掉了",
            "删除了",
            "不再显示",
            "不会显示",
            "不再展示",
            "不会展示",
        ],
    );
    has_detail_context && claims_clear
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
    // “已完成待办”常用于列表状态、能力说明或规则解释，
    // 不能等同于“我已经把某条待办完成”。真正的动作成功文案仍会被
    // 后续“待办 + 已完成/已删除”等组合拦截；但风险提示里的“已删除项目不可恢复”
    // 不应反向覆盖前面的缺参 / 能力说明。
    let starts_with_status = ["已完成待办", "已完成的待办", "已完成列表"]
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

fn contains_todo_success_marker(text: &str, include_other_writes: bool) -> bool {
    contains_any(text, TODO_CREATE_SUCCESS_MARKERS)
        || (include_other_writes && contains_any(text, TODO_OTHER_WRITE_SUCCESS_MARKERS))
}

fn starts_with_todo_success_marker(text: &str, include_other_writes: bool) -> bool {
    TODO_CREATE_SUCCESS_MARKERS
        .iter()
        .chain(
            include_other_writes
                .then_some(TODO_OTHER_WRITE_SUCCESS_MARKERS)
                .into_iter()
                .flatten(),
        )
        .any(|marker| text.starts_with(marker))
}

fn allowlist_marker_without_action_success(text: &str, allowlist_markers: &[&str]) -> bool {
    contains_any(text, allowlist_markers) && !contains_clear_todo_action_success_marker(text)
}

fn contains_clear_todo_action_success_marker(text: &str) -> bool {
    TODO_CREATE_SUCCESS_MARKERS
        .iter()
        .copied()
        .chain(
            [
                "已修改",
                "已更新",
                "已恢复",
                "已删除",
                "已跳过",
                "已关闭",
                "已经修改",
                "已经更新",
                "已经恢复",
                "已经删除",
                "已经跳过",
                "已经关闭",
            ]
            .iter()
            .copied(),
        )
        .any(|marker| {
            text.match_indices(marker)
                .any(|(pos, _)| !is_explanatory_clear_success_usage(text, pos, marker))
        })
}

fn is_explanatory_clear_success_usage(text: &str, pos: usize, marker: &str) -> bool {
    // “已删除项目不可恢复”是缺参/能力说明里的风险提示，不表示本轮已经删除待办。
    marker == "已删除" && text[pos..].starts_with("已删除项目不可恢复")
}

pub(crate) fn todo_success_not_verified_reply() -> String {
    "这次没有确认改动成功。请先查看最新待办列表，再按编号操作一次。".to_owned()
}

pub(crate) fn todo_success_not_verified_reply_for_tool_results(
    tool_results: &[ToolExecutionResult],
) -> String {
    let summaries = todo_tool_result_summaries(tool_results);
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
    "这次没有确认改动成功。请先查看最新待办列表，再按编号操作一次。".to_owned()
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
            "没有找到可删除的已完成待办，请先查看对应列表后再选择。".to_owned()
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
        Some("bad_tool_arguments") => "这次待办目标不完整，请换个说法说明目标。".to_owned(),
        Some(_) if summary.exception => {
            "这次没有确认改动成功。请稍后重试，或先查看最新待办列表。".to_owned()
        }
        Some(_) => "这次没有确认改动成功。请先查看最新待办列表，再试一次。".to_owned(),
        None if summary.requires_clarification => "目标不明确，请选择具体待办。".to_owned(),
        None if summary.skipped => {
            "前一步没有确认成功，本次没有继续修改待办。请先查看最新待办列表。".to_owned()
        }
        None => todo_success_not_verified_reply(),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
