//! Todo 管理 API 使用的领域门面。
//!
//! 本模块只复用 Todo 现有草稿归一、状态转换、提醒同步和 Repository；它不理解
//! HTTP 状态码、cookie、JSON envelope，也不抽象其他领域的 CRUD。

use crate::storage::notification::NotificationOutboxStore;

use super::{
    TodoEditPatch, TodoError, TodoItem, TodoItemDraft, TodoOwner, TodoQuery, TodoQueryPage,
    TodoStatus, TodoStore, cancel_reminder_task, edit_patch, sync_reminder_task,
};

const MANAGEMENT_SCOPE_PREFIX: &str = "management:";

#[derive(Debug, Clone)]
pub(crate) struct TodoManagementUpdate {
    pub(crate) fields: TodoEditPatch,
    pub(crate) status: Option<TodoStatus>,
}

impl TodoManagementUpdate {
    pub(crate) fn has_field_changes(&self) -> bool {
        let fields = &self.fields;
        fields.title.is_some()
            || fields.detail.is_some()
            || fields.due_date.is_some()
            || fields.due_at.is_some()
            || fields.reminder_at.is_some()
            || fields.time_precision.is_some()
            || fields.recurrence_kind.is_some()
            || fields.recurrence_interval_days.is_some()
            || fields.recurrence_interval.is_some()
            || fields.recurrence_unit.is_some()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum TodoManagementError {
    Domain(TodoError),
    NotFound,
    Conflict(String),
    Notification(String),
    InvalidActor,
}

impl TodoManagementError {
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::Domain(error) => error.code(),
            Self::NotFound => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Notification(_) => "notification_error",
            Self::InvalidActor => "permission_denied",
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Domain(error) => error.message(),
            Self::NotFound => "todo not found",
            Self::Conflict(message) | Self::Notification(message) => message,
            Self::InvalidActor => "authenticated API actor is invalid",
        }
    }
}

impl From<TodoError> for TodoManagementError {
    fn from(value: TodoError) -> Self {
        Self::Domain(value)
    }
}

/// Todo 管理场景的领域 Service；未来 Memory API 应实现自己的独立 Service。
#[derive(Clone)]
pub(crate) struct TodoManagementService {
    store: TodoStore,
    notification_store: NotificationOutboxStore,
}

impl TodoManagementService {
    pub(crate) fn new(store: TodoStore, notification_store: NotificationOutboxStore) -> Self {
        Self {
            store,
            notification_store,
        }
    }

    pub(crate) fn create(
        &self,
        actor_subject: &str,
        draft: TodoItemDraft,
    ) -> Result<TodoItem, TodoManagementError> {
        let owner = owner_for_actor(actor_subject)?;
        let item = self.store.create(&owner, draft)?;
        self.sync_reminder(&owner, &item)?;
        Ok(item)
    }

    pub(crate) fn list(
        &self,
        actor_subject: &str,
        query: &TodoQuery,
    ) -> Result<TodoQueryPage, TodoManagementError> {
        let owner = owner_for_actor(actor_subject)?;
        self.store
            .query_todos_for_management(&owner, query)
            .map_err(Into::into)
    }

    pub(crate) fn get(
        &self,
        actor_subject: &str,
        id: &str,
    ) -> Result<TodoItem, TodoManagementError> {
        let owner = owner_for_actor(actor_subject)?;
        self.get_for_owner(&owner, id)
    }

    pub(crate) fn update(
        &self,
        actor_subject: &str,
        id: &str,
        update: TodoManagementUpdate,
    ) -> Result<TodoItem, TodoManagementError> {
        let owner = owner_for_actor(actor_subject)?;
        let current = self.get_for_owner(&owner, id)?;
        let has_fields = update.has_field_changes();

        let mut item = match (&current.status, &update.status) {
            (TodoStatus::Completed, Some(TodoStatus::Pending)) => {
                let outcome = self
                    .store
                    .restore_completed_by_ids(&owner, &[id.to_owned()])?;
                outcome.restored.into_iter().next().ok_or_else(|| {
                    TodoManagementError::Conflict("todo status changed".to_owned())
                })?
            }
            (TodoStatus::Pending, Some(TodoStatus::Completed)) => current.clone(),
            (TodoStatus::Completed, _) if has_fields => {
                return Err(TodoManagementError::Conflict(
                    "completed todo fields cannot be edited before restoring it".to_owned(),
                ));
            }
            _ => current.clone(),
        };

        if has_fields {
            let raw_text = item.raw_text.clone().unwrap_or_else(|| item.title.clone());
            let draft = edit_patch::apply_to_draft(
                TodoItemDraft::from_item(&item, &raw_text),
                &update.fields,
                &raw_text,
            );
            item = self.store.edit(&owner, id, draft)?;
        }

        if matches!(current.status, TodoStatus::Pending)
            && matches!(update.status, Some(TodoStatus::Completed))
        {
            let outcome = self
                .store
                .complete_by_ids_with_recurrence(&owner, &[id.to_owned()])?;
            if outcome.completed.is_empty() && outcome.advanced.is_empty() {
                return Err(TodoManagementError::Conflict(
                    "todo status changed".to_owned(),
                ));
            }
            item = self.get_for_owner(&owner, id)?;
        }

        self.sync_reminder(&owner, &item)?;
        Ok(item)
    }

    pub(crate) fn delete(&self, actor_subject: &str, id: &str) -> Result<(), TodoManagementError> {
        let owner = owner_for_actor(actor_subject)?;
        let item = self.get_for_owner(&owner, id)?;
        let outcome = self.store.delete_by_ids(&owner, &[id.to_owned()])?;
        if outcome.deleted_count == 0 {
            return Err(TodoManagementError::NotFound);
        }
        cancel_reminder_task(&self.notification_store, &item)
            .map_err(TodoManagementError::Notification)
    }

    fn get_for_owner(&self, owner: &TodoOwner, id: &str) -> Result<TodoItem, TodoManagementError> {
        self.store
            .get_by_id(owner, id)?
            .ok_or(TodoManagementError::NotFound)
    }

    fn sync_reminder(&self, owner: &TodoOwner, item: &TodoItem) -> Result<(), TodoManagementError> {
        if item.reminder_at.is_none() {
            return cancel_reminder_task(&self.notification_store, item)
                .map_err(TodoManagementError::Notification);
        }
        sync_reminder_task(&self.notification_store, owner, item)
            .map_err(TodoManagementError::Notification)
    }
}

fn owner_for_actor(actor_subject: &str) -> Result<TodoOwner, TodoManagementError> {
    let actor_subject = actor_subject.trim();
    if actor_subject.is_empty() || actor_subject.len() > 128 {
        return Err(TodoManagementError::InvalidActor);
    }
    // 管理 API Todo 与聊天入口 Todo 使用不同 conversation scope；认证 subject 既决定
    // owner 又参与 scope，未来即使出现多个管理员也不会越权读取彼此的数据。
    Ok(TodoStore::owner(
        Some(actor_subject),
        &format!("{MANAGEMENT_SCOPE_PREFIX}{actor_subject}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::tools::todo::{
            TodoRecurrenceKind, TodoRecurrenceUnit, TodoTimePrecision, storage::TODO_MIGRATIONS,
        },
        storage::{
            database::SqliteDatabase,
            notification::{NOTIFICATION_MIGRATIONS, NotificationOutboxStore},
        },
    };

    fn service() -> TodoManagementService {
        let mut migrations = TODO_MIGRATIONS.to_vec();
        migrations.extend_from_slice(NOTIFICATION_MIGRATIONS);
        let database = SqliteDatabase::open_temp("todo-management-service", &migrations).unwrap();
        TodoManagementService::new(
            TodoStore::new(database.clone()),
            NotificationOutboxStore::new(database),
        )
    }

    fn draft(title: &str) -> TodoItemDraft {
        TodoItemDraft {
            title: title.to_owned(),
            detail: None,
            raw_text: None,
            due_date: None,
            due_at: None,
            reminder_at: None,
            time_precision: TodoTimePrecision::None,
            recurrence_kind: TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: TodoRecurrenceUnit::Day,
        }
    }

    #[test]
    fn management_service_isolates_actors_and_supports_crud() {
        let service = service();
        let created = service
            .create("console_admin:1", draft("原始标题"))
            .unwrap();
        assert!(matches!(
            service.get("console_admin:2", &created.id),
            Err(TodoManagementError::NotFound)
        ));
        let other_page = service
            .list(
                "console_admin:2",
                &TodoQuery {
                    status: crate::runtime::tools::todo::TodoQueryStatus::All,
                    ..TodoQuery::default()
                },
            )
            .unwrap();
        assert_eq!(other_page.total_count, 0);
        assert!(matches!(
            service.update(
                "console_admin:2",
                &created.id,
                TodoManagementUpdate {
                    fields: TodoEditPatch {
                        title: Some("越权更新".to_owned()),
                        ..Default::default()
                    },
                    status: None,
                }
            ),
            Err(TodoManagementError::NotFound)
        ));
        assert!(matches!(
            service.delete("console_admin:2", &created.id),
            Err(TodoManagementError::NotFound)
        ));

        let updated = service
            .update(
                "console_admin:1",
                &created.id,
                TodoManagementUpdate {
                    fields: TodoEditPatch {
                        title: Some("更新标题".to_owned()),
                        detail: Some("详情".to_owned()),
                        ..Default::default()
                    },
                    status: None,
                },
            )
            .unwrap();
        assert_eq!(updated.title, "更新标题");
        assert_eq!(updated.detail.as_deref(), Some("详情"));

        service.delete("console_admin:1", &created.id).unwrap();
        assert!(matches!(
            service.get("console_admin:1", &created.id),
            Err(TodoManagementError::NotFound)
        ));
    }

    #[test]
    fn management_update_clears_nullable_fields_and_uses_status_transitions() {
        let service = service();
        let mut initial = draft("待办");
        initial.detail = Some("将清空".to_owned());
        let created = service.create("console_admin:1", initial).unwrap();
        let completed = service
            .update(
                "console_admin:1",
                &created.id,
                TodoManagementUpdate {
                    fields: TodoEditPatch {
                        detail: Some(String::new()),
                        ..Default::default()
                    },
                    status: Some(TodoStatus::Completed),
                },
            )
            .unwrap();
        assert_eq!(completed.detail, None);
        assert_eq!(completed.status, TodoStatus::Completed);

        let restored = service
            .update(
                "console_admin:1",
                &created.id,
                TodoManagementUpdate {
                    fields: TodoEditPatch::default(),
                    status: Some(TodoStatus::Pending),
                },
            )
            .unwrap();
        assert_eq!(restored.status, TodoStatus::Pending);
    }
}
