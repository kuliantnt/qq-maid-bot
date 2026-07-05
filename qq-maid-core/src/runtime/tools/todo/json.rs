//! 面向模型的 Todo Tool JSON 序列化与状态文案。
//!
//! 集中维护 item / draft / 选中结果的 JSON 字段映射与状态字符串，
//! 避免各 Tool 各自手抄字段名导致输出漂移。这里不依赖 session / store。

use serde_json::{Map, Value, json};

use crate::runtime::todo::{TodoItem, TodoStatus, preview_next_reminder_at};

use crate::runtime::todo::status::status_machine_str;

use super::common::TodoSelectionLabel;

/// 列表结果按展示顺序编号成 JSON。
pub(in crate::runtime::tools::todo) fn todo_items_json(items: &[TodoItem]) -> Vec<Value> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| todo_numbered_item_json(index + 1, item))
        .collect()
}

/// 不带编号标签的条目 JSON，供按标题 / 全状态删除等语义选择输出使用。
pub(in crate::runtime::tools::todo) fn todo_plain_item_json(item: &TodoItem) -> Value {
    Value::Object(todo_item_json_object(item))
}

/// 不带编号标签的条目 JSON 列表，避免把 Agent 内部查询顺序误展示成用户可见编号。
pub(in crate::runtime::tools::todo) fn todo_plain_items_json(items: &[TodoItem]) -> Vec<Value> {
    items.iter().map(todo_plain_item_json).collect()
}

/// 选中条目保留 label 信息；complete/restore 结果按编号顺序回填。
pub(in crate::runtime::tools::todo) fn todo_selected_items_json(
    items: &[(TodoSelectionLabel, TodoItem)],
) -> Vec<Value> {
    items
        .iter()
        .map(|(label, item)| todo_selected_item_json(label.clone(), item))
        .collect()
}

fn todo_numbered_item_json(number: usize, item: &TodoItem) -> Value {
    // 仅 json 内部复用，不需要 pub。
    todo_selected_item_json(TodoSelectionLabel::Number(number), item)
}

pub(in crate::runtime::tools::todo) fn todo_selected_item_json(
    label: TodoSelectionLabel,
    item: &TodoItem,
) -> Value {
    let mut object = todo_item_json_object(item);
    match label {
        TodoSelectionLabel::Number(number) => {
            object.insert("visible_number".to_owned(), json!(number));
        }
        TodoSelectionLabel::Reference(reference) => {
            object.insert("reference".to_owned(), json!(reference.as_str()));
        }
    }
    Value::Object(object)
}

fn todo_item_json_object(item: &TodoItem) -> Map<String, Value> {
    use crate::runtime::todo::display_todo_time;
    let mut object = Map::new();
    object.insert("title".to_owned(), json!(item.title));
    object.insert("detail".to_owned(), json!(item.detail));
    object.insert("due_date".to_owned(), json!(item.due_date));
    object.insert("due_at".to_owned(), json!(item.due_at));
    object.insert("reminder_at".to_owned(), json!(item.reminder_at));
    object.insert("recurrence_kind".to_owned(), json!(item.recurrence_kind));
    object.insert(
        "recurrence_interval_days".to_owned(),
        json!(item.recurrence_interval_days),
    );
    object.insert(
        "next_reminder_at".to_owned(),
        json!(preview_next_reminder_at(item).ok().flatten()),
    );
    object.insert("display_time".to_owned(), json!(display_todo_time(item)));
    object.insert("status".to_owned(), json!(status_machine_str(&item.status)));
    object.insert("created_at".to_owned(), json!(item.created_at));
    object.insert("updated_at".to_owned(), json!(item.updated_at));
    object.insert("completed_at".to_owned(), json!(item.completed_at));
    object.insert("cancelled_at".to_owned(), json!(item.cancelled_at));
    object
}

/// 面向用户的中文状态标签，delete_todos 的 source_condition 复用。
pub(in crate::runtime::tools::todo) fn status_label(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "未完成待办",
        TodoStatus::Completed => "已完成待办",
        TodoStatus::Cancelled => "已取消待办",
    }
}
