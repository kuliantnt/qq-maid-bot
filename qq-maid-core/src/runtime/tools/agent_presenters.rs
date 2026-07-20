//! 非 Todo 业务 Tool 的确定性展示适配器。
//!
//! 通用 Agent 编排层不理解具体业务字段；这里按工具名把已注册业务 Tool 的
//! 安全结构化输出转换为可信响应块，避免模型最终文案覆盖或丢弃真实工具结果。

use chrono::NaiveDate;
use qq_maid_common::time_context::{format_local_time_for_display, format_rss_time_for_display};
use qq_maid_llm::provider::ToolExecutionResult;
use serde_json::Value;

use crate::{
    error::LlmError,
    runtime::{
        respond::{
            agent_outcome::{
                OutcomePresentation, ResponseBlock, ToolEffect, ToolExecutionOutcome,
                ToolOutcomeStatus,
            },
            common::{CommandBody, structured_command_body, truncate_chars},
            search_flow::{
                format_web_search_error_reply, format_web_search_research_error_reply,
                format_web_search_tool_reply,
            },
            train_flow::{format_train_error_reply, format_train_schedule_reply},
            weather_flow::{format_forecast_day_label, weather_code_label},
        },
        tools::train::{TrainSchedule, TrainStop},
    },
};

const RSS_TOOL_NAME: &str = "get_rss_recent_items";
const RSS_MANAGE_TOOL_NAME: &str = "manage_rss_subscriptions";
const TRAIN_TOOL_NAME: &str = "get_train_schedule";
const WEATHER_TOOL_NAME: &str = "get_weather";
const WEB_SEARCH_TOOL_NAME: &str = "web_search";
const KNOWLEDGE_SEARCH_TOOL_NAME: &str = "knowledge_search";
const RSS_FACT_MAX_CHARS: usize = 1200;
const WEATHER_FACT_MAX_CHARS: usize = 900;

pub(crate) fn tool_outcome_from_rss_result(
    result: &ToolExecutionResult,
) -> Option<ToolExecutionOutcome> {
    if result.name == RSS_MANAGE_TOOL_NAME {
        return Some(rss_manage_outcome(result));
    }
    if result.name != RSS_TOOL_NAME {
        return None;
    }

    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = structured_error_code(&result.output);
    let block = match status {
        ToolOutcomeStatus::Succeeded => ResponseBlock::FactCard(rss_fact_card(&result.output)),
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(rss_skip_body(&result.output)),
        ToolOutcomeStatus::RequiresClarification => {
            ResponseBlock::Clarification(CommandBody::plain("请说明要查看哪个 RSS 订阅或关键词。"))
        }
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => {
            ResponseBlock::Error(rss_error_body(&result.output))
        }
    };

    Some(ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "rss".to_owned(),
        status,
        effect: ToolEffect::ReadOnly,
        presentation: OutcomePresentation::Trusted,
        blocks: vec![block],
        error_code,
        command: Some("rss".to_owned()),
    })
}

fn rss_manage_outcome(result: &ToolExecutionResult) -> ToolExecutionOutcome {
    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = structured_error_code(&result.output);
    let block = match status {
        ToolOutcomeStatus::Succeeded => {
            ResponseBlock::MutationReceipt(rss_manage_body(&result.output))
        }
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(rss_skip_body(&result.output)),
        ToolOutcomeStatus::RequiresClarification => {
            ResponseBlock::Clarification(CommandBody::plain("请说明要新增或删除哪些 RSS 订阅。"))
        }
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => {
            ResponseBlock::Error(rss_manage_error_body(&result.output))
        }
    };
    ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "rss".to_owned(),
        status,
        effect: rss_manage_effect(&result.output),
        presentation: OutcomePresentation::Trusted,
        blocks: vec![block],
        error_code,
        command: Some("rss".to_owned()),
    }
}

pub(crate) fn tool_outcome_from_train_result(
    result: &ToolExecutionResult,
) -> Option<ToolExecutionOutcome> {
    if result.name != TRAIN_TOOL_NAME {
        return None;
    }

    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = structured_error_code(&result.output);
    let block = match status {
        ToolOutcomeStatus::Succeeded => train_schedule_from_output(&result.output)
            .map(|schedule| ResponseBlock::FactCard(format_train_schedule_reply(&schedule)))
            .unwrap_or_else(|| ResponseBlock::Error(train_error_body(Some("provider_error")))),
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

pub(crate) fn tool_outcome_from_web_search_result(
    result: &ToolExecutionResult,
) -> Option<ToolExecutionOutcome> {
    if result.name != WEB_SEARCH_TOOL_NAME {
        return None;
    }

    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = structured_error_code(&result.output);
    let block = match status {
        ToolOutcomeStatus::Succeeded => ResponseBlock::FactCard(structured_command_body(
            format_web_search_tool_reply(&result.output),
        )),
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(web_search_skip_body(&result.output)),
        ToolOutcomeStatus::RequiresClarification => {
            ResponseBlock::Clarification(CommandBody::plain("请说明要联网查询什么内容。"))
        }
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => {
            ResponseBlock::Error(web_search_error_body(&result.output))
        }
    };

    Some(ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "search".to_owned(),
        status,
        effect: ToolEffect::ReadOnly,
        presentation: OutcomePresentation::Trusted,
        blocks: vec![block],
        error_code,
        command: Some("web_search".to_owned()),
    })
}

pub(crate) fn tool_outcome_from_weather_result(
    result: &ToolExecutionResult,
) -> Option<ToolExecutionOutcome> {
    if result.name != WEATHER_TOOL_NAME {
        return None;
    }

    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = structured_error_code(&result.output);
    let block = match status {
        ToolOutcomeStatus::Succeeded => ResponseBlock::FactCard(weather_fact_card(&result.output)),
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(weather_skip_body(&result.output)),
        ToolOutcomeStatus::RequiresClarification => {
            ResponseBlock::Clarification(CommandBody::plain("请说明要查询哪个城市的天气。"))
        }
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => {
            ResponseBlock::Error(weather_error_body(&result.output))
        }
    };

    Some(ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "weather".to_owned(),
        status,
        effect: ToolEffect::ReadOnly,
        presentation: OutcomePresentation::Trusted,
        blocks: vec![block],
        error_code,
        command: Some("weather".to_owned()),
    })
}

pub(crate) fn tool_outcome_from_knowledge_result(
    result: &ToolExecutionResult,
) -> Option<ToolExecutionOutcome> {
    if result.name != KNOWLEDGE_SEARCH_TOOL_NAME {
        return None;
    }

    let evidence_status = string_field(&result.output, "status");
    let (status, presentation, blocks, error_code) = match evidence_status.as_deref() {
        Some("ok" | "truncated") if result.succeeded => (
            ToolOutcomeStatus::Succeeded,
            OutcomePresentation::Internal,
            Vec::new(),
            None,
        ),
        Some("no_hit") => (
            ToolOutcomeStatus::Failed,
            OutcomePresentation::Trusted,
            vec![ResponseBlock::Warning(CommandBody::plain(
                "本地知识库没有找到相关证据，无法据此给出知识库结论。",
            ))],
            Some("knowledge_no_hit".to_owned()),
        ),
        Some("low_relevance") => (
            ToolOutcomeStatus::Failed,
            OutcomePresentation::Trusted,
            vec![ResponseBlock::Warning(CommandBody::plain(
                "本地知识库只有低相关片段，当前证据不足，无法据此下结论。",
            ))],
            Some("knowledge_low_relevance".to_owned()),
        ),
        Some("failed") => (
            ToolOutcomeStatus::Failed,
            OutcomePresentation::Trusted,
            vec![ResponseBlock::Error(CommandBody::plain(
                "本地知识检索失败，当前不能基于知识库回答。",
            ))],
            structured_error_code(&result.output)
                .or_else(|| Some("knowledge_search_failed".to_owned())),
        ),
        _ => (
            ToolOutcomeStatus::Failed,
            OutcomePresentation::Trusted,
            vec![ResponseBlock::Error(CommandBody::plain(
                "本地知识检索返回了无效结果，当前不能基于知识库回答。",
            ))],
            Some("knowledge_invalid_result".to_owned()),
        ),
    };
    Some(ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "knowledge".to_owned(),
        status,
        effect: ToolEffect::ReadOnly,
        presentation,
        blocks,
        error_code,
        command: Some("knowledge".to_owned()),
    })
}

fn rss_fact_card(output: &Value) -> CommandBody {
    let query = string_field(output, "query");
    let items = output
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if items.is_empty() {
        let text = match query.as_deref() {
            Some(query) => format!("📰 RSS 最近记录\n\n未找到与“{query}”匹配的本地 RSS 条目。"),
            None => "📰 RSS 最近记录\n\n当前目标没有已入库的 RSS 条目。".to_owned(),
        };
        let markdown = match query.as_deref() {
            Some(query) => format!("# 📰 RSS 最近记录\n\n未找到与 `{query}` 匹配的本地 RSS 条目。"),
            None => "# 📰 RSS 最近记录\n\n当前目标没有已入库的 RSS 条目。".to_owned(),
        };
        return CommandBody::dual(text, markdown);
    }

    let title = query
        .as_deref()
        .map(|query| format!("📰 RSS 最近记录：{query}"))
        .unwrap_or_else(|| "📰 RSS 最近记录".to_owned());
    let mut text_lines = vec![title.clone(), String::new()];
    let mut markdown_lines = vec![format!("# {title}"), String::new()];
    for item in items.iter().take(10) {
        let entry = item.get("item").unwrap_or(&Value::Null);
        let subscription = item.get("subscription").unwrap_or(&Value::Null);
        let feed_title =
            string_field(subscription, "title").unwrap_or_else(|| "未命名订阅".to_owned());
        let item_title = string_field(entry, "title").unwrap_or_else(|| "无标题".to_owned());
        text_lines.push(format!("【{feed_title}】{item_title}"));
        markdown_lines.push(format!("## {}", item_title));
        markdown_lines.push(format!("订阅：**{}**  ", feed_title));
        if let Some(summary) = string_field(entry, "summary") {
            let summary = truncate_chars(&summary, 260);
            text_lines.push(summary.clone());
            markdown_lines.push(summary);
        }
        if let Some(time_line) = rss_item_time_line(entry) {
            text_lines.push(time_line.clone());
            markdown_lines.push(format!("{time_line}  "));
        }
        if let Some(link) = string_field(entry, "link") {
            text_lines.push(format!("链接：{link}"));
            markdown_lines.push(format!("链接：{link}"));
        }
        text_lines.push(String::new());
        markdown_lines.push(String::new());
    }
    text_lines.push("以上为本地已轮询入库记录，不代表刚刚刷新远端 RSS。".to_owned());
    markdown_lines.push("> 以上为本地已轮询入库记录，不代表刚刚刷新远端 RSS。".to_owned());

    CommandBody::dual(
        truncate_chars(&text_lines.join("\n"), RSS_FACT_MAX_CHARS),
        truncate_chars(&markdown_lines.join("\n"), RSS_FACT_MAX_CHARS),
    )
}

fn rss_item_time_line(entry: &Value) -> Option<String> {
    let primary = optional_string_field(entry, "updated_at")
        .map(|value| ("更新时间", value))
        .or_else(|| optional_string_field(entry, "published_at").map(|value| ("发布时间", value)));
    let mut parts = Vec::new();
    if let Some((label, value)) = primary {
        parts.push(format!("{label}：{}", format_rss_time_for_display(&value)));
    }
    if let Some(value) = optional_string_field(entry, "pushed_at") {
        parts.push(format!("推送时间：{}", format_rss_time_for_display(&value)));
    } else if let Some(value) = optional_string_field(entry, "last_seen_at") {
        parts.push(format!("入库时间：{}", format_rss_time_for_display(&value)));
    }
    (!parts.is_empty()).then(|| parts.join("；"))
}

fn rss_manage_body(output: &Value) -> CommandBody {
    let operation = string_field(output, "operation").unwrap_or_else(|| "manage".to_owned());
    let title = match operation.as_str() {
        "add" => "RSS 新增结果",
        "delete" => "RSS 删除结果",
        _ => "RSS 管理结果",
    };
    let mut lines = vec![title.to_owned(), String::new()];
    let mut markdown = vec![format!("# {title}"), String::new()];

    match operation.as_str() {
        "add" => {
            append_rss_manage_items(
                &mut lines,
                &mut markdown,
                output.get("created").and_then(Value::as_array),
                "已添加",
            );
            append_rss_manage_failures(
                &mut lines,
                &mut markdown,
                output.get("failed").and_then(Value::as_array),
                "失败",
            );
        }
        "delete" => {
            append_rss_manage_items(
                &mut lines,
                &mut markdown,
                output.get("deleted").and_then(Value::as_array),
                "已删除",
            );
            if let Some(missing) = output.get("missing").and_then(Value::as_array)
                && !missing.is_empty()
            {
                lines.push(format!("未找到 {} 个目标：", missing.len()));
                markdown.push(format!("## 未找到 {} 个目标", missing.len()));
                for (index, item) in missing.iter().enumerate() {
                    if let Some(value) = item.as_str() {
                        lines.push(format!("{}. {}", index + 1, value));
                        markdown.push(format!("{}. `{}`", index + 1, value));
                    }
                }
            }
        }
        _ => {
            if let Some(message) = string_field(output, "message") {
                lines.push(message.clone());
                markdown.push(message);
            }
        }
    }
    CommandBody::dual(lines.join("\n"), markdown.join("\n"))
}

fn rss_manage_error_body(output: &Value) -> CommandBody {
    if rss_manage_has_business_details(output) {
        return rss_manage_body(output);
    }
    rss_error_body(output)
}

fn rss_manage_has_business_details(output: &Value) -> bool {
    string_field(output, "operation")
        .map(|operation| matches!(operation.as_str(), "add" | "delete"))
        .unwrap_or(false)
        || output
            .get("failed")
            .and_then(Value::as_array)
            .map(|items| !items.is_empty())
            .unwrap_or(false)
        || output
            .get("missing")
            .and_then(Value::as_array)
            .map(|items| !items.is_empty())
            .unwrap_or(false)
}

fn append_rss_manage_items(
    lines: &mut Vec<String>,
    markdown: &mut Vec<String>,
    items: Option<&Vec<Value>>,
    label: &str,
) {
    let Some(items) = items.filter(|items| !items.is_empty()) else {
        return;
    };
    lines.push(format!("{label} {} 个订阅：", items.len()));
    markdown.push(format!("## {label} {} 个订阅", items.len()));
    for (index, item) in items.iter().enumerate() {
        let title = string_field(item, "title").unwrap_or_else(|| "未命名订阅".to_owned());
        let url = string_field(item, "url").unwrap_or_default();
        lines.push(format!("{}. {} {}", index + 1, title, url));
        markdown.push(format!("{}. **{}** {}", index + 1, title, url));
    }
}

fn append_rss_manage_failures(
    lines: &mut Vec<String>,
    markdown: &mut Vec<String>,
    items: Option<&Vec<Value>>,
    label: &str,
) {
    let Some(items) = items.filter(|items| !items.is_empty()) else {
        return;
    };
    lines.push(format!("{label} {} 个：", items.len()));
    markdown.push(format!("## {label} {} 个", items.len()));
    for (index, item) in items.iter().enumerate() {
        let url = string_field(item, "url").unwrap_or_else(|| "未知地址".to_owned());
        let error = string_field(item, "error").unwrap_or_else(|| "未知错误".to_owned());
        lines.push(format!("{}. {}：{}", index + 1, url, error));
        markdown.push(format!("{}. `{}`：{}", index + 1, url, error));
    }
}

fn rss_manage_effect(output: &Value) -> ToolEffect {
    match string_field(output, "operation").as_deref() {
        Some("add") => ToolEffect::Created,
        Some("delete") => ToolEffect::Deleted,
        _ => ToolEffect::Updated,
    }
}

fn rss_error_body(output: &Value) -> CommandBody {
    let code = structured_error_code(output);
    let text = match code.as_deref() {
        Some("bad_tool_arguments") => {
            "【RSS】\n\nRSS 参数不完整，请说明订阅名、关键词，或把条数限制在 1 到 20。"
        }
        Some("permission_denied") => "【RSS】\n\n群聊 RSS 管理只允许群主或管理员执行。",
        _ => "【RSS】\n\nRSS 本地记录暂时无法读取，请稍后再试。",
    };
    CommandBody::plain(text)
}

fn rss_skip_body(output: &Value) -> CommandBody {
    let text = match string_field(output, "reason").as_deref() {
        Some("dependency_previous_call_failed") => {
            "RSS 查询因前序工具失败已跳过；根因以上方失败信息为准。".to_owned()
        }
        Some(reason) => format!("RSS 查询已跳过：{reason}。"),
        None => "RSS 查询已跳过。".to_owned(),
    };
    CommandBody::plain(text)
}

fn train_schedule_from_output(output: &Value) -> Option<TrainSchedule> {
    let travel_date =
        NaiveDate::parse_from_str(&string_field(output, "travel_date")?, "%Y-%m-%d").ok()?;
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
            .unwrap_or_else(|| string_field(output, "train_code").unwrap_or_default()),
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

fn web_search_error_body(output: &Value) -> CommandBody {
    let code = structured_error_code(output).unwrap_or_else(|| "provider_error".to_owned());
    let stage = output
        .get("error")
        .and_then(|error| error.get("stage"))
        .and_then(Value::as_str)
        .unwrap_or("web_search");
    let err = LlmError::new(code, "web search tool failed", stage);
    let reply = format_web_search_error_reply(&err);
    if string_field(output, "mode").as_deref() == Some("multi_entity_research") {
        return structured_command_body(format_web_search_research_error_reply(output, &reply));
    }
    structured_command_body(reply)
}

fn web_search_skip_body(output: &Value) -> CommandBody {
    let text = match string_field(output, "reason").as_deref() {
        Some("dependency_previous_call_failed") => {
            "联网查询因前序工具失败已跳过；根因以上方失败信息为准。".to_owned()
        }
        Some(reason) => format!("联网查询已跳过：{reason}。"),
        None => "联网查询已跳过。".to_owned(),
    };
    CommandBody::plain(text)
}

fn weather_fact_card(output: &Value) -> CommandBody {
    let location = output.get("location").unwrap_or(&Value::Null);
    let current = output.get("current").unwrap_or(&Value::Null);
    let daily = output
        .get("daily")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let name = string_field(location, "name").unwrap_or_else(|| "当前城市".to_owned());
    let full_location = full_location_label(location, &name);
    let weather = current
        .get("weather_code")
        .and_then(Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .map(weather_code_label)
        .unwrap_or("未知");
    let temp = number_field(current, "temperature_c")
        .map(format_number)
        .unwrap_or_else(|| "--".to_owned());
    let time = string_field(current, "time")
        .map(|value| short_time(&value))
        .unwrap_or_else(|| "--:--".to_owned());

    let mut text_lines = vec![format!("🌦 {name}天气")];
    let mut markdown_lines = vec![format!("# 🌦 {name}天气")];
    if let Some(detail) = location_detail(&name, &full_location) {
        text_lines.push(detail.clone());
        markdown_lines.push(format!("**{detail}**"));
    }
    text_lines.push(String::new());
    markdown_lines.push(String::new());
    text_lines.push(format!("当前 {time}｜{weather}｜{temp}°C"));
    markdown_lines.push(format!("**当前 {time}｜{weather}｜{temp}°C**  "));
    if let Some(details) = current_details(current) {
        text_lines.push(details.clone());
        markdown_lines.push(format!("{details}  "));
    }
    if let Some(air) = air_quality_summary(output) {
        text_lines.push(air.clone());
        markdown_lines.push(air);
    }

    let forecast = daily
        .iter()
        .take(3)
        .filter_map(format_daily_summary)
        .collect::<Vec<_>>();
    if !forecast.is_empty() {
        text_lines.push(String::new());
        markdown_lines.push(String::new());
        text_lines.push(format!("未来 {} 天", forecast.len()));
        markdown_lines.push(format!("## 未来 {} 天", forecast.len()));
        for line in forecast {
            text_lines.push(format!("- {line}"));
            markdown_lines.push(format!("- **{}**", line));
        }
    }

    CommandBody::dual(
        truncate_chars(&text_lines.join("\n"), WEATHER_FACT_MAX_CHARS),
        truncate_chars(&markdown_lines.join("\n"), WEATHER_FACT_MAX_CHARS),
    )
}

fn format_daily_summary(day: &Value) -> Option<String> {
    let date = string_field(day, "date")?;
    let weather = string_field(day, "weather_day")
        .or_else(|| string_field(day, "weather_night"))
        .or_else(|| {
            day.get("weather_code")
                .and_then(Value::as_u64)
                .and_then(|code| u16::try_from(code).ok())
                .map(weather_code_label)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "未知".to_owned());
    let min = number_field(day, "temperature_min_c").map(format_number);
    let max = number_field(day, "temperature_max_c").map(format_number);
    let temp = match (min, max) {
        (Some(min), Some(max)) => format!("{min}～{max}°C"),
        (None, Some(max)) => format!("最高 {max}°C"),
        (Some(min), None) => format!("最低 {min}°C"),
        (None, None) => "温度未知".to_owned(),
    };
    let mut parts = vec![format_forecast_day_label(&date, None), weather, temp];
    if let Some(probability) = day
        .get("precipitation_probability_max")
        .and_then(Value::as_u64)
    {
        parts.push(format!("降水概率 {probability}%"));
    }
    Some(parts.join("，"))
}

fn current_details(current: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(apparent) = number_field(current, "apparent_temperature_c") {
        parts.push(format!("体感 {}°C", format_number(apparent)));
    }
    if let Some(humidity) = current.get("humidity_percent").and_then(Value::as_u64) {
        parts.push(format!("湿度 {humidity}%"));
    }
    if let Some(precipitation) = number_field(current, "precipitation_mm")
        && precipitation > 0.0
    {
        parts.push(format!("降水 {}mm", format_number(precipitation)));
    }
    if let Some(wind) = format_wind(
        string_field(current, "wind_direction").as_deref(),
        string_field(current, "wind_scale").as_deref(),
    ) {
        parts.push(wind);
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn air_quality_summary(output: &Value) -> Option<String> {
    let air = output.get("air_quality")?.get("summary")?;
    let aqi = string_field(air, "aqi_display")?;
    let category = string_field(air, "category");
    let mut text = format!("空气质量：AQI {aqi}");
    if let Some(category) = category {
        text.push_str(&format!("（{category}）"));
    }
    if let Some(primary) = string_field(air, "primary_pollutant") {
        text.push_str(&format!(" · 首要污染物 {primary}"));
    }
    Some(text)
}

fn weather_error_body(output: &Value) -> CommandBody {
    let code = structured_error_code(output);
    let text = match code.as_deref() {
        Some("not_found") => "【天气】\n\n没找到这个城市。可以换成更完整的城市名再试。",
        Some("timeout") => "【天气】\n\n天气服务超时了，请稍后再试。",
        Some("bad_tool_arguments") => "【天气】\n\n天气查询参数不完整，请说明要查询的城市。",
        _ => "【天气】\n\n天气服务暂时不可用，请稍后再试。",
    };
    CommandBody::plain(text)
}

fn weather_skip_body(output: &Value) -> CommandBody {
    let text = match string_field(output, "reason").as_deref() {
        Some("dependency_previous_call_failed") => {
            "天气查询因前序工具失败已跳过；根因以上方失败信息为准。".to_owned()
        }
        Some(reason) => format!("天气查询已跳过：{reason}。"),
        None => "天气查询已跳过。".to_owned(),
    };
    CommandBody::plain(text)
}

fn full_location_label(location: &Value, name: &str) -> String {
    let mut parts = vec![name.trim().to_owned()];
    for key in ["admin2", "admin1", "country"] {
        if let Some(value) = string_field(location, key)
            && !parts.iter().any(|part| part == &value)
        {
            parts.push(value);
        }
    }
    parts.join("，")
}

fn location_detail(name: &str, full_location: &str) -> Option<String> {
    let name = name.trim();
    let full_location = full_location.trim();
    if full_location.is_empty() || full_location == name {
        None
    } else {
        Some(full_location.to_owned())
    }
}

fn format_wind(direction: Option<&str>, scale: Option<&str>) -> Option<String> {
    match (
        direction.map(str::trim).filter(|value| !value.is_empty()),
        scale.map(str::trim).filter(|value| !value.is_empty()),
    ) {
        (Some(direction), Some(scale)) => Some(format!("{direction} {scale}级")),
        (Some(direction), None) => Some(direction.to_owned()),
        (None, Some(scale)) => Some(format!("{scale}级风")),
        (None, None) => None,
    }
}

fn short_time(value: &str) -> String {
    let display = format_local_time_for_display(value);
    display
        .split_once(' ')
        .map(|(_, time)| time.get(..5).unwrap_or(time).to_owned())
        .unwrap_or(display)
}

fn number_field(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn format_number(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
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

    use qq_maid_llm::provider::ToolExecutionResult;

    use super::*;

    fn rss_manage_result(output: Value) -> ToolExecutionResult {
        ToolExecutionResult {
            name: RSS_MANAGE_TOOL_NAME.to_owned(),
            output,
            succeeded: true,
        }
    }

    fn web_search_result(output: Value) -> ToolExecutionResult {
        ToolExecutionResult {
            name: WEB_SEARCH_TOOL_NAME.to_owned(),
            output,
            succeeded: true,
        }
    }

    fn web_search_fact_text(output: Value) -> String {
        let outcome = tool_outcome_from_web_search_result(&web_search_result(output)).unwrap();
        let ResponseBlock::FactCard(body) = &outcome.blocks[0] else {
            panic!("expected web search fact card");
        };
        body.text.clone()
    }

    #[test]
    fn single_web_search_keeps_top_level_answer_card() {
        let text = web_search_fact_text(json!({
            "answer": "单次搜索的明确答案",
            "sources": []
        }));

        assert!(text.starts_with("【联网查询】"));
        assert!(text.contains("单次搜索的明确答案"));
        assert!(!text.contains("没查到明确结果"));
    }

    #[test]
    fn single_web_search_with_only_sources_does_not_look_empty() {
        let text = web_search_fact_text(json!({
            "answer": "",
            "sources": [{
                "title": "来源标题",
                "url": "https://example.test/source",
                "snippet": "来源摘要"
            }]
        }));

        assert!(text.contains("来源标题"));
        assert!(text.contains("来源摘要"));
        assert!(!text.contains("没查到明确结果"));
    }

    #[test]
    fn multi_entity_web_search_renders_facts_without_top_level_answer() {
        let text = web_search_fact_text(json!({
            "mode": "multi_entity_research",
            "successful": 1,
            "failed": 0,
            "results": [{
                "entity": "项目甲",
                "status": "success",
                "facts": "项目甲支持能力 A",
                "sources": [{
                    "title": "项目甲文档",
                    "url": "https://example.test/project-a",
                    "snippet": "官方功能摘要"
                }]
            }]
        }));

        assert!(text.starts_with("【联网查询】"));
        assert!(text.contains("项目甲支持能力 A"));
        assert!(text.contains("项目甲文档"));
        assert!(!text.contains("没查到明确结果"));
    }

    #[test]
    fn multi_entity_web_search_shows_partial_success_counts() {
        let text = web_search_fact_text(json!({
            "mode": "multi_entity_research",
            "successful": "类型异常",
            "failed": null,
            "results": [{
                "entity": "成功项",
                "status": "success",
                "facts": "成功事实"
            }, {
                "entity": "失败项",
                "status": "failed",
                "facts": "不应展示的失败详情",
                "error": {"message": "内部错误"}
            }]
        }));

        assert!(text.starts_with("【联网查询（成功 1，失败 1）】"));
        assert!(text.contains("成功事实"));
        assert!(!text.contains("不应展示的失败详情"));
        assert!(!text.contains("内部错误"));
    }

    #[test]
    fn multi_entity_web_search_counts_timeout_as_failure() {
        let text = web_search_fact_text(json!({
            "mode": "multi_entity_research",
            "results": [{
                "entity": "成功项",
                "status": "success",
                "facts": "成功事实"
            }, {
                "entity": "超时项",
                "status": "timeout"
            }, {
                "entity": "失败项",
                "status": "failed"
            }]
        }));

        assert!(text.starts_with("【联网查询（成功 1，失败 2）】"));
        assert!(text.contains("成功事实"));
    }

    #[test]
    fn all_failed_multi_entity_web_search_keeps_friendly_failure_hint() {
        let outcome = tool_outcome_from_web_search_result(&web_search_result(json!({
            "ok": false,
            "mode": "multi_entity_research",
            "successful": 0,
            "failed": 2,
            "results": [{
                "entity": "失败项",
                "status": "failed",
                "error": {"message": "内部错误"}
            }, {
                "entity": "超时项",
                "status": "timeout"
            }]
        })))
        .unwrap();

        assert_eq!(outcome.status, ToolOutcomeStatus::Failed);
        let ResponseBlock::Error(body) = &outcome.blocks[0] else {
            panic!("expected web search error block");
        };
        assert!(body.text.starts_with("【联网查询（成功 0，失败 2）】"));
        assert!(body.text.contains("联网查询服务暂时不可用"));
        assert!(!body.text.contains("内部错误"));
    }

    #[test]
    fn rss_manage_failed_add_renders_business_failures() {
        let outcome = rss_manage_outcome(&rss_manage_result(json!({
            "ok": false,
            "operation": "add",
            "created": [],
            "failed": [
                {"url": "https://example.test/feed.xml", "error": "rss subscription already exists"}
            ],
            "message": "RSS 批量新增完成：成功 0 个，失败 1 个。"
        })));

        assert_eq!(outcome.status, ToolOutcomeStatus::Failed);
        let ResponseBlock::Error(body) = &outcome.blocks[0] else {
            panic!("expected RSS manage error block");
        };
        assert!(body.text.contains("RSS 新增结果"));
        assert!(body.text.contains("https://example.test/feed.xml"));
        assert!(body.text.contains("rss subscription already exists"));
        assert!(!body.text.contains("RSS 本地记录暂时无法读取"));
    }

    #[test]
    fn rss_manage_failed_delete_renders_missing_targets() {
        let outcome = rss_manage_outcome(&rss_manage_result(json!({
            "ok": false,
            "operation": "delete",
            "deleted": [],
            "missing": ["1", "missing-id"],
            "message": "RSS 批量删除完成：成功 0 个，未找到 2 个。"
        })));

        assert_eq!(outcome.status, ToolOutcomeStatus::Failed);
        let ResponseBlock::Error(body) = &outcome.blocks[0] else {
            panic!("expected RSS manage error block");
        };
        assert!(body.text.contains("RSS 删除结果"));
        assert!(body.text.contains("未找到 2 个目标"));
        assert!(body.text.contains("missing-id"));
        assert!(!body.text.contains("RSS 本地记录暂时无法读取"));
    }
}
