//! Todo 主动查询参数归一化。

use qq_maid_common::time_context::{
    RequestTimeContext, parse_date_range_expression, parse_local_datetime_for_comparison,
};

use super::{TodoError, TodoListDateField, TodoQuery, TodoQueryStatus, TodoQueryTimeFilter};

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
    let mut keyword_parts = Vec::new();
    let mut conditions = Vec::new();
    let tokens = argument.split_whitespace().collect::<Vec<_>>();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        match token {
            "未完成" | "进行中" | "pending" => {
                query.status = TodoQueryStatus::Pending;
                conditions.push("未完成".to_owned());
            }
            "已完成" | "completed" => {
                query.status = TodoQueryStatus::Completed;
                conditions.push("已完成".to_owned());
            }
            "全部" | "all" => {
                query.status = TodoQueryStatus::All;
                conditions.push("全部状态".to_owned());
            }
            "逾期" | "overdue" => {
                set_time_filter(
                    &mut query,
                    TodoQueryTimeFilter::Overdue {
                        now: parse_local_datetime_for_comparison(ctx.current_time())
                            .expect("request time context must contain a valid local datetime"),
                    },
                )?;
                query.status = TodoQueryStatus::Pending;
                conditions.push("逾期".to_owned());
            }
            "无截止时间" | "无日期" | "no-due" => {
                set_time_filter(&mut query, TodoQueryTimeFilter::NoDueDate)?;
                conditions.push("无截止时间".to_owned());
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
                    conditions.push(range.raw);
                } else {
                    keyword_parts.push(token.to_owned());
                }
            }
        }
        index += 1;
    }
    if !keyword_parts.is_empty() {
        let keyword = keyword_parts.join(" ");
        conditions.push(format!("关键词“{keyword}”"));
        query.keyword = Some(keyword);
    }
    if matches!(query.status, TodoQueryStatus::Completed)
        && let Some(TodoQueryTimeFilter::DateRange { field, .. }) = query.time.as_mut()
    {
        *field = TodoListDateField::CompletedAt;
    }
    Ok(ParsedTodoQuery {
        query,
        condition: conditions.join("、"),
    })
}

fn set_time_filter(query: &mut TodoQuery, filter: TodoQueryTimeFilter) -> Result<(), TodoError> {
    if query.time.is_some() {
        return Err(TodoError::bad_request("一次查询只能指定一个时间条件。"));
    }
    query.time = Some(filter);
    Ok(())
}
