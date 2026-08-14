//! 列车时刻 Tool 与火车命令共用的可信展示格式。
//!
//! 错误分类和时刻表排版都属于 Train 领域；respond flow 只负责命令解析与调用，
//! 通用 Agent 投影也通过本模块消费同一套结果展示契约。

use qq_maid_common::markdown::escape_inline;

use crate::{error::LlmError, runtime::respond::common::CommandBody};

use super::{TrainSchedule, TrainStop};

const TRAIN_NO_SCHEDULE_REPLY: &str = "该日期未查询到开行信息。";
const TRAIN_TIMEOUT_REPLY: &str = "【火车】铁路时刻服务超时了，请稍后再试。";
const TRAIN_UPSTREAM_ERROR_REPLY: &str =
    "【火车】铁路时刻服务暂时不可用，可能是上游接口、代理或网络配置异常。请稍后再试。";
const TRAIN_RESPONSE_INVALID_REPLY: &str =
    "【火车】铁路时刻服务返回了不完整数据，本次无法整理时刻表。请稍后再试。";
const TRAIN_SCHEDULE_FOOTER_REPLY: &str =
    "当前展示为当日计划时刻，不含实时正晚点、余票及临时停运信息，请以铁路12306或车站公告为准。";

pub(crate) fn format_train_error_reply(err: &LlmError) -> String {
    match err.code.as_str() {
        "no_schedule" => TRAIN_NO_SCHEDULE_REPLY.to_owned(),
        "timeout" => TRAIN_TIMEOUT_REPLY.to_owned(),
        "provider_error" if err.stage == "train_json" => TRAIN_RESPONSE_INVALID_REPLY.to_owned(),
        _ => TRAIN_UPSTREAM_ERROR_REPLY.to_owned(),
    }
}

pub(crate) fn format_train_schedule_reply(schedule: &TrainSchedule) -> CommandBody {
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
        format!("# 🚄 {} 列车时刻", escape_inline(&schedule.train_code)),
        String::new(),
        format!("**日期：** {}", schedule.travel_date),
        format!(
            "**行程：** {} → {}",
            escape_inline(&schedule.start_station),
            escape_inline(&schedule.end_station)
        ),
    ];
    // 可选字段缺失时省略，严格只展示 12306 真实返回的数据，不推测、不补造。
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
        // 始发、终到、中间站和单站异常数据分别处理，避免虚构无意义的到发时间。
        let (arrive, departure, stopover) = format_stop_columns(stop, index, stop_count);
        let station_name = format_station_name(stop);
        markdown_rows.push(format!(
            "| {} | {} | {} | {} | {} |",
            stop.station_no,
            escape_inline(&station_name),
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
    CommandBody::dual(text_rows.join("\n"), markdown_rows.join("\n"))
}

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
    markdown_rows.push(format!("**{label}：** {}", escape_inline(value)));
}

fn format_stop_columns(
    stop: &TrainStop,
    index: usize,
    stop_count: usize,
) -> (String, String, String) {
    let arrive = stop.arrive_time.as_deref().unwrap_or("--");
    let departure = stop.departure_time.as_deref().unwrap_or("--");
    if stop_count <= 1 {
        return (arrive.to_owned(), departure.to_owned(), "--".to_owned());
    }
    if index == 0 {
        return ("--".to_owned(), departure.to_owned(), "始发".to_owned());
    }
    if index == stop_count - 1 {
        return (arrive.to_owned(), "--".to_owned(), "终到".to_owned());
    }
    (
        arrive.to_owned(),
        departure.to_owned(),
        format_stopover(stop),
    )
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
