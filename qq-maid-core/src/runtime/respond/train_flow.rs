//! 列车时刻查询指令处理流程。
//!
//! 该 flow 目前只承载 `/火车` 查询能力，不涉及 Todo 集成。
//! 车次与日期均在本地完成解析，再通过 `runtime::train` 执行器查询真实时刻表。

use chrono::{Datelike, Duration, NaiveDate};
use serde_json::json;

use crate::{
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        session::SessionRecord,
        train::{TrainSchedule, TrainScheduleRequest, TrainStop},
    },
    util::time_context::{RequestTimeContext, request_time_context},
};

use super::{
    RespondResponse, RustRespondService,
    command_render::escape_markdown_inline,
    common::{command_response, session_error},
};

/// 查询参数最长字符数，限制异常输入。
const TRAIN_ARGUMENT_MAX_CHARS: usize = 80;
/// 参数超长提示。
const TRAIN_ARGUMENT_TOO_LONG_REPLY: &str = "火车查询参数太长了，请压缩到 80 字以内再试。";
/// 车次缺失提示。
const TRAIN_CODE_REQUIRED_REPLY: &str = "请先提供车次，例如：/火车 G1";
/// 日期格式无法识别提示。
const TRAIN_DATE_INVALID_REPLY: &str =
    "日期暂时只支持 今天、明天、后天、YYYY-MM-DD、YYYY年M月D日 或 M月D日。";
/// 12306 无开行数据提示。
const TRAIN_NO_SCHEDULE_REPLY: &str = "该日期未查询到开行信息。";
/// 12306 超时提示。
const TRAIN_TIMEOUT_REPLY: &str = "【火车】铁路时刻服务超时了，请稍后再试。";
/// 12306 上游异常提示。
const TRAIN_UPSTREAM_ERROR_REPLY: &str =
    "【火车】铁路时刻服务暂时不可用，可能是上游接口、代理或网络配置异常。请稍后再试。";
/// 响应字段异常提示。
const TRAIN_RESPONSE_INVALID_REPLY: &str =
    "【火车】铁路时刻服务返回了不完整数据，本次无法整理时刻表。请稍后再试。";

/// 已解析的 `/火车` 指令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedTrainCommand {
    /// 固定动作名。
    pub action: String,
    /// 用户输入中的原始命令名。
    pub raw_command: String,
    /// 规范化后的车次。
    pub train_code: String,
    /// 查询日期。
    pub travel_date: NaiveDate,
    /// 用户是否显式提供日期。
    pub date_provided: bool,
    /// 解析失败原因；只要命中了 `/火车`，就不应静默落回普通聊天。
    pub parse_error: Option<TrainCommandParseError>,
}

/// `/火车` 参数解析失败原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrainCommandParseError {
    /// 未提供车次。
    MissingCode,
    /// 参数过长。
    ArgumentTooLong,
    /// 日期格式无法识别。
    InvalidDate,
}

/// 从用户文本解析 `/火车` 指令。
pub(super) fn parse_train_command(text: &str) -> Option<ParsedTrainCommand> {
    let command = parse_slash_command(text)?;
    if command.action != "train" {
        return None;
    }
    parse_train_command_from_parts(command, &request_time_context())
}

impl RustRespondService {
    /// 处理 `/火车` 指令。
    pub(super) async fn handle_train_command(
        &self,
        command: ParsedTrainCommand,
        user_text: &str,
        session: &mut SessionRecord,
    ) -> Result<RespondResponse, LlmError> {
        if let Some(reply) = parse_error_reply(command.parse_error) {
            return Ok(command_response(
                reply,
                Some(session.session_id.clone()),
                Some(command.action),
            ));
        }

        let outcome = match self
            .train_executor
            .query_train_schedule(TrainScheduleRequest {
                train_code: command.train_code.clone(),
                travel_date: command.travel_date,
            })
            .await
        {
            Ok(schedule) => schedule,
            Err(err) => {
                tracing::warn!(
                    error_code = %err.code,
                    error_stage = %err.stage,
                    train_provider = self.train_executor.provider_name(),
                    train_code = %command.train_code,
                    travel_date = %command.travel_date,
                    "train command failed"
                );
                let reply = format_train_error_reply(&err);
                self.session_store
                    .append_exchange(session, user_text, &reply)
                    .map_err(session_error)?;

                let mut response = command_response(
                    reply,
                    Some(session.session_id.clone()),
                    Some(command.action),
                );
                response.diagnostics = Some(json!({
                    "backend": "rust",
                    "session_backend": "rust",
                    "used_memory": false,
                    "used_search": false,
                    "used_train": true,
                    "train_provider": self.train_executor.provider_name(),
                    "train_code": command.train_code,
                    "travel_date": command.travel_date.to_string(),
                    "date_provided": command.date_provided,
                    "train_error_code": err.code,
                    "train_error_stage": err.stage,
                }));
                return Ok(response);
            }
        };

        let reply = format_train_schedule_reply(&outcome);
        self.session_store
            .append_exchange(
                session,
                user_text,
                &reply.markdown.clone().unwrap_or_default(),
            )
            .map_err(session_error)?;

        let mut response = command_response(
            reply,
            Some(session.session_id.clone()),
            Some(command.action),
        );
        response.diagnostics = Some(json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": false,
            "used_search": false,
            "used_train": true,
            "train_provider": self.train_executor.provider_name(),
            "train_code": outcome.train_code,
            "travel_date": outcome.travel_date.to_string(),
            "start_station": outcome.start_station,
            "end_station": outcome.end_station,
            "stop_count": outcome.stops.len(),
            "date_provided": command.date_provided,
        }));
        Ok(response)
    }
}

fn parse_train_command_from_parts(
    command: ParsedCommand,
    ctx: &RequestTimeContext,
) -> Option<ParsedTrainCommand> {
    let argument = command.argument.trim();
    if argument.is_empty() {
        return Some(ParsedTrainCommand {
            action: command.action,
            raw_command: command.raw_command,
            train_code: String::new(),
            travel_date: ctx.local_date(),
            date_provided: false,
            parse_error: Some(TrainCommandParseError::MissingCode),
        });
    }
    if argument.chars().count() > TRAIN_ARGUMENT_MAX_CHARS {
        return Some(ParsedTrainCommand {
            action: command.action,
            raw_command: command.raw_command,
            train_code: String::new(),
            travel_date: ctx.local_date(),
            date_provided: false,
            parse_error: Some(TrainCommandParseError::ArgumentTooLong),
        });
    }

    let mut parts = argument.split_whitespace();
    let train_code = parts.next()?.trim().to_ascii_uppercase();
    let date_text = parts.collect::<Vec<_>>().join(" ");
    let date_text = date_text.trim();
    let (travel_date, date_provided) = if date_text.is_empty() {
        (ctx.local_date(), false)
    } else {
        match parse_train_date(date_text, ctx) {
            Some(date) => (date, true),
            None => {
                return Some(ParsedTrainCommand {
                    action: command.action,
                    raw_command: command.raw_command,
                    train_code,
                    travel_date: ctx.local_date(),
                    date_provided: true,
                    parse_error: Some(TrainCommandParseError::InvalidDate),
                });
            }
        }
    };
    Some(ParsedTrainCommand {
        action: command.action,
        raw_command: command.raw_command,
        train_code,
        travel_date,
        date_provided,
        parse_error: None,
    })
}

fn parse_train_date(text: &str, ctx: &RequestTimeContext) -> Option<NaiveDate> {
    let trimmed = text.trim();
    match trimmed {
        "今天" => Some(ctx.local_date()),
        "明天" => Some(ctx.local_date() + Duration::days(1)),
        "后天" => Some(ctx.local_date() + Duration::days(2)),
        _ => parse_explicit_train_date(trimmed, ctx.local_date()),
    }
}

fn parse_explicit_train_date(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Some(date);
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y年%m月%d日") {
        return Some(date);
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y年%-m月%-d日") {
        return Some(date);
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y年%m月%d号") {
        return Some(date);
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y年%-m月%-d号") {
        return Some(date);
    }
    parse_month_day_train_date(text, today)
}

fn parse_month_day_train_date(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let normalized = text.trim().replace('号', "日");
    let (month_text, day_text) = normalized.split_once('月')?;
    let month = month_text
        .strip_suffix('年')
        .unwrap_or(month_text)
        .parse::<u32>()
        .ok()?;
    let day = day_text.strip_suffix('日')?.parse::<u32>().ok()?;
    let mut year = today.year();
    let mut date = NaiveDate::from_ymd_opt(year, month, day)?;
    if date < today {
        year += 1;
        date = NaiveDate::from_ymd_opt(year, month, day)?;
    }
    Some(date)
}

fn format_train_error_reply(err: &LlmError) -> String {
    match err.code.as_str() {
        "no_schedule" => TRAIN_NO_SCHEDULE_REPLY.to_owned(),
        "timeout" => TRAIN_TIMEOUT_REPLY.to_owned(),
        "provider_error" if err.stage == "train_json" => TRAIN_RESPONSE_INVALID_REPLY.to_owned(),
        _ => TRAIN_UPSTREAM_ERROR_REPLY.to_owned(),
    }
}

fn parse_error_reply(error: Option<TrainCommandParseError>) -> Option<&'static str> {
    match error? {
        TrainCommandParseError::MissingCode => Some(TRAIN_CODE_REQUIRED_REPLY),
        TrainCommandParseError::ArgumentTooLong => Some(TRAIN_ARGUMENT_TOO_LONG_REPLY),
        TrainCommandParseError::InvalidDate => Some(TRAIN_DATE_INVALID_REPLY),
    }
}

pub(super) fn format_train_schedule_reply(schedule: &TrainSchedule) -> super::common::CommandBody {
    let mut text_rows = vec![
        format!("🚄 {} 列车时刻", schedule.train_code),
        String::new(),
        format!("日期：{}", schedule.travel_date),
        format!(
            "行程：{} → {}",
            schedule.start_station, schedule.end_station
        ),
    ];
    let mut markdown_rows = vec![
        format!(
            "# 🚄 {} 列车时刻",
            escape_markdown_inline(&schedule.train_code)
        ),
        String::new(),
        format!("**日期：** {}", schedule.travel_date),
        format!(
            "**行程：** {} → {}",
            escape_markdown_inline(&schedule.start_station),
            escape_markdown_inline(&schedule.end_station)
        ),
    ];
    // 以下 4 个字段来自 12306 可选属性，缺失或为空时省略对应行，
    // 只展示接口真实返回的数据，不推测、不补造。
    push_optional_info_row(
        &mut text_rows,
        &mut markdown_rows,
        "完整车次",
        &schedule.full_train_code,
    );
    push_optional_info_row(
        &mut text_rows,
        &mut markdown_rows,
        "担当客运段",
        &schedule.corporation,
    );
    push_optional_info_row(
        &mut text_rows,
        &mut markdown_rows,
        "车型信息",
        &schedule.train_style,
    );
    push_optional_info_row(
        &mut text_rows,
        &mut markdown_rows,
        "配属",
        &schedule.dept_train,
    );

    text_rows.push(String::new());
    text_rows.push("站序 / 车站 / 到达 / 出发 / 停留".to_owned());
    markdown_rows.push(String::new());
    markdown_rows.push("| 站序 | 车站 | 到达 | 出发 | 停留 |".to_owned());
    markdown_rows.push("| ---: | --- | ---: | ---: | ---: |".to_owned());

    let stop_count = schedule.stops.len();
    for (index, stop) in schedule.stops.iter().enumerate() {
        // 始发站 / 终到站 / 中间站 / 单站异常数据分别按位置渲染到发和停留三列，
        // 避免始发站也显示到达时间、终到站也显示出发时间造成误解。
        let (arrive, departure, stopover) = format_stop_columns(stop, index, stop_count);
        let station_name = format_station_name(stop);
        markdown_rows.push(format!(
            "| {} | {} | {} | {} | {} |",
            stop.station_no,
            escape_markdown_inline(&station_name),
            arrive,
            departure,
            stopover
        ));
        text_rows.push(format!(
            "{} / {} / {} / {} / {}",
            stop.station_no, station_name, arrive, departure, stopover
        ));
    }
    text_rows.push(String::new());
    text_rows.push(TRAIN_SCHEDULE_FOOTER_REPLY.to_owned());
    markdown_rows.push(String::new());
    markdown_rows.push(format!("> {}", TRAIN_SCHEDULE_FOOTER_REPLY));
    super::common::CommandBody::dual(text_rows.join("\n"), markdown_rows.join("\n"))
}

/// 将 12306 可选字段追加到纯文本和 Markdown 输出中。
///
/// 字段为 `None` 时直接跳过，不输出对应行；非空时纯文本侧为 `标签：值`，
/// Markdown 侧为 `**标签：** 值`，并对值做行内 Markdown 转义。
fn push_optional_info_row(
    text_rows: &mut Vec<String>,
    markdown_rows: &mut Vec<String>,
    label: &str,
    value: &Option<String>,
) {
    let Some(value) = value else {
        return;
    };
    text_rows.push(format!("{label}：{value}"));
    markdown_rows.push(format!("**{label}：** {}", escape_markdown_inline(value)));
}

/// 时刻表底部提示，强调当日计划时刻与实时信息的差异。
const TRAIN_SCHEDULE_FOOTER_REPLY: &str =
    "当前展示为当日计划时刻，不含实时正晚点、余票及临时停运信息，请以铁路12306或车站公告为准。";

/// 根据经停站在时刻表中的位置渲染到达、出发和停留三列。
///
/// - 始发站（第一站）：到达显示 `--`，出发取实际发车时间，停留显示 `始发`；
/// - 终到站（最后一站）：到达取实际到达时间，出发显示 `--`，停留显示 `终到`；
/// - 中间站：保持原来到发时间和停留分钟数逻辑；
/// - 仅一站的异常数据：不同时硬标为始发和终到，保留原始到发数据，停留显示 `--`。
fn format_stop_columns(
    stop: &TrainStop,
    index: usize,
    stop_count: usize,
) -> (String, String, String) {
    let arrive = stop.arrive_time.as_deref().unwrap_or("--");
    let departure = stop.departure_time.as_deref().unwrap_or("--");
    if stop_count <= 1 {
        // 只有一站的异常数据，保留原始到发，停留用 -- 占位，避免同时硬标始发/终到。
        return (arrive.to_owned(), departure.to_owned(), "--".to_owned());
    }
    if index == 0 {
        // 始发站：到达无意义，统一显示 --；停留固定为始发。
        return ("--".to_owned(), departure.to_owned(), "始发".to_owned());
    }
    if index == stop_count - 1 {
        // 终到站：出发无意义，统一显示 --；停留固定为终到。
        return (arrive.to_owned(), "--".to_owned(), "终到".to_owned());
    }
    // 中间站保持原有停留分钟数逻辑。
    let stopover = format_stopover(stop);
    (arrive.to_owned(), departure.to_owned(), stopover)
}

fn format_station_name(stop: &TrainStop) -> String {
    if stop.day_difference <= 0 {
        return stop.station_name.clone();
    }
    format!("{}（+{}天）", stop.station_name, stop.day_difference)
}

fn format_stopover(stop: &TrainStop) -> String {
    match stop.stopover_minutes {
        Some(0) if stop.arrive_time.is_some() && stop.departure_time.is_some() => {
            "0 分钟".to_owned()
        }
        Some(0) | None => "--".to_owned(),
        Some(minutes) => format!("{minutes} 分钟"),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, TimeZone};

    use super::*;

    #[test]
    fn parse_train_command_defaults_to_today() {
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        let ctx = RequestTimeContext::from_datetime(
            offset.with_ymd_and_hms(2026, 6, 23, 9, 0, 0).unwrap(),
        );
        let command = ParsedCommand {
            action: "train".to_owned(),
            argument: "G1".to_owned(),
            raw_command: "火车".to_owned(),
        };

        let parsed = parse_train_command_from_parts(command, &ctx).unwrap();
        assert_eq!(parsed.train_code, "G1");
        assert_eq!(
            parsed.travel_date,
            NaiveDate::from_ymd_opt(2026, 6, 23).unwrap()
        );
        assert!(!parsed.date_provided);
        assert_eq!(parsed.parse_error, None);
    }

    #[test]
    fn parse_train_command_supports_relative_and_iso_date() {
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        let ctx = RequestTimeContext::from_datetime(
            offset.with_ymd_and_hms(2026, 6, 23, 9, 0, 0).unwrap(),
        );

        let relative = parse_train_command_from_parts(
            ParsedCommand {
                action: "train".to_owned(),
                argument: "d1234 明天".to_owned(),
                raw_command: "火车".to_owned(),
            },
            &ctx,
        )
        .unwrap();
        assert_eq!(relative.train_code, "D1234");
        assert_eq!(
            relative.travel_date,
            NaiveDate::from_ymd_opt(2026, 6, 24).unwrap()
        );
        assert!(relative.date_provided);
        assert_eq!(relative.parse_error, None);

        let iso = parse_train_command_from_parts(
            ParsedCommand {
                action: "train".to_owned(),
                argument: "1461 2026-06-28".to_owned(),
                raw_command: "火车".to_owned(),
            },
            &ctx,
        )
        .unwrap();
        assert_eq!(iso.train_code, "1461");
        assert_eq!(
            iso.travel_date,
            NaiveDate::from_ymd_opt(2026, 6, 28).unwrap()
        );
    }

    #[test]
    fn parse_train_command_marks_invalid_date() {
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        let ctx = RequestTimeContext::from_datetime(
            offset.with_ymd_and_hms(2026, 6, 23, 9, 0, 0).unwrap(),
        );

        let parsed = parse_train_command_from_parts(
            ParsedCommand {
                action: "train".to_owned(),
                argument: "G1 下周一".to_owned(),
                raw_command: "火车".to_owned(),
            },
            &ctx,
        )
        .unwrap();
        assert_eq!(
            parsed.parse_error,
            Some(TrainCommandParseError::InvalidDate)
        );
    }

    #[test]
    fn parse_train_date_supports_month_day_rollover() {
        let today = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        let parsed = parse_month_day_train_date("1月2日", today).unwrap();
        assert_eq!(parsed, NaiveDate::from_ymd_opt(2027, 1, 2).unwrap());
    }
}
