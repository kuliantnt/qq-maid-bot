//! Memory 管理 API 所需的精确查询与原子事务。
//!
//! 这里不解析 opaque reference，也不决定部署管理员的授权范围；调用方必须先在
//! `runtime/tools/memory/management.rs` 完成 target 回查。storage 只接受已经解析的
//! `MemoryTarget`，并保证列表条件、revision CAS 和高影响操作的事务一致性。

use rusqlite::types::Value as SqlValue;
use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};

use super::{
    MemoryCategory, MemoryError, MemoryKind, MemoryRecord, MemoryScopeType, MemoryStatus,
    MemoryStore, MemoryTarget, MemoryVisibility, clean::clean_scope_id, row::memory_from_row,
};

const MANAGEMENT_COLUMNS: &str = "id, created_at, updated_at, memory_type, scope,
    scope_type, scope_id, created_by_user_id, user_id, group_id, content, source_text,
    memory_kind, subject_id, relation_subject_id, relation_object_id, visibility,
    source_type, source_ref, last_confirmed_at, status, pinned, attribute_key, revision";
const SUPPORTED_MEMORY_CATEGORIES: &str =
    "('note', 'preference', 'identity', 'relation', 'instruction')";

/// 管理列表的服务端 SQL 条件。所有字段均来自领域层已经验证的枚举或 target。
#[derive(Debug, Clone, Default)]
pub(crate) struct ManagementListQuery {
    pub(crate) targets: Vec<MemoryTarget>,
    pub(crate) status: Option<MemoryStatus>,
    pub(crate) category: Option<MemoryCategory>,
    pub(crate) visibility: Option<MemoryVisibility>,
    pub(crate) pinned: Option<bool>,
    pub(crate) keyword: Option<String>,
    pub(crate) limit: usize,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagementPage {
    pub(crate) items: Vec<MemoryRecord>,
    pub(crate) total_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagementTargetSnapshot {
    pub(crate) active: Vec<(String, u64)>,
    pub(crate) profile_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagementMutationResult {
    pub(crate) record: MemoryRecord,
    pub(crate) profile_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagementBulkMutationResult {
    pub(crate) affected_count: usize,
    pub(crate) profile_enabled: bool,
}

impl MemoryStore {
    /// 从已经存在的 v3 Memory 记录发现完整 target。
    ///
    /// 只返回 personal/group 的合法四元组；legacy、缺失 scope、未知 kind 或不完整
    /// subject 一律留在数据库中但不进入管理发现面。
    pub(crate) fn management_target_candidates(&self) -> Result<Vec<MemoryTarget>, MemoryError> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT scope_type, scope_id, memory_kind, subject_id
                 FROM memories
                 WHERE scope_type IN ('personal', 'group')
                   AND memory_kind IN ('personal', 'group_profile', 'group')
                   AND scope_id IS NOT NULL AND trim(scope_id) <> ''
                 ORDER BY scope_type, scope_id, memory_kind, subject_id",
            )
            .map_err(MemoryError::from_sql)?;
        let rows = stmt
            .query_map([], |row| {
                let scope_type: String = row.get(0)?;
                let scope_id: String = row.get(1)?;
                let memory_kind: String = row.get(2)?;
                let subject_id: Option<String> = row.get(3)?;
                Ok((scope_type, scope_id, memory_kind, subject_id))
            })
            .map_err(MemoryError::from_sql)?;
        let mut targets = Vec::new();
        for row in rows {
            let (scope_type, scope_id, memory_kind, subject_id) =
                row.map_err(MemoryError::from_sql)?;
            let Ok(scope_type) = scope_type.parse::<MemoryScopeType>() else {
                continue;
            };
            let Ok(memory_kind) = memory_kind.parse::<MemoryKind>() else {
                continue;
            };
            let target = MemoryTarget {
                scope_type,
                scope_id,
                memory_kind,
                subject_id,
            };
            if let Ok(target) = target.clean() {
                targets.push(target);
            }
        }
        Ok(targets)
    }

    pub(crate) fn management_list(
        &self,
        query: &ManagementListQuery,
    ) -> Result<ManagementPage, MemoryError> {
        if query.targets.is_empty() {
            return Ok(ManagementPage {
                items: Vec::new(),
                total_count: 0,
            });
        }
        let conn = self.connection()?;
        let (where_sql, values) = management_where(query);
        let total_count = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM memories {where_sql}"),
                params_from_iter(values.iter()),
                |row| row.get::<_, i64>(0),
            )
            .map_err(MemoryError::from_sql)?;
        let total_count = usize::try_from(total_count)
            .map_err(|_| MemoryError::io("memory count exceeds platform range"))?;

        let mut page_values = values;
        page_values.push(SqlValue::Integer(i64::try_from(query.limit).map_err(
            |_| MemoryError::bad_request("memory page size is too large"),
        )?));
        page_values.push(SqlValue::Integer(i64::try_from(query.offset).map_err(
            |_| MemoryError::bad_request("memory page offset is too large"),
        )?));
        let sql = format!(
            "SELECT {MANAGEMENT_COLUMNS} FROM memories {where_sql}
             ORDER BY pinned DESC, COALESCE(updated_at, created_at) DESC, row_id DESC
             LIMIT ? OFFSET ?"
        );
        let mut stmt = conn.prepare(&sql).map_err(MemoryError::from_sql)?;
        let rows = stmt
            .query_map(params_from_iter(page_values.iter()), memory_from_row)
            .map_err(MemoryError::from_sql)?;
        let items = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(MemoryError::from_sql)?;
        Ok(ManagementPage { items, total_count })
    }

    pub(crate) fn management_records_for_target(
        &self,
        target: &MemoryTarget,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let target = target.clean()?;
        let conn = self.connection()?;
        let sql = format!(
            "SELECT {MANAGEMENT_COLUMNS} FROM memories
             WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
               AND subject_id IS ?4
             ORDER BY row_id DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(MemoryError::from_sql)?;
        let rows = stmt
            .query_map(
                params![
                    target.scope_type.as_str(),
                    target.scope_id,
                    target.memory_kind.as_str(),
                    target.subject_id,
                ],
                memory_from_row,
            )
            .map_err(MemoryError::from_sql)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(MemoryError::from_sql)
    }

    /// 以同一连接读取 active revision 和群画像偏好，供 prepare 固定快照。
    pub(crate) fn management_snapshot(
        &self,
        target: &MemoryTarget,
    ) -> Result<ManagementTargetSnapshot, MemoryError> {
        #[cfg(test)]
        if self
            .management_snapshot_failure
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(MemoryError::io(
                "management snapshot failure injected for test",
            ));
        }
        let target = target.clean()?;
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, revision FROM memories
                 WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
                   AND subject_id IS ?4 AND status = 'active'
                 ORDER BY row_id DESC",
            )
            .map_err(MemoryError::from_sql)?;
        let rows = stmt
            .query_map(
                params![
                    target.scope_type.as_str(),
                    target.scope_id,
                    target.memory_kind.as_str(),
                    target.subject_id,
                ],
                |row| {
                    let revision = row.get::<_, i64>(1)?;
                    let revision = u64::try_from(revision).map_err(|_| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Integer,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "memory revision must be positive",
                            )),
                        )
                    })?;
                    Ok((row.get::<_, String>(0)?, revision))
                },
            )
            .map_err(MemoryError::from_sql)?;
        let active = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(MemoryError::from_sql)?;
        let profile_enabled = if target.memory_kind == MemoryKind::GroupProfile {
            let subject_id = target
                .subject_id
                .as_deref()
                .ok_or_else(|| MemoryError::bad_request("subject_id is required"))?;
            Some(
                conn.query_row(
                    "SELECT profile_enabled FROM memory_profile_preferences
                     WHERE group_scope_id = ?1 AND subject_id = ?2",
                    params![target.scope_id, subject_id],
                    |row| row.get::<_, bool>(0),
                )
                .optional()
                .map_err(MemoryError::from_sql)?
                .unwrap_or(true),
            )
        } else {
            None
        };
        Ok(ManagementTargetSnapshot {
            active,
            profile_enabled,
        })
    }

    /// 只读取目标级群画像开关；单条删除确认不需要扫描整个 target 的 active 记录。
    pub(crate) fn management_profile_enabled(
        &self,
        target: &MemoryTarget,
    ) -> Result<bool, MemoryError> {
        #[cfg(test)]
        if self
            .management_snapshot_failure
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(MemoryError::io(
                "management snapshot failure injected for test",
            ));
        }
        let target = target.clean()?;
        if target.memory_kind != MemoryKind::GroupProfile {
            return Ok(true);
        }
        let subject_id = target
            .subject_id
            .as_deref()
            .ok_or_else(|| MemoryError::bad_request("subject_id is required"))?;
        let conn = self.connection()?;
        conn.query_row(
            "SELECT profile_enabled FROM memory_profile_preferences
             WHERE group_scope_id = ?1 AND subject_id = ?2",
            params![target.scope_id, subject_id],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map(|value| value.unwrap_or(true))
        .map_err(MemoryError::from_sql)
    }

    /// 清空的领域语义是 active → archived；ID 与 revision 快照不完全相同则整笔事务失败。
    pub(crate) fn management_clear_if_unchanged_with_audit<F>(
        &self,
        target: &MemoryTarget,
        expected: &[(String, u64)],
        audit: F,
    ) -> Result<ManagementBulkMutationResult, MemoryError>
    where
        F: FnOnce(&Transaction<'_>, Option<u64>) -> Result<(), MemoryError>,
    {
        let target = target.clean()?;
        self.with_immediate_transaction(|tx| {
            let current = active_versions_unlocked(tx, &target)?;
            if current != expected {
                return Err(MemoryError::changed(
                    "memory target changed after confirmation was prepared",
                ));
            }
            let affected_count = super::v3::archive_target_unlocked_for_management(tx, &target)?;
            let profile_enabled = profile_enabled_for_management_in_transaction(tx, &target)?;
            audit(tx, None)?;
            Ok(ManagementBulkMutationResult {
                affected_count,
                profile_enabled,
            })
        })
    }

    /// 停用群画像沿用现有 profile preference + 归档语义，并把偏好与记录状态放在同一事务。
    pub(crate) fn management_disable_group_profile_if_unchanged_with_audit<F>(
        &self,
        target: &MemoryTarget,
        expected_enabled: bool,
        expected: &[(String, u64)],
        audit: F,
    ) -> Result<ManagementBulkMutationResult, MemoryError>
    where
        F: FnOnce(&Transaction<'_>, Option<u64>) -> Result<(), MemoryError>,
    {
        let target = target.clean()?;
        if target.memory_kind != MemoryKind::GroupProfile {
            return Err(MemoryError::bad_request(
                "profile preference requires a group profile target",
            ));
        }
        let subject_id = target
            .subject_id
            .as_deref()
            .ok_or_else(|| MemoryError::bad_request("subject_id is required"))?;
        self.with_immediate_transaction(|tx| {
            let enabled = profile_enabled_in_transaction(tx, &target)?;
            let current = active_versions_unlocked(tx, &target)?;
            if enabled != expected_enabled || current != expected {
                return Err(MemoryError::changed(
                    "memory target changed after confirmation was prepared",
                ));
            }
            let now = super::now_iso_cn();
            tx.execute(
                "INSERT INTO memory_profile_preferences (
                     group_scope_id, subject_id, profile_enabled, created_at, updated_at
                 ) VALUES (?1, ?2, 0, ?3, ?3)
                 ON CONFLICT(group_scope_id, subject_id) DO UPDATE SET
                     profile_enabled = 0, updated_at = excluded.updated_at",
                params![target.scope_id, subject_id, now],
            )
            .map_err(MemoryError::from_sql)?;
            let affected_count = super::v3::archive_target_unlocked_for_management(tx, &target)?;
            audit(tx, None)?;
            Ok(ManagementBulkMutationResult {
                affected_count,
                profile_enabled: false,
            })
        })
    }

    /// 管理更新/归档之外的恢复动作仍然必须在领域 storage 事务内比较完整快照。
    pub(crate) fn management_restore_if_unchanged_with_audit<F>(
        &self,
        target: &MemoryTarget,
        expected: &MemoryRecord,
        audit: F,
    ) -> Result<ManagementMutationResult, MemoryError>
    where
        F: FnOnce(&Transaction<'_>, Option<u64>) -> Result<(), MemoryError>,
    {
        let target = target.clean()?;
        if expected.status != MemoryStatus::Archived {
            return Err(MemoryError::changed(
                "memory is no longer in the prepared archived state",
            ));
        }
        let after_version = expected
            .revision
            .checked_add(1)
            .ok_or_else(|| MemoryError::io("memory revision overflow"))?;
        self.with_immediate_transaction(|tx| {
            super::v3::ensure_record_unchanged_for_management(tx, &target, &expected.id, expected)?;
            let profile_enabled = profile_enabled_for_management_in_transaction(tx, &target)?;
            if expected.memory_kind == MemoryKind::GroupProfile && !profile_enabled {
                return Err(MemoryError::profile_opted_out());
            }
            if let Some(attribute_key) = expected.attribute_key.as_deref()
                && active_attribute_conflict_exists(tx, expected, attribute_key)?
            {
                return Err(MemoryError::changed(
                    "an active memory already uses this attribute",
                ));
            }
            let changed = tx
                .execute(
                    "UPDATE memories SET status = 'active', updated_at = ?1, revision = revision + 1
                     WHERE id = ?2 AND scope_type = ?3 AND scope_id = ?4 AND memory_kind = ?5
                       AND subject_id IS ?6 AND status = 'archived' AND revision = ?7",
                    params![
                        super::now_iso_cn(),
                        expected.id,
                        target.scope_type.as_str(),
                        target.scope_id,
                        target.memory_kind.as_str(),
                        target.subject_id,
                        sqlite_revision(expected.revision)?,
                    ],
                )
                .map_err(MemoryError::from_sql)?;
            if changed != 1 {
                return Err(MemoryError::changed(
                    "memory changed after confirmation was prepared",
                ));
            }
            let record = super::get_by_id_unlocked(tx, &expected.id)?
                .ok_or_else(|| MemoryError::io("memory disappeared inside restore transaction"))?;
            if record.status != MemoryStatus::Active || record.revision != after_version {
                return Err(MemoryError::io("restored memory result is inconsistent"));
            }
            audit(tx, Some(after_version))?;
            Ok(ManagementMutationResult {
                record,
                profile_enabled,
            })
        })
    }

    pub(crate) fn management_archive_if_unchanged_with_audit<F>(
        &self,
        target: &MemoryTarget,
        expected: &MemoryRecord,
        audit: F,
    ) -> Result<ManagementMutationResult, MemoryError>
    where
        F: FnOnce(&Transaction<'_>, Option<u64>) -> Result<(), MemoryError>,
    {
        let target = target.clean()?;
        if expected.status != MemoryStatus::Active {
            return Err(MemoryError::changed(
                "memory is no longer in the prepared active state",
            ));
        }
        let after_version = expected
            .revision
            .checked_add(1)
            .ok_or_else(|| MemoryError::io("memory revision overflow"))?;
        self.with_immediate_transaction(|tx| {
            super::v3::ensure_record_unchanged_for_management(tx, &target, &expected.id, expected)?;
            let changed = super::v3::archive_id_for_target_unlocked_for_management(
                tx,
                &target,
                &expected.id,
            )?;
            if changed != 1 {
                return Err(MemoryError::changed(
                    "memory changed after confirmation was prepared",
                ));
            }
            let profile_enabled = profile_enabled_for_management_in_transaction(tx, &target)?;
            let record = super::get_by_id_unlocked(tx, &expected.id)?
                .ok_or_else(|| MemoryError::io("memory disappeared inside archive transaction"))?;
            if record.status != MemoryStatus::Archived || record.revision != after_version {
                return Err(MemoryError::io("archived memory result is inconsistent"));
            }
            audit(tx, Some(after_version))?;
            Ok(ManagementMutationResult {
                record,
                profile_enabled,
            })
        })
    }

    /// 管理员永久删除只允许 active 记录；完整快照校验、DELETE 和审计共享同一事务。
    pub(crate) fn management_delete_if_unchanged_with_audit<F>(
        &self,
        target: &MemoryTarget,
        expected: &MemoryRecord,
        audit: F,
    ) -> Result<bool, MemoryError>
    where
        F: FnOnce(&Transaction<'_>, Option<u64>) -> Result<(), MemoryError>,
    {
        let target = target.clean()?;
        if expected.status != MemoryStatus::Active {
            return Err(MemoryError::changed(
                "memory is no longer in the prepared active state",
            ));
        }
        self.with_immediate_transaction(|tx| {
            super::v3::ensure_record_unchanged_for_management(tx, &target, &expected.id, expected)?;
            let profile_enabled = profile_enabled_for_management_in_transaction(tx, &target)?;
            if expected.memory_kind == MemoryKind::GroupProfile && !profile_enabled {
                return Err(MemoryError::profile_opted_out());
            }
            let changed = tx
                .execute(
                    "DELETE FROM memories
                     WHERE id = ?1 AND scope_type = ?2 AND scope_id = ?3
                       AND memory_kind = ?4 AND subject_id IS ?5
                       AND status = 'active' AND revision = ?6",
                    params![
                        expected.id,
                        target.scope_type.as_str(),
                        target.scope_id,
                        target.memory_kind.as_str(),
                        target.subject_id,
                        sqlite_revision(expected.revision)?,
                    ],
                )
                .map_err(MemoryError::from_sql)?;
            if changed != 1 {
                return Err(MemoryError::changed(
                    "memory changed after confirmation was prepared",
                ));
            }
            audit(tx, None)?;
            Ok(profile_enabled)
        })
    }
}

fn management_where(query: &ManagementListQuery) -> (String, Vec<SqlValue>) {
    let mut sql = String::from("WHERE ");
    let mut values = Vec::new();
    if query.targets.is_empty() {
        sql.push_str("0 = 1");
        return (sql, values);
    }
    sql.push('(');
    for (index, target) in query.targets.iter().enumerate() {
        if index > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str("(scope_type = ? AND scope_id = ? AND memory_kind = ? AND subject_id IS ?)");
        values.push(SqlValue::Text(target.scope_type.as_str().to_owned()));
        values.push(SqlValue::Text(target.scope_id.clone()));
        values.push(SqlValue::Text(target.memory_kind.as_str().to_owned()));
        values.push(
            target
                .subject_id
                .as_deref()
                .map_or(SqlValue::Null, |value| SqlValue::Text(value.to_owned())),
        );
    }
    sql.push(')');
    // 历史 schema 允许自由 TEXT；管理 API 对未知 category 采取 fail-closed，避免把它
    // 伪装成当前枚举中的任何一个类别。
    sql.push_str(" AND memory_type IN ");
    sql.push_str(SUPPORTED_MEMORY_CATEGORIES);
    if let Some(status) = query.status {
        sql.push_str(" AND status = ?");
        values.push(SqlValue::Text(status.as_str().to_owned()));
    }
    if let Some(category) = query.category {
        sql.push_str(" AND memory_type = ?");
        values.push(SqlValue::Text(category.as_str().to_owned()));
    }
    if let Some(visibility) = query.visibility {
        sql.push_str(" AND visibility = ?");
        values.push(SqlValue::Text(visibility.as_str().to_owned()));
    }
    if let Some(pinned) = query.pinned {
        sql.push_str(" AND pinned = ?");
        values.push(SqlValue::Integer(i64::from(pinned)));
    }
    if let Some(keyword) = query.keyword.as_deref() {
        sql.push_str(" AND content LIKE ? ESCAPE '\\'");
        values.push(SqlValue::Text(format!(
            "%{}%",
            escape_like_literal(keyword)
        )));
    }
    (sql, values)
}

/// `%`, `_` 和 escape 字符都按用户输入的字面字符处理。
pub(crate) fn escape_like_literal(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '%' => output.push_str("\\%"),
            '_' => output.push_str("\\_"),
            _ => output.push(character),
        }
    }
    output
}

fn active_versions_unlocked(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
) -> Result<Vec<(String, u64)>, MemoryError> {
    let mut stmt = tx
        .prepare(
            "SELECT id, revision FROM memories
             WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
               AND subject_id IS ?4 AND status = 'active'
             ORDER BY row_id DESC",
        )
        .map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(
            params![
                target.scope_type.as_str(),
                target.scope_id,
                target.memory_kind.as_str(),
                target.subject_id,
            ],
            |row| {
                let revision = row.get::<_, i64>(1)?;
                let revision = u64::try_from(revision).map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Integer,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "memory revision must be positive",
                        )),
                    )
                })?;
                Ok((row.get::<_, String>(0)?, revision))
            },
        )
        .map_err(MemoryError::from_sql)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)
}

fn profile_enabled_in_transaction(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
) -> Result<bool, MemoryError> {
    let subject_id = target
        .subject_id
        .as_deref()
        .ok_or_else(|| MemoryError::bad_request("subject_id is required"))?;
    tx.query_row(
        "SELECT profile_enabled FROM memory_profile_preferences
         WHERE group_scope_id = ?1 AND subject_id = ?2",
        params![target.scope_id, subject_id],
        |row| row.get::<_, bool>(0),
    )
    .optional()
    .map(|value| value.unwrap_or(true))
    .map_err(MemoryError::from_sql)
}

fn profile_enabled_for_management_in_transaction(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
) -> Result<bool, MemoryError> {
    if target.memory_kind == MemoryKind::GroupProfile {
        profile_enabled_in_transaction(tx, target)
    } else {
        Ok(true)
    }
}

fn active_attribute_conflict_exists(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
    attribute_key: &str,
) -> Result<bool, MemoryError> {
    tx.query_row(
        "SELECT 1 FROM memories
         WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
           AND subject_id IS ?4 AND relation_subject_id IS ?5
           AND relation_object_id IS ?6 AND attribute_key = ?7
           AND status = 'active' AND id <> ?8 LIMIT 1",
        params![
            record.scope_type,
            record.scope_id,
            record.memory_kind.as_str(),
            record.subject_id,
            record.relation_subject_id,
            record.relation_object_id,
            attribute_key,
            record.id,
        ],
        |_| Ok(()),
    )
    .optional()
    .map(|value| value.is_some())
    .map_err(MemoryError::from_sql)
}

fn sqlite_revision(revision: u64) -> Result<i64, MemoryError> {
    i64::try_from(revision).map_err(|_| MemoryError::io("memory revision exceeds SQLite range"))
}

#[allow(dead_code)]
fn _clean_scope_id_for_future_management(value: &str) -> Result<String, MemoryError> {
    clean_scope_id(value)
}
