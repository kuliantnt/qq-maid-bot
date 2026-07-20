//! Todo 主动查询参数归一化。

use qq_maid_common::time_context::{
    RequestTimeContext, parse_date_range_expression, parse_local_datetime_for_comparison,
    parse_single_date_expression,
};

use super::{
    TodoError, TodoListDateField, TodoQuery, TodoQueryStatus, TodoQueryTimeFilter,
    storage::validate_todo_query,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedTodoQuery {
    pub query: TodoQuery,
    pub condition: String,
}

/// 解析 `/todo list ...` 的组合筛选。未识别的剩余文本作为关键词参与 AND 查询，
/// 因此 `/todo list 项目 A` 会在标题、详情和原文中做模糊匹配。
pub(crate) fn parse_todo_list_query(
    argument: &str,
    ctx: &RequestTimeContext,
) -> Result<ParsedTodoQuery, TodoError> {
    let mut query = TodoQuery::default();
    let mut explicit_status = None;
    let mut keyword_parts = Vec::new();
    let mut time_condition = None;
    let tokens = argument.split_whitespace().collect::<Vec<_>>();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        match token {
            "未完成" | "进行中" | "pending" => {
                set_explicit_status(&mut explicit_status, TodoQueryStatus::Pending)?;
            }
            "已完成" | "completed" => {
                set_explicit_status(&mut explicit_status, TodoQueryStatus::Completed)?;
            }
            "全部" | "all" => {
                set_explicit_status(&mut explicit_status, TodoQueryStatus::All)?;
            }
            "逾期" | "overdue" => {
                set_time_filter(
                    &mut query,
                    TodoQueryTimeFilter::Overdue {
                        now: parse_local_datetime_for_comparison(ctx.current_time())
                            .expect("request time context must contain a valid local datetime"),
                    },
                )?;
                time_condition = Some("逾期".to_owned());
            }
            "无截止时间" | "无日期" | "no-due" => {
                set_time_filter(&mut query, TodoQueryTimeFilter::NoDueDate)?;
                time_condition = Some("无截止时间".to_owned());
            }
            "周期" | "周期性" | "重复" | "recurring" => {
                set_recurring_filter(&mut query, true)?;
            }
            "一次" | "一次性" | "非周期" | "非重复" | "one-off" | "once" => {
                set_recurring_filter(&mut query, false)?;
            }
            "关键词" | "keyword" => {
                let keyword = tokens[index + 1..].join(" ");
                if keyword.trim().is_empty() {
                    return Err(TodoError::bad_request(
                        "关键词不能为空。用法：/todo list 关键词 报告",
                    ));
                }
                keyword_parts.push(keyword);
                break;
            }
            _ => {
                if let Some(range) = parse_date_range_expression(token, ctx) {
                    set_time_filter(
                        &mut query,
                        TodoQueryTimeFilter::DateRange {
                            start: range.start,
                            end: range.end,
                            field: TodoListDateField::Planned,
                        },
                    )?;
                    time_condition = Some(range.raw);
                } else if let Some(date) = parse_single_date_expression(token, ctx) {
                    // 支持 /todo list 2099-07-15 这类绝对日期；相对表达优先走 date_range。
                    set_time_filter(
                        &mut query,
                        TodoQueryTimeFilter::DateRange {
                            start: date.date,
                            end: date.date,
                            field: TodoListDateField::Planned,
                        },
                    )?;
                    time_condition = Some(date.raw);
                } else {
                    keyword_parts.push(token.to_owned());
                }
            }
        }
        index += 1;
    }
    query.status = explicit_status.unwrap_or_default();
    if !keyword_parts.is_empty() {
        let keyword = keyword_parts.join(" ");
        query.keyword = Some(keyword);
    }
    validate_todo_query(&query)?;
    if matches!(query.status, TodoQueryStatus::Completed)
        && let Some(TodoQueryTimeFilter::DateRange { field, .. }) = query.time.as_mut()
    {
        *field = TodoListDateField::CompletedAt;
    }
    // 展示条件按固定类别排序，避免用户调整 token 顺序后标题或快照发生变化。
    let mut conditions = Vec::new();
    if let Some(status) = explicit_status {
        conditions.push(status_condition(status).to_owned());
    }
    if let Some(condition) = time_condition {
        conditions.push(condition);
    }
    if let Some(keyword) = query.keyword.as_deref() {
        conditions.push(format!("关键词“{keyword}”"));
    }
    if let Some(recurring) = query.recurring {
        conditions.push(if recurring {
            "周期性待办".to_owned()
        } else {
            "一次性待办".to_owned()
        });
    }
    Ok(ParsedTodoQuery {
        query,
        condition: conditions.join("、"),
    })
}

fn set_explicit_status(
    explicit_status: &mut Option<TodoQueryStatus>,
    status: TodoQueryStatus,
) -> Result<(), TodoError> {
    match explicit_status {
        Some(current) if *current != status => {
            Err(TodoError::bad_request("一次查询只能指定一个状态条件。"))
        }
        Some(_) => Ok(()),
        None => {
            *explicit_status = Some(status);
            Ok(())
        }
    }
}

fn status_condition(status: TodoQueryStatus) -> &'static str {
    match status {
        TodoQueryStatus::Pending => "未完成",
        TodoQueryStatus::Completed => "已完成",
        TodoQueryStatus::All => "全部状态",
    }
}

fn set_time_filter(query: &mut TodoQuery, filter: TodoQueryTimeFilter) -> Result<(), TodoError> {
    if query.time.is_some() {
        return Err(TodoError::bad_request("一次查询只能指定一个时间条件。"));
    }
    query.time = Some(filter);
    Ok(())
}

fn set_recurring_filter(query: &mut TodoQuery, recurring: bool) -> Result<(), TodoError> {
    match query.recurring {
        Some(current) if current != recurring => {
            Err(TodoError::bad_request("一次查询只能指定一个周期类型条件。"))
        }
        Some(_) => Ok(()),
        None => {
            query.recurring = Some(recurring);
            Ok(())
        }
    }
}
