use super::*;

pub(super) fn list_spec_from_output(output: &Value) -> RelatedListSpec {
    let status = string_field(output, "status");
    let mut spec = match status.as_deref() {
        Some("completed") => completed_list_spec(),
        Some("all") => RelatedListSpec { ..all_list_spec() },
        _ => pending_list_spec(),
    };
    spec.query_status = match status.as_deref() {
        Some("completed") => TodoQueryStatus::Completed,
        Some("all") => TodoQueryStatus::All,
        _ => TodoQueryStatus::Pending,
    };
    spec.shared_query = true;
    spec.condition = string_field(output, "condition").unwrap_or_default();
    spec.keyword = string_field(output, "keyword");
    spec.special_time_filter = string_field(output, "time_filter");
    spec.recurring = bool_field(output, "recurring");
    if let Some(due_date) = string_field(output, "due_date")
        .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok())
    {
        spec.condition = due_date.format("%Y-%m-%d").to_string();
        spec.due_date = Some(due_date);
    } else if let (Some(start), Some(end)) = (
        string_field(output, "due_start")
            .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok()),
        string_field(output, "due_end")
            .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok()),
    ) {
        spec.condition = string_field(output, "date_range_text").unwrap_or_else(|| {
            format!("{} 至 {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"))
        });
        spec.due_range = Some((start, end));
        spec.date_field = date_field_from_output(output, status.as_deref());
    }
    spec.query_type = match status.as_deref() {
        Some("all") => "all",
        Some("completed") => "completed-list",
        _ if spec.due_date.is_some() || spec.due_range.is_some() => "due-date",
        _ if spec.keyword.is_some() || spec.recurring.is_some() => "search",
        _ => "list",
    };
    spec
}

pub(super) fn list_spec_from_replay(
    query: &TodoQuery,
    condition: &str,
    output: &Value,
) -> RelatedListSpec {
    let mut spec = match query.status {
        TodoQueryStatus::Pending => pending_list_spec(),
        TodoQueryStatus::Completed => completed_list_spec(),
        TodoQueryStatus::All => all_list_spec(),
    };
    spec.query_status = query.status;
    spec.query_type = crate::runtime::tools::todo::todo_query_type(query);
    spec.condition = condition.to_owned();
    spec.keyword = query.keyword.clone();
    spec.recurring = query.recurring;
    spec.shared_query = true;
    match query.time.as_ref() {
        Some(TodoQueryTimeFilter::DateRange { start, end, field }) => {
            if start == end {
                spec.due_date = Some(*start);
            } else {
                spec.due_range = Some((*start, *end));
            }
            spec.date_field = *field;
        }
        Some(TodoQueryTimeFilter::Overdue { .. }) => {
            spec.special_time_filter = Some("overdue".to_owned())
        }
        Some(TodoQueryTimeFilter::NoDueDate) => {
            spec.special_time_filter = Some("no_due_date".to_owned())
        }
        None => {}
    }
    if spec.condition.is_empty() {
        spec.condition = string_field(output, "condition").unwrap_or_default();
    }
    spec
}

pub(super) fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

pub(super) fn all_list_spec() -> RelatedListSpec {
    RelatedListSpec {
        status: TodoStatus::Pending,
        query_status: TodoQueryStatus::All,
        query_type: "all",
        condition: "全部待办".to_owned(),
        due_date: None,
        due_range: None,
        date_field: TodoListDateField::Planned,
        keyword: None,
        special_time_filter: None,
        recurring: None,
        shared_query: false,
        title: "📋 全部待办",
        empty_text: "当前没有待办。",
        time_value: todo_due_chip,
    }
}

pub(super) fn date_field_from_output(output: &Value, status: Option<&str>) -> TodoListDateField {
    match string_field(output, "date_range_field").as_deref() {
        Some("completed_at") => TodoListDateField::CompletedAt,
        Some("planned") => TodoListDateField::Planned,
        _ => match status {
            Some("completed") => TodoListDateField::CompletedAt,
            _ => TodoListDateField::Planned,
        },
    }
}

pub(super) fn display_todo_completed_at(item: &TodoItem) -> Option<String> {
    item.completed_at.as_deref().and_then(todo_timestamp_chip)
}

pub(super) fn success_lines(title: &str, item: Option<&ReceiptItem>) -> Vec<String> {
    let mut lines = vec![title.to_owned()];
    if let Some(item) = item {
        lines.push(String::new());
        lines.push(format!("- {}", item.title));
        if let Some(time) = item
            .display_time
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("  时间：{time}"));
        }
    }
    lines
}

pub(super) fn success_markdown_lines(title: &str, item: Option<&ReceiptItem>) -> Vec<String> {
    success_lines(&format!("# {title}"), item)
}

pub(super) fn success_items_lines(title: &str, items: &[ReceiptItem]) -> Vec<String> {
    let mut lines = vec![if items.len() > 1 {
        format!("{title} · {} 条", items.len())
    } else {
        title.to_owned()
    }];
    for item in items {
        lines.push(format!("- {}", item.title));
        if let Some(time) = item
            .display_time
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("  时间：{time}"));
        }
    }
    lines
}

pub(super) fn success_items_markdown_lines(title: &str, items: &[ReceiptItem]) -> Vec<String> {
    let mut lines = success_items_lines(title, items);
    if let Some(first) = lines.first_mut() {
        *first = format!("# {first}");
    }
    lines
}

pub(super) fn success_count_lines(
    title: &str,
    count: usize,
    unit: &str,
    field: &str,
    output: &Value,
) -> Vec<String> {
    let mut lines = vec![format!("{title} · {count}{unit}")];
    if let Some(items) = output.get(field).and_then(Value::as_array) {
        for item in items
            .iter()
            .filter_map(|value| item_from_value(Some(value)))
        {
            lines.push(format!("- {}", item.title));
        }
    }
    lines
}

pub(super) fn success_count_markdown_lines(
    title: &str,
    count: usize,
    unit: &str,
    field: &str,
    output: &Value,
) -> Vec<String> {
    let mut lines = success_count_lines(title, count, unit, field, output);
    if let Some(first) = lines.first_mut() {
        *first = format!("# {first}");
    }
    lines
}

#[derive(Debug, Clone)]
pub(super) struct ReceiptItem {
    pub(super) title: String,
    display_time: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct TodoDetailCardItem {
    title: String,
    detail: Option<String>,
    due_date: Option<String>,
    due_at: Option<String>,
    reminder_at: Option<String>,
    recurrence_kind: TodoRecurrenceKind,
    recurrence_interval_days: u32,
    recurrence_interval: u32,
    recurrence_unit: TodoRecurrenceUnit,
    status: Option<String>,
    next_reminder_at: Option<String>,
    completed_at: Option<String>,
}

pub(super) fn item_from_value(value: Option<&Value>) -> Option<ReceiptItem> {
    let value = value?;
    let title = string_field(value, "title")?;
    Some(ReceiptItem {
        title: truncate_chars(&title, 80),
        display_time: string_field(value, "display_time"),
    })
}

pub(super) fn receipt_items_from_array(output: &Value, key: &str) -> Option<Vec<ReceiptItem>> {
    let items = output
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(|value| item_from_value(Some(value)))
        .collect::<Vec<_>>();
    (!items.is_empty()).then_some(items)
}

pub(super) fn todo_detail_card_item_from_value(
    value: Option<&Value>,
) -> Option<TodoDetailCardItem> {
    let value = value?;
    let title = string_field(value, "title")?;
    Some(TodoDetailCardItem {
        title: truncate_chars(&title, 120),
        detail: string_field(value, "detail").map(|value| truncate_chars(&value, 300)),
        due_date: string_field(value, "due_date"),
        due_at: string_field(value, "due_at"),
        reminder_at: string_field(value, "reminder_at"),
        recurrence_kind: recurrence_kind_field(value, "recurrence_kind")
            .unwrap_or(TodoRecurrenceKind::None),
        recurrence_interval_days: positive_u32_field(value, "recurrence_interval_days")
            .unwrap_or_default(),
        recurrence_interval: positive_u32_field(value, "recurrence_interval").unwrap_or_default(),
        recurrence_unit: recurrence_unit_field(value, "recurrence_unit")
            .unwrap_or(TodoRecurrenceUnit::Day),
        status: string_field(value, "status"),
        next_reminder_at: string_field(value, "next_reminder_at"),
        completed_at: string_field(value, "completed_at"),
    })
}

pub(super) fn todo_detail_card_items_from_array(
    output: &Value,
    key: &str,
) -> Option<Vec<TodoDetailCardItem>> {
    let items = output
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(|value| todo_detail_card_item_from_value(Some(value)))
        .collect::<Vec<_>>();
    (!items.is_empty()).then_some(items)
}

pub(super) fn todo_detail_card_body(title: &str, item: &TodoDetailCardItem) -> CommandBody {
    todo_detail_cards_body(title, std::slice::from_ref(item))
}

pub(super) fn todo_detail_cards_body(title: &str, items: &[TodoDetailCardItem]) -> CommandBody {
    let render_items = items
        .iter()
        .cloned()
        .map(todo_render_item_from_detail_card)
        .collect::<Vec<_>>();
    let body = format_todo_cards(
        title,
        &render_items,
        TodoCardOptions {
            reminder_mode: ReminderFieldMode::Current,
            show_next_reminder: true,
        },
    );
    CommandBody::dual(body.text, body.markdown)
}

pub(super) fn append_todo_detail_card_lines(
    lines: &mut Vec<String>,
    item: &TodoDetailCardItem,
    markdown: bool,
    index: usize,
    numbered: bool,
) {
    let mut render_item = todo_render_item_from_detail_card(item.clone());
    if numbered {
        render_item.title = format!("{}. {}", index + 1, render_item.title);
    }
    let body = format_todo_cards(
        "__card__",
        &[render_item],
        TodoCardOptions {
            reminder_mode: ReminderFieldMode::Current,
            show_next_reminder: true,
        },
    );
    let rendered = if markdown { body.markdown } else { body.text };
    let mut parts = rendered.lines();
    let _ = parts.next();
    lines.extend(parts.map(str::to_owned));
}

pub(super) fn recurrence_kind_field(value: &Value, key: &str) -> Option<TodoRecurrenceKind> {
    match value.get(key).and_then(Value::as_str) {
        Some("none") => Some(TodoRecurrenceKind::None),
        Some("daily") => Some(TodoRecurrenceKind::Daily),
        Some("every_n_days") => Some(TodoRecurrenceKind::EveryNDays),
        Some("weekly") => Some(TodoRecurrenceKind::Weekly),
        Some("every_n_weeks") => Some(TodoRecurrenceKind::EveryNWeeks),
        Some("monthly") => Some(TodoRecurrenceKind::Monthly),
        Some("every_n_months") => Some(TodoRecurrenceKind::EveryNMonths),
        Some("yearly") => Some(TodoRecurrenceKind::Yearly),
        Some("every_n_years") => Some(TodoRecurrenceKind::EveryNYears),
        _ => None,
    }
}

pub(super) fn recurrence_unit_field(value: &Value, key: &str) -> Option<TodoRecurrenceUnit> {
    match value.get(key).and_then(Value::as_str) {
        Some("day") => Some(TodoRecurrenceUnit::Day),
        Some("week") => Some(TodoRecurrenceUnit::Week),
        Some("month") => Some(TodoRecurrenceUnit::Month),
        Some("year") => Some(TodoRecurrenceUnit::Year),
        _ => None,
    }
}

pub(super) fn positive_u32_field(value: &Value, key: &str) -> Option<u32> {
    value.get(key).and_then(Value::as_u64)?.try_into().ok()
}

pub(super) fn todo_render_item_from_detail_card(item: TodoDetailCardItem) -> TodoRenderItem {
    TodoRenderItem {
        title: item.title,
        detail: item.detail,
        due_date: item.due_date,
        due_at: item.due_at,
        reminder_at: item.reminder_at,
        recurrence_kind: item.recurrence_kind,
        recurrence_interval_days: item.recurrence_interval_days,
        recurrence_interval: item.recurrence_interval,
        recurrence_unit: item.recurrence_unit,
        status: item.status,
        next_reminder_at: item.next_reminder_at,
        completed_at: item.completed_at,
    }
}

pub(super) fn error_reply_for_tool_result(output: &Value) -> String {
    let code = structured_error_code(output);
    match code.as_deref() {
        Some("todo_visible_numbers_unavailable") => {
            "没有可用的最近待办编号。请先查看对应待办列表，再按编号操作。".to_owned()
        }
        Some("todo_reference_unavailable") => {
            "找不到“刚才那条”待办。请先查看列表或明确说明要操作哪一条。".to_owned()
        }
        Some("todo_reference_invalid_state") => {
            "目标待办当前状态不允许执行这次操作。请查看最新列表后再试。".to_owned()
        }
        Some("todo_selection_not_found") => {
            "没有找到符合条件的待办，或可见编号已经失效。请查看最新列表后再操作。".to_owned()
        }
        Some("todo_delete_invalid_state") => {
            "目标待办当前无法永久删除，请查看最新列表后再试。".to_owned()
        }
        Some("todo_delete_mixed_status") => {
            "这次永久删除没有成功，请查看最新列表后再试。".to_owned()
        }
        Some("todo_pending_exists") | Some("todo_pending_conflict") => {
            "当前已有待确认的待办操作，请先回复“确认”或“取消”。".to_owned()
        }
        _ => string_field(output, "message")
            .or_else(|| {
                output
                    .get("error")
                    .and_then(|error| string_field(error, "message"))
            })
            .unwrap_or_else(|| "这次待办操作没有成功，没有修改待办。".to_owned()),
    }
}

pub(super) fn skip_reply_for_tool_result(output: &Value) -> String {
    match string_field(output, "reason").as_deref() {
        Some("dependency_previous_call_failed") => {
            "前序工具没有成功，本次待办操作已跳过，数据库未因此继续修改。".to_owned()
        }
        Some(reason) => format!("本次待办操作已跳过：{reason}。"),
        None => "本次待办操作已跳过，数据库未因此继续修改。".to_owned(),
    }
}

pub(super) fn structured_error_code(output: &Value) -> Option<String> {
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

pub(super) fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(super) fn status_label(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "进行中待办",
        TodoStatus::Completed => "已完成待办",
    }
}
