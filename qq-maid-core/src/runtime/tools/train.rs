//! 火车时刻 Tool。
//!
//! 该 Tool 复用现有 `TrainExecutor`，只做模型工具参数校验和结果结构化整理。
//! slash `/火车` 命令仍保留在 respond/train_flow.rs，不通过 Tool Loop。

use async_trait::async_trait;
use chrono::{Duration, NaiveDate};
use qq_maid_common::time_context::request_time_context;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::train::{DynTrainExecutor, TrainSchedule, TrainScheduleRequest, TrainStop},
};

const TRAIN_TOOL_NAME: &str = "get_train_schedule";
const TRAIN_TOOL_CODE_MAX_CHARS: usize = 20;

pub(crate) mod route {
    //! 列车普通消息 Agent Chat 路由判断。

    pub(crate) fn has_train_intent(text: &str, _lower: &str) -> bool {
        contains_any(
            text,
            &["火车", "列车", "车次", "高铁", "动车", "时刻", "站台"],
        ) || has_train_code(text)
    }

    fn has_train_code(text: &str) -> bool {
        let chars = text.chars().collect::<Vec<_>>();
        for start in 0..chars.len() {
            let ch = chars[start];
            if !matches!(
                ch,
                'G' | 'D' | 'C' | 'K' | 'Z' | 'T' | 'g' | 'd' | 'c' | 'k' | 'z' | 't'
            ) || !is_train_code_boundary(chars.get(start.wrapping_sub(1)).copied())
            {
                continue;
            }

            let mut end = start + 1;
            while end < chars.len() && chars[end].is_ascii_digit() && end - start <= 5 {
                end += 1;
            }
            let digit_count = end - start - 1;
            // 单数字车次在技术语境中误伤很高，当前只保留常见的 G1 这类高铁短码。
            let allow_single_digit = matches!(ch, 'G' | 'g');
            let valid_digit_count =
                (2..=5).contains(&digit_count) || digit_count == 1 && allow_single_digit;
            if valid_digit_count && is_train_code_boundary(chars.get(end).copied()) {
                return true;
            }
        }
        false
    }

    fn is_train_code_boundary(ch: Option<char>) -> bool {
        match ch {
            None => true,
            Some(ch) => ch.is_whitespace() || ch.is_ascii_punctuation() || is_cjk_punctuation(ch),
        }
    }

    fn is_cjk_punctuation(ch: char) -> bool {
        matches!(
            ch,
            '，' | '。'
                | '、'
                | '：'
                | '；'
                | '？'
                | '！'
                | '（'
                | '）'
                | '【'
                | '】'
                | '《'
                | '》'
                | '“'
                | '”'
                | '‘'
                | '’'
        )
    }

    fn contains_any(text: &str, needles: &[&str]) -> bool {
        needles.iter().any(|needle| text.contains(needle))
    }
}

/// 模型可调用的列车时刻查询 Tool。
#[derive(Clone)]
pub struct TrainScheduleTool {
    executor: DynTrainExecutor,
}

impl TrainScheduleTool {
    pub fn new(executor: DynTrainExecutor) -> Self {
        Self { executor }
    }
}

#[async_trait]
impl Tool for TrainScheduleTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: TRAIN_TOOL_NAME.to_owned(),
            description: "查询中国铁路列车计划时刻表。用于回答车次经停站、始发终到、到达/出发时间、停留时间等问题；不查询余票、票价、实时正晚点或停运公告。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "train_code": {
                        "type": "string",
                        "description": "车次，例如 G1、D1234、1461"
                    },
                    "travel_date": {
                        "type": ["string", "null"],
                        "description": "查询日期。支持 YYYY-MM-DD、今天、明天、后天；不确定时传 null，系统默认今天"
                    }
                },
                "required": ["train_code", "travel_date"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let train_code = parse_train_code(&arguments)?;
        let travel_date = parse_travel_date(arguments.get("travel_date"))?;
        let schedule = self
            .executor
            .query_train_schedule(TrainScheduleRequest {
                train_code,
                travel_date,
            })
            .await?;
        Ok(ToolOutput::json(train_schedule_tool_output(
            &schedule,
            self.executor.provider_name(),
        )))
    }
}

fn parse_train_code(arguments: &Value) -> Result<String, LlmError> {
    let train_code = arguments
        .get("train_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            LlmError::new(
                "bad_tool_arguments",
                "get_train_schedule requires non-empty train_code",
                "tool",
            )
        })?;
    if train_code.chars().count() > TRAIN_TOOL_CODE_MAX_CHARS {
        return Err(LlmError::new(
            "bad_tool_arguments",
            "train_code is too long",
            "tool",
        ));
    }
    Ok(train_code.to_ascii_uppercase())
}

fn parse_travel_date(value: Option<&Value>) -> Result<NaiveDate, LlmError> {
    let ctx = request_time_context();
    let Some(value) = value else {
        return Ok(ctx.local_date());
    };
    if value.is_null() {
        return Ok(ctx.local_date());
    }
    let Some(text) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return reject_invalid_travel_date();
    };
    match text {
        "今天" => Ok(ctx.local_date()),
        "明天" => Ok(ctx.local_date() + Duration::days(1)),
        "后天" => Ok(ctx.local_date() + Duration::days(2)),
        _ => NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| {
            tracing::warn!(
                tool = TRAIN_TOOL_NAME,
                error_code = "bad_tool_arguments",
                argument = "travel_date",
                "invalid train travel_date argument rejected",
            );
            LlmError::new(
                "bad_tool_arguments",
                "travel_date must be YYYY-MM-DD, 今天, 明天, 后天, or null",
                "tool",
            )
        }),
    }
}

fn reject_invalid_travel_date() -> Result<NaiveDate, LlmError> {
    tracing::warn!(
        tool = TRAIN_TOOL_NAME,
        error_code = "bad_tool_arguments",
        argument = "travel_date",
        "invalid train travel_date argument rejected",
    );
    Err(LlmError::new(
        "bad_tool_arguments",
        "travel_date must be a string or null",
        "tool",
    ))
}

fn train_schedule_tool_output(schedule: &TrainSchedule, provider: &str) -> Value {
    json!({
        "provider": provider,
        "train_code": schedule.train_code,
        "travel_date": schedule.travel_date.to_string(),
        "start_station": schedule.start_station,
        "end_station": schedule.end_station,
        "full_train_code": schedule.full_train_code,
        "corporation": schedule.corporation,
        "train_style": schedule.train_style,
        "dept_train": schedule.dept_train,
        "stops": schedule.stops.iter().map(train_stop_json).collect::<Vec<_>>(),
        "notice": "计划时刻，不含实时正晚点、余票及临时停运信息",
    })
}

fn train_stop_json(stop: &TrainStop) -> Value {
    json!({
        "station_no": stop.station_no,
        "station_name": stop.station_name,
        "arrive_time": stop.arrive_time,
        "departure_time": stop.departure_time,
        "stopover_minutes": stop.stopover_minutes,
        "day_difference": stop.day_difference,
        "day_difference_reliable": stop.day_difference_reliable,
        "station_train_code": stop.station_train_code,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use crate::runtime::train::TrainExecutor;

    use super::*;

    fn test_context() -> ToolContext {
        ToolContext {
            task_id: "msg-1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            group_member_role: None,
            tool_call_id: Some("call-1".to_owned()),
        }
    }

    #[derive(Clone, Default)]
    struct MockTrainExecutor {
        requests: Arc<Mutex<Vec<TrainScheduleRequest>>>,
    }

    #[async_trait]
    impl TrainExecutor for MockTrainExecutor {
        async fn query_train_schedule(
            &self,
            req: TrainScheduleRequest,
        ) -> Result<TrainSchedule, LlmError> {
            self.requests.lock().unwrap().push(req.clone());
            Ok(TrainSchedule {
                train_code: req.train_code.clone(),
                travel_date: req.travel_date,
                start_station: "北京南".to_owned(),
                end_station: "上海虹桥".to_owned(),
                stops: vec![
                    TrainStop {
                        station_no: 1,
                        station_name: "北京南".to_owned(),
                        arrive_time: None,
                        departure_time: Some("06:30".to_owned()),
                        stopover_minutes: None,
                        day_difference: 0,
                        day_difference_reliable: true,
                        station_train_code: req.train_code.clone(),
                    },
                    TrainStop {
                        station_no: 2,
                        station_name: "上海虹桥".to_owned(),
                        arrive_time: Some("11:24".to_owned()),
                        departure_time: None,
                        stopover_minutes: None,
                        day_difference: 0,
                        day_difference_reliable: true,
                        station_train_code: req.train_code.clone(),
                    },
                ],
                full_train_code: None,
                corporation: None,
                train_style: None,
                dept_train: None,
            })
        }

        fn provider_name(&self) -> &'static str {
            "mock-train"
        }
    }

    #[tokio::test]
    async fn train_tool_reuses_train_executor() {
        let executor = MockTrainExecutor::default();
        let requests = executor.requests.clone();
        let tool = TrainScheduleTool::new(Arc::new(executor));

        let output = tool
            .execute(
                test_context(),
                json!({"train_code": "g1", "travel_date": "2026-06-28"}),
            )
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].train_code, "G1");
        assert_eq!(
            requests[0].travel_date,
            NaiveDate::from_ymd_opt(2026, 6, 28).unwrap()
        );
        assert_eq!(output.value["provider"], "mock-train");
        assert_eq!(output.value["start_station"], "北京南");
        assert_eq!(output.value["stops"][0]["departure_time"], "06:30");
    }

    #[tokio::test]
    async fn train_tool_rejects_empty_train_code_without_calling_executor() {
        let executor = MockTrainExecutor::default();
        let requests = executor.requests.clone();
        let tool = TrainScheduleTool::new(Arc::new(executor));

        let err = tool
            .execute(
                test_context(),
                json!({"train_code": " ", "travel_date": null}),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "bad_tool_arguments");
        assert_eq!(requests.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn train_tool_rejects_invalid_travel_date_without_calling_executor() {
        let executor = MockTrainExecutor::default();
        let requests = executor.requests.clone();
        let tool = TrainScheduleTool::new(Arc::new(executor));

        let err = tool
            .execute(
                test_context(),
                json!({"train_code": "G1", "travel_date": "下周一"}),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "bad_tool_arguments");
        assert_eq!(requests.lock().unwrap().len(), 0);
    }
}
