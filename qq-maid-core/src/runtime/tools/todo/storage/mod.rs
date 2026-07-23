//! 待办事项（Todo）存储模块。
//!
//! 以项目级 SQLite 存储待办事项列表，支持创建、列表（按状态和条件）、
//! 搜索、编辑、完成等操作。支持中文自然语言日期推断。

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use qq_maid_common::time_context::{
    local_date_from_timestamp, parse_local_datetime_for_comparison,
};
use rusqlite::{OptionalExtension, params};

use crate::{
    identity::{owner_scope_key, parse_stable_scope_key},
    storage::{database::SqliteDatabase, session::now_iso_cn},
};

// 拆分出的纯 helper 子模块：均不改变 schema 与对外 API。
mod delete;
mod id;
mod normalize;
mod query;
mod query_model;
mod recurrence;
mod schema;
mod search;
mod sort;
mod time;
mod types;
mod write;

pub(crate) use query_model::validate_todo_query;

// 时间相关 helper 是 Todo 存储 API 的一部分，经由 runtime::tools::todo 统一导出。
pub(crate) use recurrence::{
    TodoRecurrenceRule, recurrence_kind_for_rule, recurrence_rule_from_interval_unit,
};
pub use recurrence::{apply_recurrence_patch_to_draft, preview_next_reminder_at, recurrence_label};
pub use schema::{
    TODO_DAILY_REMINDER_PREF_SCHEMA_V5, TODO_RECURRENCE_RULE_SCHEMA_V4, TODO_RECURRENCE_SCHEMA_V3,
    TODO_REMINDER_SCHEMA_V2, TODO_SCHEMA_V1,
};
pub use time::{display_todo_time, enrich_draft_time_from_text};
pub use types::*;

#[cfg(test)]
pub use schema::TODO_MIGRATIONS;
#[cfg(test)]
pub use time::infer_due_date_from_text;

#[cfg(test)]
mod group_admin_tests;

use self::id::{
    clean_todo_id, parse_required_todo_db_id, parse_todo_db_id, private_target_from_scope_key,
};
use normalize::normalize_draft;
#[cfg(test)]
use query::query_private_pending_owner_scopes;
use query::{
    get_by_id_any_unlocked, get_by_id_status_unlocked, get_by_id_unlocked, query_items,
    query_items_by_owner_scopes_and_status, query_items_by_status,
    query_private_daily_reminder_owner_scopes,
};
use search::search_score;
use sort::{
    compare_todo_order, sort_completed_todos, sort_completed_todos_desc, sort_todo_all_board,
    sort_todos, sort_todos_by_created_desc,
};
use write::{complete_pending_unlocked, insert_todo_unlocked, update_pending_todo_unlocked};

impl TodoStore {
    /// 创建一个新的 TodoStore，复用应用级 SQLite 句柄。
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    /// 构造所有者标识。
    ///
    /// 新跨平台 scope 下 owner_key 纳入会话 scope 与 actor，避免跨平台同名用户串号；
    /// 旧 `private:` / `group:` scope 只保留给测试和未归一的受控入口，不在运行时做别名查询。
    pub fn owner(user_id: Option<&str>, scope_key: &str) -> TodoOwner {
        let user_id = user_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let scope_key = clean_optional(scope_key).unwrap_or_else(|| "unknown".to_owned());
        let key = if parse_stable_scope_key(&scope_key).is_some() {
            owner_scope_key(user_id.as_deref(), &scope_key)
        } else {
            user_id.clone().unwrap_or_else(|| scope_key.clone())
        };
        TodoOwner {
            key,
            user_id,
            scope_key,
        }
    }

    /// 创建一条待办事项，自动分配 ID。
    pub fn create(&self, owner: &TodoOwner, draft: TodoItemDraft) -> Result<TodoItem, TodoError> {
        let draft = normalize_draft(draft)?;
        let now = now_iso_cn();
        let conn = self.connection()?;
        insert_todo_unlocked(&conn, owner, draft, &now)
    }

    /// 批量创建待办事项，整批在同一事务中提交。
    ///
    /// Tool Loop 可一次提交多条创建意图；任一条规范化或 SQLite 写入失败时，
    /// 已经插入的同批记录必须随事务回滚，避免用户看到失败但数据库留下半批结果。
    pub fn create_many(
        &self,
        owner: &TodoOwner,
        drafts: Vec<TodoItemDraft>,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut created = Vec::with_capacity(drafts.len());

        for draft in drafts {
            let draft = normalize_draft(draft)?;
            created.push(insert_todo_unlocked(&tx, owner, draft, &now)?);
        }

        tx.commit().map_err(TodoError::from_sql)?;
        Ok(created)
    }

    /// 列出所有待处理（Pending）的待办事项，按截止时间排序。
    pub fn list_pending(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, TodoStatus::Pending)?;
        sort_todos(&mut items);
        Ok(items)
    }

    /// 按计划日期列出指定状态的待办，日期按请求本地自然日解释。
    pub fn list_by_due_date(
        &self,
        owner: &TodoOwner,
        status: TodoStatus,
        due_date: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, status.clone())?
            .into_iter()
            .filter(|item| todo_due_matches_local_date(item, due_date))
            .collect::<Vec<_>>();
        sort_items_for_status(&mut items, &status);
        Ok(items)
    }

    /// 按计划日期闭区间列出指定状态的待办，日期按请求本地自然日解释。
    pub fn list_by_due_date_range(
        &self,
        owner: &TodoOwner,
        status: TodoStatus,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        if start > end {
            return Err(TodoError::bad_request(
                "日期范围无效，开始日期不能晚于结束日期。",
            ));
        }
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, status.clone())?
            .into_iter()
            .filter(|item| {
                todo_due_local_date(item).is_some_and(|date| start <= date && date <= end)
            })
            .collect::<Vec<_>>();
        sort_items_for_status(&mut items, &status);
        Ok(items)
    }

    /// 按归一化后的业务时间字段列出指定状态的待办。
    pub fn list_by_date_filter(
        &self,
        owner: &TodoOwner,
        status: TodoStatus,
        filter: TodoListDateFilter,
    ) -> Result<Vec<TodoItem>, TodoError> {
        if filter.start > filter.end {
            return Err(TodoError::bad_request(
                "日期范围无效，开始日期不能晚于结束日期。",
            ));
        }
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, status.clone())?
            .into_iter()
            .filter(|item| todo_list_date_matches_range(item, filter))
            .collect::<Vec<_>>();
        sort_items_for_status(&mut items, &status);
        Ok(items)
    }

    /// 按 owner_key + 一组私聊 scope 读取 pending。
    ///
    /// reminder 需要按 owner 聚合扫描，但同一 owner 可能保留多个历史 private scope；
    /// 这里保持一次 owner 只查一轮 pending，并继续复用既有待办排序语义。
    pub fn list_pending_for_private_scopes(
        &self,
        owner_key: &str,
        private_scope_keys: &[String],
    ) -> Result<Vec<TodoItem>, TodoError> {
        if private_scope_keys.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.connection()?;
        let mut items = query_items_by_owner_scopes_and_status(
            &conn,
            owner_key,
            private_scope_keys,
            TodoStatus::Pending,
        )?;
        sort_todos(&mut items);
        Ok(items)
    }

    /// 查询可验证私聊推送目标的 owner 列表。
    ///
    /// 这里只判断 owner 与 private target 的对应关系是否可靠，不负责日志记录；
    /// 同一 owner 若存在冲突 target 或不可解析 scope，会作为 skipped 结果返回。
    pub fn list_private_reminder_owners(&self) -> Result<TodoReminderOwnerQueryResult, TodoError> {
        let conn = self.connection()?;
        let rows = query_private_daily_reminder_owner_scopes(&conn)?;
        self.build_private_reminder_owner_result(rows)
    }

    /// 列出存在 pending Todo 的可验证私聊 owner，仅供迁移期测试和诊断确认候选来源。
    #[cfg(test)]
    pub fn list_private_pending_reminder_owners(
        &self,
    ) -> Result<TodoReminderOwnerQueryResult, TodoError> {
        let conn = self.connection()?;
        let rows = query_private_pending_owner_scopes(&conn)?;
        self.build_private_reminder_owner_result(rows)
    }

    fn build_private_reminder_owner_result(
        &self,
        rows: Vec<(String, String)>,
    ) -> Result<TodoReminderOwnerQueryResult, TodoError> {
        let mut grouped = BTreeMap::<String, Vec<String>>::new();
        for (owner_key, scope_key) in rows {
            grouped.entry(owner_key).or_default().push(scope_key);
        }

        let mut result = TodoReminderOwnerQueryResult::default();
        for (owner_key, private_scope_keys) in grouped {
            let mut parsed_target_ids = BTreeSet::new();
            let mut has_invalid_scope = false;
            for scope_key in &private_scope_keys {
                let Some(target_id) = private_target_from_scope_key(scope_key) else {
                    has_invalid_scope = true;
                    continue;
                };
                parsed_target_ids.insert(target_id);
            }

            if has_invalid_scope {
                result.skipped.push(TodoReminderSkippedOwner {
                    owner_key,
                    private_scope_keys,
                    parsed_target_ids: parsed_target_ids.into_iter().collect(),
                    reason: TodoReminderOwnerSkipReason::InvalidPrivateScope,
                });
                continue;
            }
            if parsed_target_ids.len() != 1 {
                result.skipped.push(TodoReminderSkippedOwner {
                    owner_key,
                    private_scope_keys,
                    parsed_target_ids: parsed_target_ids.into_iter().collect(),
                    reason: TodoReminderOwnerSkipReason::ConflictingPrivateTargets,
                });
                continue;
            }

            result.candidates.push(TodoReminderOwnerCandidate {
                owner_key,
                private_target_id: parsed_target_ids.into_iter().next().unwrap_or_default(),
                primary_private_scope_key: private_scope_keys.first().cloned().unwrap_or_default(),
                private_scope_keys,
            });
        }
        Ok(result)
    }

    /// 设置当前 owner/scope 的每日待办摘要开关。
    ///
    /// 全局 `TODO_DAILY_REMINDER_ENABLED` 只是后台调度总开关；这里保存用户/范围级偏好。
    pub fn set_daily_reminder_enabled(
        &self,
        owner: &TodoOwner,
        enabled: bool,
    ) -> Result<(), TodoError> {
        let conn = self.connection()?;
        let now = now_iso_cn();
        conn.execute(
            "INSERT INTO todo_daily_reminder_prefs (
                owner_key, scope_key, enabled, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(owner_key, scope_key) DO UPDATE SET
                enabled = excluded.enabled,
                updated_at = excluded.updated_at",
            params![
                owner.key.as_str(),
                owner.scope_key.as_str(),
                if enabled { 1 } else { 0 },
                now.as_str()
            ],
        )
        .map(|_| ())
        .map_err(TodoError::from_sql)
    }

    /// 读取当前 owner/scope 的每日待办摘要开关，未配置时默认关闭。
    pub fn daily_reminder_enabled(&self, owner: &TodoOwner) -> Result<bool, TodoError> {
        let conn = self.connection()?;
        let enabled = conn
            .query_row(
                "SELECT enabled
                 FROM todo_daily_reminder_prefs
                 WHERE owner_key = ?1 AND scope_key = ?2",
                params![owner.key.as_str(), owner.scope_key.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(TodoError::from_sql)?;
        Ok(enabled.unwrap_or(0) != 0)
    }

    /// 列出所有已完成的待办事项，按完成时间降序排列。
    pub fn list_completed(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, TodoStatus::Completed)?;
        sort_completed_todos_desc(&mut items);
        Ok(items)
    }

    /// 列出所有待办事项（不限状态），按创建时间降序排列。
    pub fn list_all(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?;
        sort_todos_by_created_desc(&mut items);
        Ok(items)
    }

    /// 列出 `/todo all` 看板使用的全部待办，按用户可见分组顺序排列。
    pub fn list_all_for_board(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?;
        // 用户面已取消不再作为可见状态，all 看板只展示进行中和已完成。
        sort_todos_by_created_desc(&mut items);
        sort_todo_all_board(&mut items);
        Ok(items)
    }

    /// 按计划日期列出全部状态待办，排序与 `/todo all` 看板保持一致。
    pub fn list_all_by_due_date_for_board(
        &self,
        owner: &TodoOwner,
        due_date: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?
            .into_iter()
            .filter(|item| todo_due_matches_local_date(item, due_date))
            .collect::<Vec<_>>();
        sort_todos_by_created_desc(&mut items);
        sort_todo_all_board(&mut items);
        Ok(items)
    }

    /// 按计划日期闭区间列出全部状态待办，排序与 `/todo all` 看板保持一致。
    pub fn list_all_by_due_date_range_for_board(
        &self,
        owner: &TodoOwner,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        if start > end {
            return Err(TodoError::bad_request(
                "日期范围无效，开始日期不能晚于结束日期。",
            ));
        }
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?
            .into_iter()
            .filter(|item| {
                todo_due_local_date(item).is_some_and(|date| start <= date && date <= end)
            })
            .collect::<Vec<_>>();
        sort_todos_by_created_desc(&mut items);
        sort_todo_all_board(&mut items);
        Ok(items)
    }

    /// 按归一化后的业务时间字段列出全部状态待办，排序与 `/todo all` 看板保持一致。
    pub fn list_all_by_date_filter_for_board(
        &self,
        owner: &TodoOwner,
        filter: TodoListDateFilter,
    ) -> Result<Vec<TodoItem>, TodoError> {
        if filter.start > filter.end {
            return Err(TodoError::bad_request(
                "日期范围无效，开始日期不能晚于结束日期。",
            ));
        }
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?
            .into_iter()
            .filter(|item| todo_list_date_matches_range(item, filter))
            .collect::<Vec<_>>();
        sort_todos_by_created_desc(&mut items);
        sort_todo_all_board(&mut items);
        Ok(items)
    }

    /// 按关键词搜索待处理事项，返回按匹配得分排序的结果。
    pub fn search_pending(
        &self,
        owner: &TodoOwner,
        query: &str,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut scored = query_items_by_status(&conn, owner, TodoStatus::Pending)?
            .into_iter()
            .filter_map(|item| search_score(&item, query).map(|score| (score, item)))
            .collect::<Vec<_>>();
        scored.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| compare_todo_order(left, right))
        });
        Ok(scored.into_iter().map(|(_, item)| item).collect())
    }

    /// 智能匹配待处理事项：只做标题/详情等用户可见内容匹配（至多返回 5 条）。
    /// 用户侧不再暴露内部 ID，因此这里不能再把查询词当成 ID 直连数据库项。
    pub fn match_pending(
        &self,
        owner: &TodoOwner,
        query: &str,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let items = query_items_by_status(&conn, owner, TodoStatus::Pending)?;
        let mut scored = items
            .into_iter()
            .filter_map(|item| search_score(&item, query).map(|score| (score, item)))
            .collect::<Vec<_>>();
        scored.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| compare_todo_order(left, right))
        });
        Ok(scored.into_iter().take(5).map(|(_, item)| item).collect())
    }

    /// 列出在指定日期之前完成的待办事项（基于北京时间）。
    pub fn list_completed_before(
        &self,
        owner: &TodoOwner,
        completed_before: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, TodoStatus::Completed)?
            .into_iter()
            .filter(|item| {
                item.completed_at
                    .as_deref()
                    .and_then(local_date_from_timestamp)
                    .is_some_and(|date| date < completed_before)
            })
            .collect::<Vec<_>>();
        sort_completed_todos(&mut items);
        Ok(items)
    }

    /// 根据 ID 列表查找已完成的待办事项。
    pub fn list_completed_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut matched = Vec::new();
        for id in ids.iter().filter_map(|id| parse_todo_db_id(id)) {
            if let Some(item) = get_by_id_status_unlocked(&conn, owner, id, TodoStatus::Completed)?
            {
                matched.push(item);
            }
        }
        Ok(matched)
    }

    /// 根据 ID 列表查找指定状态的待办事项。
    pub fn list_by_ids_with_status(
        &self,
        owner: &TodoOwner,
        ids: &[String],
        status: TodoStatus,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut matched = Vec::new();
        for id in ids.iter().filter_map(|id| parse_todo_db_id(id)) {
            if let Some(item) = get_by_id_status_unlocked(&conn, owner, id, status.clone())? {
                matched.push(item);
            }
        }
        Ok(matched)
    }

    /// 根据内部 ID 获取当前 owner/scope 下任意状态的待办。
    pub fn get_by_id(&self, owner: &TodoOwner, id: &str) -> Result<Option<TodoItem>, TodoError> {
        let Some(id) = parse_todo_db_id(id) else {
            return Ok(None);
        };
        let conn = self.connection()?;
        get_by_id_unlocked(&conn, owner, id)
    }

    /// 后台提醒发送成功后，把仍处于 pending 的重复待办推进到下一次。
    ///
    /// 该方法只供 Notification Worker 根据可信 outbox source_id + scheduled_at 调用：
    /// 只有刚发送的 outbox 时间仍等于 Todo 当前提醒时间时才推进。这个锚点校验可以让
    /// 同一条已发送 outbox 的重复处理变成空操作，避免分钟级重复提醒被异常连跳多轮。
    /// 非重复、非 pending、无提醒、时间不匹配或不存在的待办返回 Ok(None)。
    pub fn advance_recurring_reminder_by_id(
        &self,
        id: &str,
        delivered_scheduled_at: &str,
    ) -> Result<Option<(TodoOwner, TodoItem)>, TodoError> {
        let Some(id) = parse_todo_db_id(id) else {
            return Ok(None);
        };
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let Some(item) = get_by_id_any_unlocked(&tx, id)? else {
            tx.commit().map_err(TodoError::from_sql)?;
            return Ok(None);
        };
        if item.status != TodoStatus::Pending || !recurrence::is_recurring(&item) {
            tx.commit().map_err(TodoError::from_sql)?;
            return Ok(None);
        }
        let Some(reminder_at) = item.reminder_at.as_deref() else {
            tx.commit().map_err(TodoError::from_sql)?;
            return Ok(None);
        };
        if !same_reminder_time(reminder_at, delivered_scheduled_at) {
            tx.commit().map_err(TodoError::from_sql)?;
            return Ok(None);
        }

        let owner = TodoStore::owner(item.user_id.as_deref(), &item.scope_key);
        let draft = normalize_draft(recurrence::advance_after_completion(&item)?)?;
        let updated = update_pending_todo_unlocked(&tx, &owner, id, draft, &now)?;
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(updated.map(|item| (owner, item)))
    }

    /// 将待办事项标记为已完成。
    pub fn complete(&self, owner: &TodoOwner, id: &str) -> Result<TodoItem, TodoError> {
        let id = parse_required_todo_db_id(id)?;
        let conn = self.connection()?;
        let now = now_iso_cn();
        let affected = conn
            .execute(
                "UPDATE todos
                 SET status = ?4,
                     completed = 1,
                     updated_at = ?5,
                     completed_at = ?5
                 WHERE id = ?1
                   AND owner_key = ?2
                   AND scope_key = ?3
                   AND status = ?6",
                params![
                    id,
                    owner.key.as_str(),
                    owner.scope_key.as_str(),
                    TodoStatus::Completed.as_str(),
                    now,
                    TodoStatus::Pending.as_str(),
                ],
            )
            .map_err(TodoError::from_sql)?;
        if affected == 0 {
            return Err(TodoError::not_found("todo not found"));
        }
        get_by_id_unlocked(&conn, owner, id)?
            .ok_or_else(|| TodoError::io("todo disappeared after complete"))
    }

    /// 批量完成待办事项（按 ID 列表匹配 Pending 项）。
    pub fn complete_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkCompleteOutcome, TodoError> {
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut completed = Vec::new();
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            let affected = tx
                .execute(
                    "UPDATE todos
                     SET status = ?4,
                         completed = 1,
                         updated_at = ?5,
                         completed_at = ?5
                     WHERE id = ?1
                       AND owner_key = ?2
                       AND scope_key = ?3
                       AND status = ?6",
                    params![
                        id,
                        owner.key.as_str(),
                        owner.scope_key.as_str(),
                        TodoStatus::Completed.as_str(),
                        now,
                        TodoStatus::Pending.as_str(),
                    ],
                )
                .map_err(TodoError::from_sql)?;
            if affected == 0 {
                skipped_ids.push(id_text);
            } else if let Some(item) = get_by_id_unlocked(&tx, owner, id)? {
                completed.push(item);
            }
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoBulkCompleteOutcome {
            completed,
            skipped_ids,
        })
    }

    /// 批量完成本次待办，普通待办完成，重复待办推进下一次；整批在同一事务提交。
    ///
    /// 所有重复待办会先完成推进计划计算和草稿归一化，确认无错误后才写库。
    /// 因此任一重复规则或时间字段非法时，前面的普通待办也不会被部分完成。
    pub fn complete_by_ids_with_recurrence(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoCompleteProgressOutcome, TodoError> {
        enum CompletePlan {
            Complete { id: i64, id_text: String },
            Advance { id: i64, draft: TodoItemDraft },
        }

        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut plans = Vec::new();
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            let Some(item) = get_by_id_unlocked(&tx, owner, id)? else {
                skipped_ids.push(id_text);
                continue;
            };
            if item.status != TodoStatus::Pending {
                skipped_ids.push(id_text);
                continue;
            }
            if recurrence::is_recurring(&item) {
                let draft = normalize_draft(recurrence::advance_after_completion(&item)?)?;
                plans.push(CompletePlan::Advance { id, draft });
            } else {
                plans.push(CompletePlan::Complete { id, id_text });
            }
        }

        let mut completed = Vec::new();
        let mut advanced = Vec::new();
        for plan in plans {
            match plan {
                CompletePlan::Complete { id, id_text } => {
                    match complete_pending_unlocked(&tx, owner, id, &now)? {
                        Some(item) => completed.push(item),
                        None => skipped_ids.push(id_text),
                    }
                }
                CompletePlan::Advance { id, draft } => {
                    match update_pending_todo_unlocked(&tx, owner, id, draft, &now)? {
                        Some(item) => advanced.push(item),
                        None => skipped_ids.push(id.to_string()),
                    }
                }
            }
        }

        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoCompleteProgressOutcome {
            completed,
            advanced,
            skipped_ids,
        })
    }

    /// 批量跳过重复提醒的当前周期，只推进重复项，不改变一次性待办。
    ///
    /// 这是 Phase 3 的阶段性 skip 表达：当前 schema 没有独立 skip 状态，
    /// 因此“跳过这次 / 今天别提醒了”复用“推进到下一周期”的存储动作，
    /// 但不会把一次性待办标记完成，也不会关闭 recurrence。
    pub fn advance_recurring_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoCompleteProgressOutcome, TodoError> {
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut advanced = Vec::new();
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            let Some(item) = get_by_id_unlocked(&tx, owner, id)? else {
                skipped_ids.push(id_text);
                continue;
            };
            if item.status != TodoStatus::Pending || !recurrence::is_recurring(&item) {
                skipped_ids.push(id_text);
                continue;
            }
            let draft = normalize_draft(recurrence::advance_after_completion(&item)?)?;
            match update_pending_todo_unlocked(&tx, owner, id, draft, &now)? {
                Some(item) => advanced.push(item),
                None => skipped_ids.push(id_text),
            }
        }

        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoCompleteProgressOutcome {
            completed: Vec::new(),
            advanced,
            skipped_ids,
        })
    }

    /// 批量恢复已完成待办事项（按 ID 列表匹配 Completed 项）。
    pub fn restore_completed_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkRestoreOutcome, TodoError> {
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut restored = Vec::new();
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            // 恢复完成项时必须清空 completed_at，避免 pending 列表残留完成时间语义。
            let affected = tx
                .execute(
                    "UPDATE todos
                     SET status = ?4,
                         completed = 0,
                         updated_at = ?5,
                         completed_at = NULL
                     WHERE id = ?1
                       AND owner_key = ?2
                       AND scope_key = ?3
                       AND status = ?6",
                    params![
                        id,
                        owner.key.as_str(),
                        owner.scope_key.as_str(),
                        TodoStatus::Pending.as_str(),
                        now,
                        TodoStatus::Completed.as_str(),
                    ],
                )
                .map_err(TodoError::from_sql)?;
            if affected == 0 {
                skipped_ids.push(id_text);
            } else if let Some(item) = get_by_id_unlocked(&tx, owner, id)? {
                restored.push(item);
            }
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoBulkRestoreOutcome {
            restored,
            skipped_ids,
        })
    }

    /// 编辑一条待办事项（替换标题、详情、截止时间等字段）。
    pub fn edit(
        &self,
        owner: &TodoOwner,
        id: &str,
        draft: TodoItemDraft,
    ) -> Result<TodoItem, TodoError> {
        let id = parse_required_todo_db_id(id)?;
        let draft = normalize_draft(draft)?;
        let conn = self.connection()?;
        let now = now_iso_cn();
        update_pending_todo_unlocked(&conn, owner, id, draft, &now)?
            .ok_or_else(|| TodoError::not_found("todo not found"))
    }

    fn connection(&self) -> Result<crate::storage::database::PooledSqliteConnection, TodoError> {
        self.database.connection().map_err(TodoError::from_database)
    }

    #[cfg(test)]
    pub fn set_items_for_test(
        &self,
        owner: &TodoOwner,
        items: &[TodoItem],
    ) -> Result<(), TodoError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        tx.execute(
            "DELETE FROM todos WHERE owner_key = ?1 AND scope_key = ?2",
            params![owner.key.as_str(), owner.scope_key.as_str()],
        )
        .map_err(TodoError::from_sql)?;
        for item in items {
            let id = parse_required_todo_db_id(&item.id)?;
            let user_id = item.user_id.as_deref().or(owner.user_id.as_deref());
            tx.execute(
                "INSERT INTO todos (
                    id, owner_key, user_id, scope_key, title, detail, raw_text,
                    due_date, due_at, reminder_at, time_precision, recurrence_kind,
                    recurrence_interval_days, recurrence_interval, recurrence_unit,
                    status, completed, created_at, updated_at,
                    completed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
                params![
                    id,
                    owner.key.as_str(),
                    user_id,
                    owner.scope_key.as_str(),
                    item.title,
                    item.detail,
                    item.raw_text,
                    item.due_date,
                    item.due_at,
                    item.reminder_at,
                    item.time_precision.as_str(),
                    item.recurrence_kind.as_str(),
                    i64::from(item.recurrence_interval_days),
                    i64::from(item.recurrence_interval),
                    item.recurrence_unit.as_str(),
                    item.status.as_str(),
                    item.status.completed_flag(),
                    item.created_at,
                    item.updated_at,
                    item.completed_at,
                ],
            )
            .map_err(TodoError::from_sql)?;
        }
        tx.commit().map_err(TodoError::from_sql)
    }
}

fn same_reminder_time(current: &str, delivered: &str) -> bool {
    let Some(current) = parse_local_datetime_for_comparison(current) else {
        return false;
    };
    let Some(delivered) = parse_local_datetime_for_comparison(delivered) else {
        return false;
    };
    // outbox 可能保留 RFC3339 亚秒，Todo reminder 只存到秒；按秒比较可消除格式差异。
    // 这里不能放宽到同一分钟，否则同一分钟内编辑提醒后，旧 outbox 重入会误推进下一轮。
    current.timestamp() == delivered.timestamp()
}

fn sort_items_for_status(items: &mut [TodoItem], status: &TodoStatus) {
    match status {
        TodoStatus::Pending => sort_todos(items),
        TodoStatus::Completed => sort_completed_todos_desc(items),
    }
}

fn todo_due_matches_local_date(item: &TodoItem, date: NaiveDate) -> bool {
    todo_due_local_date(item).is_some_and(|due_date| due_date == date)
}

fn todo_list_date_matches_range(item: &TodoItem, filter: TodoListDateFilter) -> bool {
    let date = match filter.field {
        TodoListDateField::Planned => todo_due_local_date(item),
        TodoListDateField::CompletedAt => item
            .completed_at
            .as_deref()
            .and_then(local_date_from_timestamp),
    };
    date.is_some_and(|date| filter.start <= date && date <= filter.end)
}

fn todo_due_local_date(item: &TodoItem) -> Option<NaiveDate> {
    // due_at 是最精确的计划时间；只有不存在 due_at 时才回退 due_date。
    if let Some(due_at) = item.due_at.as_deref().and_then(clean_optional) {
        return local_date_from_timestamp(&due_at);
    }
    item.due_date
        .as_deref()
        .and_then(clean_optional)
        .and_then(|due_date| local_date_from_timestamp(&due_date))
}

#[cfg(test)]
mod tests;
