//! Train Tool 的 Agent Turn 结果投影。
//!
//! 这里负责列车结果的结构校验、状态分类和可信响应块生成；通用 Agent 调度层
//! 只调用本领域门面，不再直接依赖 Train 字段或 formatter。

use qq_maid_llm::provider::ToolExecutionResult;
use serde_json::Value;

use crate::{
    error::LlmError,
    runtime::respond::{
        agent_outcome::{
            OutcomePresentation, ResponseBlock, ToolEffect, ToolExecutionOutcome, ToolOutcomeStatus,
        },
        common::CommandBody,
    },
};

use super::{
    TRAIN_TOOL_NAME, TrainSchedule, TrainStop,
    format::{format_train_error_reply, format_train_schedule_reply},
};

pub(crate) fn tool_outcome_from_result(
    result: &ToolExecutionResult,
) -> Option<ToolExecutionOutcome> {
    if result.name != TRAIN_TOOL_NAME {
        return None;
    }

    let mut status = ToolOutcomeStatus::from_tool_result(result);
    let mut error_code = structured_error_code(&result.output);
    let block = match status {
        ToolOutcomeStatus::Succeeded => match train_schedule_from_output(&result.output) {
            Some(schedule) => ResponseBlock::FactCard(format_train_schedule_reply(&schedule)),
            None => {
                // Tool Registry 的截断包装或畸形成功 JSON 不包含可验证车次；投影失败
                // 必须进入失败语义，不能让模型正文覆盖确定性错误。
                status = ToolOutcomeStatus::Failed;
                error_code = Some("provider_error".to_owned());
                ResponseBlock::Error(train_error_body(error_code.as_deref()))
            }
        },
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(train_skip_body(&result.output)),
        ToolOutcomeStatus::RequiresClarification => {
            ResponseBlock::Clarification(CommandBody::plain("请说明要查询哪个车次。"))
        }
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => {
            ResponseBlock::Error(train_error_body(error_code.as_deref()))
        }
    };

    Some(ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "train".to_owned(),
        status,
        effect: ToolEffect::ReadOnly,
        presentation: OutcomePresentation::Trusted,
        blocks: vec![block],
        error_code,
        command: Some("train".to_owned()),
    })
}

fn train_schedule_from_output(output: &Value) -> Option<TrainSchedule> {
    let travel_date =
        chrono::NaiveDate::parse_from_str(&string_field(output, "travel_date")?, "%Y-%m-%d")
            .ok()?;
    let stops = output
        .get("stops")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(train_stop_from_output)
        .collect::<Vec<_>>();
    if stops.is_empty() {
        return None;
    }
    Some(TrainSchedule {
        train_code: string_field(output, "train_code")?,
        travel_date,
        start_station: string_field(output, "start_station")?,
        end_station: string_field(output, "end_station")?,
        stops,
        full_train_code: string_field(output, "full_train_code"),
        corporation: string_field(output, "corporation"),
        train_style: string_field(output, "train_style"),
        dept_train: string_field(output, "dept_train"),
    })
}

fn train_stop_from_output(output: &Value) -> Option<TrainStop> {
    Some(TrainStop {
        station_no: output
            .get("station_no")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())?,
        station_name: string_field(output, "station_name")?,
        arrive_time: optional_string_field(output, "arrive_time"),
        departure_time: optional_string_field(output, "departure_time"),
        stopover_minutes: output
            .get("stopover_minutes")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        day_difference: output
            .get("day_difference")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(0),
        day_difference_reliable: output
            .get("day_difference_reliable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        station_train_code: string_field(output, "station_train_code")
            .or_else(|| string_field(output, "train_code"))
            .unwrap_or_default(),
    })
}

fn train_error_body(error_code: Option<&str>) -> CommandBody {
    let code = error_code.unwrap_or("provider_error");
    if code == "bad_tool_arguments" {
        return CommandBody::plain(
            "【火车】\n\n火车查询参数不完整，请提供车次；日期支持今天、明天、后天或 YYYY-MM-DD。",
        );
    }
    let err = LlmError::new(code, "train tool failed", "train");
    CommandBody::plain(format_train_error_reply(&err))
}

fn train_skip_body(output: &Value) -> CommandBody {
    let text = match string_field(output, "reason").as_deref() {
        Some("dependency_previous_call_failed") => {
            "火车查询因前序工具失败已跳过；根因以上方失败信息为准。".to_owned()
        }
        Some(reason) => format!("火车查询已跳过：{reason}。"),
        None => "火车查询已跳过。".to_owned(),
    };
    CommandBody::plain(text)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn optional_string_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::Null) | None => None,
        Some(_) => string_field(value, key),
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn truncated_train_success_is_projected_as_failure() {
        let result = ToolExecutionResult {
            name: TRAIN_TOOL_NAME.to_owned(),
            output: json!({
                "truncated": true,
                "original_chars": 12000,
                "preview": "{\"ok\":true,\"train_code\":\"G1\""
            }),
            succeeded: true,
        };

        let outcome = tool_outcome_from_result(&result).expect("train result is consumed");

        assert_eq!(outcome.status, ToolOutcomeStatus::Failed);
        assert_eq!(outcome.error_code.as_deref(), Some("provider_error"));
        assert!(matches!(outcome.blocks[0], ResponseBlock::Error(_)));
    }
}
