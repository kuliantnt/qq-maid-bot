//! Todo 管理 API 使用的领域门面。
//!
//! 管理员 actor 只在 HTTP 公共层完成认证、审计和限流；本模块始终从 Todo 记录或
//! 服务端已知目标恢复真实 owner/scope，不把管理员身份写入 Todo。

use qq_maid_common::time_context::request_time_context;
use sha2::{Digest, Sha256};

use crate::{
    identity::{
        group_raw_target_from_scope_key, parse_stable_scope_key, private_raw_target_from_scope_key,
    },
    runtime::push::{ONEBOT11_PLATFORM, QQ_OFFICIAL_PLATFORM},
    storage::notification::NotificationOutboxStore,
};

use super::{
    TodoEditPatch, TodoError, TodoItem, TodoItemDraft, TodoOwner, TodoQuery, TodoStatus, TodoStore,
    edit_patch,
    reminder::{prepare_reminder_upsert, validate_draft_reminder},
    storage::{
        TodoManagementPage, TodoManagementRecord, TodoManagementScopeType,
        TodoManagementTargetCandidateFilter, TodoManagementTargetFilter, advance_after_completion,
        is_recurring, normalize_draft,
    },
};

const TARGET_REF_PREFIX: &str = "todo_target:v1:";
const TARGET_RESOLUTION_PAGE_SIZE: usize = 100;

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

#[derive(Debug, Clone, Default)]
pub(crate) struct TodoManagementListFilter {
    pub(crate) platform: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) scope_type: Option<TodoManagementScopeType>,
    pub(crate) user_id: Option<String>,
    pub(crate) target_ref: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TodoManagementTargetListFilter {
    pub(crate) platform: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) scope_type: Option<TodoManagementScopeType>,
    pub(crate) user_id: Option<String>,
    pub(crate) group_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct TodoManagementTarget {
    pub(crate) target_ref: Option<String>,
    pub(crate) platform: String,
    pub(crate) scope_type: String,
    pub(crate) user_id: Option<String>,
    pub(crate) group_id: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) reminder_supported: bool,
    pub(crate) diagnostic: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct TodoManagementItem {
    pub(crate) item: TodoItem,
    pub(crate) target: TodoManagementTarget,
}

#[derive(Debug, Clone)]
pub(crate) struct TodoManagementListPage {
    pub(crate) items: Vec<TodoManagementItem>,
    pub(crate) total_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct TodoManagementTargetListPage {
    pub(crate) items: Vec<TodoManagementTarget>,
    pub(crate) total_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) enum TodoManagementError {
    Domain(TodoError),
    NotFound,
    Conflict(String),
    InvalidTarget(String),
    Notification(String),
}

impl TodoManagementError {
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::Domain(error) => error.code(),
            Self::NotFound => "not_found",
            Self::Conflict(_) => "conflict",
            Self::InvalidTarget(_) => "bad_request",
            Self::Notification(_) => "notification_error",
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Domain(error) => error.message(),
            Self::NotFound => "todo not found",
            Self::Conflict(message)
            | Self::InvalidTarget(message)
            | Self::Notification(message) => message,
        }
    }
}

impl From<TodoError> for TodoManagementError {
    fn from(value: TodoError) -> Self {
        match value.code() {
            "not_found" => Self::NotFound,
            "conflict" => Self::Conflict(value.message().to_owned()),
            "notification_error" => Self::Notification(value.message().to_owned()),
            _ => Self::Domain(value),
        }
    }
}

/// Todo 管理场景的领域 Service；未来 Memory API 应实现自己的独立权限模型。
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
        target_ref: &str,
        draft: TodoItemDraft,
    ) -> Result<TodoManagementItem, TodoManagementError> {
        self.ensure_shared_database()?;
        let owner = self.resolve_target_ref(target_ref)?;
        let draft = normalize_draft(draft)?;
        self.validate_reminder(&owner, &draft)?;
        let created = self
            .store
            .create_todo_for_management(&owner, draft, reminder_upsert)?;
        Ok(self.present(created))
    }

    pub(crate) fn list(
        &self,
        query: &TodoQuery,
        filter: TodoManagementListFilter,
    ) -> Result<TodoManagementListPage, TodoManagementError> {
        let target = filter
            .target_ref
            .as_deref()
            .map(|target_ref| self.resolve_target_ref(target_ref))
            .transpose()?;
        let page = self.store.query_todos_for_management(
            query,
            &TodoManagementTargetFilter {
                platform: filter.platform,
                account_id: filter.account_id,
                scope_type: filter.scope_type,
                user_id: filter.user_id,
                target,
            },
        )?;
        Ok(self.present_page(page))
    }

    pub(crate) fn get(&self, id: &str) -> Result<TodoManagementItem, TodoManagementError> {
        let record = self.get_record(id)?;
        Ok(self.present(record))
    }

    /// 分页发现服务端已知且能完整恢复的创建目标，不暴露内部 owner/scope。
    pub(crate) fn targets(
        &self,
        filter: TodoManagementTargetListFilter,
        limit: usize,
        offset: usize,
    ) -> Result<TodoManagementTargetListPage, TodoManagementError> {
        let page = self.store.management_target_candidates_page(
            &TodoManagementTargetCandidateFilter {
                platform: filter.platform,
                account_id: filter.account_id,
                scope_type: filter.scope_type,
                user_id: filter.user_id,
                group_id: filter.group_id,
            },
            limit,
            offset,
        )?;
        let items = page
            .items
            .into_iter()
            .filter_map(|owner| {
                let target = target_from_owner(&owner);
                target.target_ref.is_some().then_some(target)
            })
            .collect();
        Ok(TodoManagementTargetListPage {
            items,
            total_count: page.total_count,
        })
    }

    pub(crate) fn update(
        &self,
        id: &str,
        update: TodoManagementUpdate,
    ) -> Result<TodoManagementItem, TodoManagementError> {
        self.ensure_shared_database()?;
        let current = self.get_record(id)?;
        if !update.has_field_changes() && update.status.as_ref() == Some(&current.item.status) {
            return Ok(self.present(current));
        }
        if let Some(reminder_at) = update
            .fields
            .reminder_at
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            // 显式新值始终按请求值校验，即使同一请求还会完成 Todo 或推进周期；
            // 不能让状态转换掩盖“用户本次提交了过去时间”。
            let mut reminder_draft = TodoItemDraft::from_item(
                &current.item,
                current.item.raw_text.clone().unwrap_or_default(),
            );
            reminder_draft.reminder_at = Some(reminder_at.to_owned());
            self.validate_reminder(&current.owner, &reminder_draft)?;
        }
        let (draft, final_status) = plan_update(&current, &update)?;
        let updated =
            self.store
                .update_todo_for_management(&current, draft, final_status, |record| {
                    reminder_upsert(record)
                })?;
        Ok(self.present(updated))
    }

    pub(crate) fn delete(&self, id: &str) -> Result<(), TodoManagementError> {
        self.ensure_shared_database()?;
        let current = self.get_record(id)?;
        self.store.delete_todo_for_management(&current)?;
        Ok(())
    }

    fn get_record(&self, id: &str) -> Result<TodoManagementRecord, TodoManagementError> {
        self.store
            .get_todo_for_management(id)?
            .ok_or(TodoManagementError::NotFound)
    }

    fn resolve_target_ref(&self, target_ref: &str) -> Result<TodoOwner, TodoManagementError> {
        validate_target_ref_format(target_ref)?;
        let mut offset = 0;
        let owner = loop {
            let page = self.store.management_target_candidates_page(
                &TodoManagementTargetCandidateFilter::default(),
                TARGET_RESOLUTION_PAGE_SIZE,
                offset,
            )?;
            if let Some(owner) = page
                .items
                .iter()
                .find(|owner| target_ref_for(owner) == target_ref)
                .cloned()
            {
                break owner;
            }
            offset = offset.saturating_add(page.limit);
            if offset >= page.total_count || page.items.is_empty() {
                return Err(TodoManagementError::InvalidTarget(
                    "target_ref is unknown or no longer references a known conversation".to_owned(),
                ));
            }
        };
        let target = target_from_owner(&owner);
        if target.target_ref.as_deref() != Some(target_ref) {
            return Err(TodoManagementError::InvalidTarget(
                "target_ref does not identify a complete platform target".to_owned(),
            ));
        }
        Ok(owner)
    }

    fn validate_reminder(
        &self,
        owner: &TodoOwner,
        draft: &TodoItemDraft,
    ) -> Result<(), TodoManagementError> {
        if draft.reminder_at.is_none() {
            return Ok(());
        }
        validate_draft_reminder(draft).map_err(TodoManagementError::InvalidTarget)?;
        let target = target_from_owner(owner);
        if target.target_ref.is_none() {
            return Err(TodoManagementError::InvalidTarget(
                "todo target cannot be restored from its owner/scope".to_owned(),
            ));
        }
        if !target.reminder_supported {
            return Err(TodoManagementError::InvalidTarget(format!(
                "platform `{}` does not support proactive Todo reminders",
                target.platform
            )));
        }
        Ok(())
    }

    fn ensure_shared_database(&self) -> Result<(), TodoManagementError> {
        if self.store.database_path() == self.notification_store.database_path() {
            Ok(())
        } else {
            Err(TodoManagementError::Notification(
                "Todo and Notification Outbox must use the same SQLite database".to_owned(),
            ))
        }
    }

    fn present(&self, record: TodoManagementRecord) -> TodoManagementItem {
        TodoManagementItem {
            target: target_from_owner(&record.owner),
            item: record.item,
        }
    }

    fn present_page(&self, page: TodoManagementPage) -> TodoManagementListPage {
        TodoManagementListPage {
            items: page
                .items
                .into_iter()
                .map(|record| self.present(record))
                .collect(),
            total_count: page.total_count,
        }
    }
}

fn plan_update(
    current: &TodoManagementRecord,
    update: &TodoManagementUpdate,
) -> Result<(TodoItemDraft, TodoStatus), TodoManagementError> {
    let has_fields = update.has_field_changes();
    if current.item.status == TodoStatus::Completed
        && has_fields
        && update.status != Some(TodoStatus::Pending)
    {
        return Err(TodoManagementError::Conflict(
            "completed todo fields cannot be edited before restoring it".to_owned(),
        ));
    }

    let raw_text = current
        .item
        .raw_text
        .clone()
        .unwrap_or_else(|| current.item.title.clone());
    let base = TodoItemDraft::from_item(&current.item, &raw_text);
    let mut draft = if has_fields {
        edit_patch::apply_to_draft(base, &update.fields, &raw_text)
    } else {
        base
    };
    draft = normalize_draft(draft)?;

    let requested_status = update
        .status
        .clone()
        .unwrap_or_else(|| current.item.status.clone());
    if current.item.status == TodoStatus::Pending && requested_status == TodoStatus::Completed {
        let pending = record_from_draft(
            &current.owner,
            &current.item.id,
            &draft,
            TodoStatus::Pending,
        );
        if is_recurring(&pending.item) {
            draft = normalize_draft(advance_after_completion(&pending.item)?)?;
            return Ok((draft, TodoStatus::Pending));
        }
    }
    Ok((draft, requested_status))
}

fn record_from_draft(
    owner: &TodoOwner,
    id: &str,
    draft: &TodoItemDraft,
    status: TodoStatus,
) -> TodoManagementRecord {
    let now = request_time_context().current_time().to_owned();
    TodoManagementRecord {
        owner: owner.clone(),
        item: TodoItem {
            id: id.to_owned(),
            user_id: owner.user_id.clone(),
            scope_key: owner.scope_key.clone(),
            title: draft.title.clone(),
            detail: draft.detail.clone(),
            raw_text: draft.raw_text.clone(),
            due_date: draft.due_date.clone(),
            due_at: draft.due_at.clone(),
            reminder_at: draft.reminder_at.clone(),
            time_precision: draft.time_precision,
            recurrence_kind: draft.recurrence_kind.clone(),
            recurrence_interval_days: draft.recurrence_interval_days,
            recurrence_interval: draft.recurrence_interval,
            recurrence_unit: draft.recurrence_unit,
            status,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        },
    }
}

fn reminder_upsert(
    record: &TodoManagementRecord,
) -> Result<Option<crate::storage::notification::NotificationUpsert>, TodoError> {
    prepare_reminder_upsert(&record.owner, &record.item).map_err(TodoError::notification)
}

fn target_from_owner(owner: &TodoOwner) -> TodoManagementTarget {
    if TodoStore::owner(owner.user_id.as_deref(), &owner.scope_key).key != owner.key {
        return unknown_target(owner, "owner_scope_mismatch");
    }
    let (platform, account_id) = parse_stable_scope_key(&owner.scope_key)
        .map(|parsed| {
            (
                parsed.platform.to_owned(),
                (parsed.account_id != "-").then(|| parsed.account_id.to_owned()),
            )
        })
        .unwrap_or_else(|| (QQ_OFFICIAL_PLATFORM.to_owned(), None));

    if let Some(group_id) = group_raw_target_from_scope_key(&owner.scope_key) {
        return known_target(owner, platform, account_id, "group", None, Some(group_id));
    }
    if let Some(private_id) = private_raw_target_from_scope_key(&owner.scope_key) {
        let user_id = owner.user_id.clone();
        if user_id.as_deref().map(str::trim) != Some(private_id.as_str()) {
            return unknown_target(owner, "private_owner_target_mismatch");
        }
        return known_target(owner, platform, account_id, "private", user_id, None);
    }
    unknown_target(owner, "unrecognized_scope")
}

fn known_target(
    owner: &TodoOwner,
    platform: String,
    account_id: Option<String>,
    scope_type: &str,
    user_id: Option<String>,
    group_id: Option<String>,
) -> TodoManagementTarget {
    let reminder_supported = match platform.as_str() {
        QQ_OFFICIAL_PLATFORM => true,
        "onebot" | ONEBOT11_PLATFORM => account_id.is_some(),
        _ => false,
    };
    TodoManagementTarget {
        target_ref: Some(target_ref_for(owner)),
        platform,
        scope_type: scope_type.to_owned(),
        user_id: user_id.or_else(|| owner.user_id.clone()),
        group_id,
        account_id,
        reminder_supported,
        diagnostic: None,
    }
}

fn unknown_target(owner: &TodoOwner, diagnostic: &str) -> TodoManagementTarget {
    TodoManagementTarget {
        target_ref: None,
        platform: parse_stable_scope_key(&owner.scope_key)
            .map(|parsed| parsed.platform.to_owned())
            .unwrap_or_else(|| "unknown".to_owned()),
        scope_type: "unknown".to_owned(),
        user_id: owner.user_id.clone(),
        group_id: None,
        account_id: None,
        reminder_supported: false,
        diagnostic: Some(diagnostic.to_owned()),
    }
}

fn target_ref_for(owner: &TodoOwner) -> String {
    let mut digest = Sha256::new();
    digest.update(b"qq-maid-todo-target-v1\0");
    digest.update(owner.key.as_bytes());
    digest.update(b"\0");
    digest.update(owner.user_id.as_deref().unwrap_or_default().as_bytes());
    digest.update(b"\0");
    digest.update(owner.scope_key.as_bytes());
    let bytes = digest.finalize();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    format!("{TARGET_REF_PREFIX}{encoded}")
}

fn validate_target_ref_format(target_ref: &str) -> Result<(), TodoManagementError> {
    let digest = target_ref
        .trim()
        .strip_prefix(TARGET_REF_PREFIX)
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
    if digest.is_none() {
        return Err(TodoManagementError::InvalidTarget(
            "target_ref format is invalid".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn management_private_scope_type() -> TodoManagementScopeType {
    TodoManagementScopeType::Private
}

pub(crate) fn management_group_scope_type() -> TodoManagementScopeType {
    TodoManagementScopeType::Group
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::conversation_scope_key,
        runtime::tools::todo::{TodoRecurrenceKind, TodoRecurrenceUnit, TodoTimePrecision},
        storage::{APP_MIGRATIONS, database::SqliteDatabase},
    };

    fn service() -> (TodoManagementService, TodoStore, NotificationOutboxStore) {
        let database =
            SqliteDatabase::open_temp("todo-management-service", APP_MIGRATIONS).unwrap();
        let store = TodoStore::new(database.clone());
        let notifications = NotificationOutboxStore::new(database);
        (
            TodoManagementService::new(store.clone(), notifications.clone()),
            store,
            notifications,
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
    fn management_service_reads_and_updates_global_real_owner() {
        let (service, store, _) = service();
        let scope = conversation_scope_key("qq_official", Some("app-1"), "private", "u1");
        let owner = TodoStore::owner(Some("u1"), &scope);
        let created = store.create(&owner, draft("原始标题")).unwrap();

        let listed = service
            .list(
                &TodoQuery {
                    status: super::super::TodoQueryStatus::All,
                    ..TodoQuery::default()
                },
                TodoManagementListFilter::default(),
            )
            .unwrap();
        assert_eq!(listed.total_count, 1);
        assert_eq!(listed.items[0].item.id, created.id);

        let updated = service
            .update(
                &created.id,
                TodoManagementUpdate {
                    fields: TodoEditPatch {
                        title: Some("更新标题".to_owned()),
                        ..Default::default()
                    },
                    status: None,
                },
            )
            .unwrap();
        assert_eq!(updated.item.title, "更新标题");
        assert_eq!(
            store.get_by_id(&owner, &created.id).unwrap().unwrap().title,
            "更新标题"
        );
    }

    #[test]
    fn invalid_patch_after_restore_does_not_partially_change_completed_todo() {
        let (service, store, _) = service();
        let scope = conversation_scope_key("qq_official", Some("app-1"), "private", "u1");
        let owner = TodoStore::owner(Some("u1"), &scope);
        let created = store.create(&owner, draft("原始标题")).unwrap();
        store.complete(&owner, &created.id).unwrap();

        let result = service.update(
            &created.id,
            TodoManagementUpdate {
                fields: TodoEditPatch {
                    title: Some("   ".to_owned()),
                    ..Default::default()
                },
                status: Some(TodoStatus::Pending),
            },
        );
        assert!(matches!(result, Err(TodoManagementError::Domain(_))));
        let unchanged = store.get_by_id(&owner, &created.id).unwrap().unwrap();
        assert_eq!(unchanged.status, TodoStatus::Completed);
        assert_eq!(unchanged.title, "原始标题");
    }

    #[test]
    fn notification_failure_rolls_back_todo_update() {
        let database =
            SqliteDatabase::open_temp("todo-management-rollback", APP_MIGRATIONS).unwrap();
        let store = TodoStore::new(database.clone());
        let service = TodoManagementService::new(
            store.clone(),
            NotificationOutboxStore::new(database.clone()),
        );
        let scope = conversation_scope_key("qq_official", Some("app-1"), "private", "u1");
        let owner = TodoStore::owner(Some("u1"), &scope);
        let created = store.create(&owner, draft("原始标题")).unwrap();
        database
            .connection()
            .unwrap()
            .execute("DROP TABLE notification_outbox", [])
            .unwrap();

        let result = service.update(
            &created.id,
            TodoManagementUpdate {
                fields: TodoEditPatch {
                    title: Some("不应落库".to_owned()),
                    ..Default::default()
                },
                status: None,
            },
        );
        assert!(matches!(result, Err(TodoManagementError::Notification(_))));
        assert_eq!(
            store.get_by_id(&owner, &created.id).unwrap().unwrap().title,
            "原始标题"
        );
    }
}
