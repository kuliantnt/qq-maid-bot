use super::support::*;
use super::*;

#[tokio::test]
async fn complete_tool_selection_text_discrete_deduplicates_numbers() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    for title in ["第一条", "第二条", "第三条"] {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                    recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
    }
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let context = test_context();
    list_tool
        .execute(context.clone(), json!({"status":"pending"}))
        .await
        .unwrap();

    let output = complete_tool
        .execute(
            context,
            json!({"numbers": null, "selection_text": "1,3,3", "reference": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["completed"].as_array().unwrap().len(), 2);
    assert_eq!(todo_store.list_completed(&owner).unwrap().len(), 2);
    assert_eq!(todo_store.list_pending(&owner).unwrap().len(), 1);
}
