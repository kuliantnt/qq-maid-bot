//! 面向模型的 Todo Tool JSON 序列化与状态文案。
//!
//! 集中维护 item / draft / 选中结果的 JSON 字段映射与状态字符串，
//! 避免各 Tool 各自手抄字段名导致输出漂移。这里不依赖 session / store。

use serde_json::{Map, Value, json};

use crate::runtime::todo::{TodoItem, TodoItemDraft, TodoStatus};

use crate::runtime::todo::status::{status_machine_str, time_precision_machine_str};

use super::common::TodoSelectionLabel;

/// 列表结果按展示顺序编号成 JSON。
pub(in crate::runtime::tools::todo) fn todo_items_json(items: &[TodoItem]) -> Vec<Value> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| todo_numbered_item_json(index + 1, item))
        .collect()
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
    object.insert("display_time".to_owned(), json!(display_todo_time(item)));
    object.insert("status".to_owned(), json!(status_machine_str(&item.status)));
    object.insert("created_at".to_owned(), json!(item.created_at));
    object.insert("updated_at".to_owned(), json!(item.updated_at));
    object.insert("completed_at".to_owned(), json!(item.completed_at));
    object.insert("cancelled_at".to_owned(), json!(item.cancelled_at));
    object
}

/// 待确认草稿的 JSON 投影，供 create_todo 输出。
pub(in crate::runtime::tools::todo) fn todo_draft_json(draft: &TodoItemDraft) -> Value {
    use crate::runtime::todo::display_draft_time;
    json!({
        "title": draft.title,
        "detail": draft.detail,
        "due_date": draft.due_date,
        "due_at": draft.due_at,
        "display_time": display_draft_time(draft),
        "time_precision": time_precision_machine_str(&draft.time_precision),
    })
}

/// 面向用户的中文状态标签，delete_todos 的 source_condition 复用。
pub(in crate::runtime::tools::todo) fn status_label(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "未完成待办",
        TodoStatus::Completed => "已完成待办",
        TodoStatus::Cancelled => "已取消待办",
    }
}
