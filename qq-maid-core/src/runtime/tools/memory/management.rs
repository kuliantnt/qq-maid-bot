//! 部署管理员 Memory 管理领域门面。
//!
//! 本文件只负责编排管理能力；opaque reference、输入校验和高影响确认协议分别位于
//! `management/refs.rs`、`management/types.rs` 与 `management/confirmation.rs`。
//! HTTP 层不接触 MemoryStore、scope key、平台 raw ID 或 Memory 内部 owner 字段。

mod confirmation;
mod refs;
mod types;

#[cfg(test)]
mod tests;

use rusqlite::Transaction;

use super::{
    MemoryCategory, MemoryError, MemoryRecord, MemorySourceType, MemoryStatus, MemoryStore,
    ops::validate_visibility,
    storage::{ManagementListQuery, PersistMemoryRequest},
};

use self::{
    confirmation::{commit as commit_confirmation, prepare as prepare_confirmation},
    refs::{
        ResolvedTarget, ensure_expected_version, memory_item, memory_ref_for, normalize_keyword,
        record_matches_target, resolved_target, target_matches_filter, validate_content,
        validate_list_filter, validate_ref, validate_target_filter,
    },
    types::{
        MemoryManagementItem, MemoryManagementMutationResult, MemoryManagementPage,
        MemoryTargetPage, PreparedMemoryOperation,
    },
};

pub(crate) use self::types::{
    ManagementActor, MemoryCreateInput, MemoryListFilter, MemoryManagementError,
    MemoryManagementService, MemoryTargetFilter, MemoryUpdatePatch,
};

impl MemoryManagementService {
    pub(crate) fn new(store: MemoryStore) -> Self {
        Self {
            store,
            confirmations: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    pub(crate) fn targets(
        &self,
        filter: MemoryTargetFilter,
        limit: usize,
        offset: usize,
    ) -> Result<MemoryTargetPage, MemoryManagementError> {
        validate_target_filter(&filter)?;
        let targets = self.visible_targets()?;
        let mut filtered = targets
            .into_iter()
            .filter(|target| target_matches_filter(target, &filter))
            .map(|target| target.summary)
            .collect::<Vec<_>>();
        let total_count = filtered.len();
        let start = offset.min(total_count);
        let end = start.saturating_add(limit).min(total_count);
        Ok(MemoryTargetPage {
            items: filtered.drain(start..end).collect(),
            total_count,
        })
    }

    pub(crate) fn list(
        &self,
        filter: MemoryListFilter,
        limit: usize,
        offset: usize,
    ) -> Result<MemoryManagementPage, MemoryManagementError> {
        validate_list_filter(&filter)?;
        let targets = self.resolve_list_targets(&filter)?;
        let page = self
            .store
            .management_list(&ManagementListQuery {
                targets: targets.iter().map(|target| target.target.clone()).collect(),
                status: filter.status,
                category: filter.category,
                visibility: filter.visibility,
                pinned: filter.pinned,
                keyword: normalize_keyword(filter.keyword)?,
                limit,
                offset,
            })
            .map_err(MemoryManagementError::from)?;
        self.present_page(page, targets)
    }

    pub(crate) fn get(
        &self,
        target_ref: &str,
        memory_ref: &str,
    ) -> Result<MemoryManagementItem, MemoryManagementError> {
        let target = self.resolve_target_ref(target_ref)?;
        let record = self.resolve_memory_ref(&target, memory_ref)?;
        self.present(&target, record)
    }

    #[cfg(test)]
    pub(crate) fn create(
        &self,
        input: MemoryCreateInput,
    ) -> Result<MemoryManagementMutationResult, MemoryManagementError> {
        self.create_with_audit(input, |_, _| Ok(()))
    }

    pub(crate) fn create_with_audit<F>(
        &self,
        input: MemoryCreateInput,
        audit: F,
    ) -> Result<MemoryManagementMutationResult, MemoryManagementError>
    where
        F: Fn(&Transaction<'_>, Option<u64>) -> Result<(), MemoryManagementError>,
    {
        let target = self.resolve_target_ref(&input.target_ref)?;
        validate_content(&input.content)?;
        validate_visibility(&target.target, input.visibility)
            .map_err(MemoryManagementError::from)?;
        let result = self
            .store
            .persist_v3_with_audit(
                PersistMemoryRequest {
                    target: target.target.clone(),
                    created_by_user_id: None,
                    content: input.content,
                    source_text: String::new(),
                    category: input.category,
                    legacy_scope: input.category.as_str().to_owned(),
                    visibility: input.visibility,
                    source_type: MemorySourceType::ManualImport,
                    source_ref: None,
                    confirmed_at: None,
                    pinned: input.pinned,
                    attribute_key: input.attribute_key,
                    relation_subject_id: None,
                    relation_object_id: None,
                },
                |tx, result| {
                    audit(tx, Some(result.record.revision))
                        .map_err(|error| MemoryError::audit_failed(error.message()))
                },
            )
            .map_err(MemoryManagementError::from)?;
        self.present_mutation(&target, result)
    }

    #[cfg(test)]
    pub(crate) fn update(
        &self,
        target_ref: &str,
        memory_ref: &str,
        expected_version: u64,
        patch: MemoryUpdatePatch,
    ) -> Result<MemoryManagementMutationResult, MemoryManagementError> {
        self.update_with_audit(target_ref, memory_ref, expected_version, patch, |_, _| {
            Ok(())
        })
    }

    pub(crate) fn update_with_audit<F>(
        &self,
        target_ref: &str,
        memory_ref: &str,
        expected_version: u64,
        patch: MemoryUpdatePatch,
        audit: F,
    ) -> Result<MemoryManagementMutationResult, MemoryManagementError>
    where
        F: Fn(&Transaction<'_>, Option<u64>) -> Result<(), MemoryManagementError>,
    {
        if patch.is_empty() {
            return Err(MemoryManagementError::Validation(
                "update patch must not be empty".to_owned(),
            ));
        }
        let target = self.resolve_target_ref(target_ref)?;
        let current = self.resolve_memory_ref(&target, memory_ref)?;
        ensure_expected_version(&current, expected_version)?;
        if current.status != MemoryStatus::Active {
            return Err(MemoryManagementError::Conflict(
                "only active memory can be edited".to_owned(),
            ));
        }
        let category = patch
            .category
            .or_else(|| current.memory_type.parse().ok())
            .ok_or_else(|| {
                MemoryManagementError::Validation("memory category is invalid".to_owned())
            })?;
        let visibility = patch.visibility.unwrap_or(current.visibility);
        validate_visibility(&target.target, visibility).map_err(MemoryManagementError::from)?;
        if let Some(content) = patch.content.as_deref() {
            validate_content(content)?;
        }
        if category != MemoryCategory::Relation
            && (current.relation_subject_id.is_some() || current.relation_object_id.is_some())
        {
            return Err(MemoryManagementError::Validation(
                "relation memory category cannot be changed".to_owned(),
            ));
        }
        let result = self
            .store
            .replace_v3_if_unchanged_with_audit(
                &target.target,
                &current.id,
                &current,
                PersistMemoryRequest {
                    target: target.target.clone(),
                    created_by_user_id: current.created_by_user_id.clone(),
                    content: patch.content.unwrap_or_else(|| current.content.clone()),
                    source_text: current.source_text.clone(),
                    category,
                    legacy_scope: current.scope.clone(),
                    visibility,
                    source_type: current.source_type,
                    source_ref: current.source_ref.clone(),
                    confirmed_at: current.last_confirmed_at.clone(),
                    pinned: patch.pinned.unwrap_or(current.pinned),
                    attribute_key: patch
                        .attribute_key
                        .unwrap_or_else(|| current.attribute_key.clone()),
                    relation_subject_id: current.relation_subject_id.clone(),
                    relation_object_id: current.relation_object_id.clone(),
                },
                |tx, result| {
                    audit(tx, Some(result.record.revision))
                        .map_err(|error| MemoryError::audit_failed(error.message()))
                },
            )
            .map_err(MemoryManagementError::from)?;
        self.present_mutation(&target, result)
    }

    #[cfg(test)]
    pub(crate) fn archive(
        &self,
        target_ref: &str,
        memory_ref: &str,
        expected_version: u64,
    ) -> Result<MemoryManagementMutationResult, MemoryManagementError> {
        self.archive_with_audit(target_ref, memory_ref, expected_version, |_, _| Ok(()))
    }

    pub(crate) fn archive_with_audit<F>(
        &self,
        target_ref: &str,
        memory_ref: &str,
        expected_version: u64,
        audit: F,
    ) -> Result<MemoryManagementMutationResult, MemoryManagementError>
    where
        F: Fn(&Transaction<'_>, Option<u64>) -> Result<(), MemoryManagementError>,
    {
        let target = self.resolve_target_ref(target_ref)?;
        let current = self.resolve_memory_ref(&target, memory_ref)?;
        ensure_expected_version(&current, expected_version)?;
        let archived = self
            .store
            .management_archive_if_unchanged_with_audit(&target.target, &current, |tx, version| {
                audit(tx, version).map_err(|error| MemoryError::audit_failed(error.message()))
            })
            .map_err(MemoryManagementError::from)?;
        Ok(MemoryManagementMutationResult {
            memory: memory_item(&target, archived.record, archived.profile_enabled)?,
            archived_count: 1,
        })
    }

    #[cfg(test)]
    pub(crate) fn restore(
        &self,
        target_ref: &str,
        memory_ref: &str,
        expected_version: u64,
    ) -> Result<MemoryManagementMutationResult, MemoryManagementError> {
        self.restore_with_audit(target_ref, memory_ref, expected_version, |_, _| Ok(()))
    }

    pub(crate) fn restore_with_audit<F>(
        &self,
        target_ref: &str,
        memory_ref: &str,
        expected_version: u64,
        audit: F,
    ) -> Result<MemoryManagementMutationResult, MemoryManagementError>
    where
        F: Fn(&Transaction<'_>, Option<u64>) -> Result<(), MemoryManagementError>,
    {
        let target = self.resolve_target_ref(target_ref)?;
        let current = self.resolve_memory_ref(&target, memory_ref)?;
        ensure_expected_version(&current, expected_version)?;
        let restored = self
            .store
            .management_restore_if_unchanged_with_audit(&target.target, &current, |tx, version| {
                audit(tx, version).map_err(|error| MemoryError::audit_failed(error.message()))
            })
            .map_err(MemoryManagementError::from)?;
        Ok(MemoryManagementMutationResult {
            memory: memory_item(&target, restored.record, restored.profile_enabled)?,
            archived_count: 0,
        })
    }

    pub(crate) fn prepare(
        &self,
        actor: ManagementActor,
        operation: &str,
        target_ref: &str,
    ) -> Result<PreparedMemoryOperation, MemoryManagementError> {
        prepare_confirmation(self, actor, operation, target_ref)
    }

    #[cfg(test)]
    pub(crate) fn commit(
        &self,
        actor: ManagementActor,
        operation: &str,
        target_ref: &str,
        confirmation_token: &str,
    ) -> Result<types::MemoryOperationResult, MemoryManagementError> {
        self.commit_with_audit(actor, operation, target_ref, confirmation_token, |_, _| {
            Ok(())
        })
    }

    pub(crate) fn commit_with_audit<F>(
        &self,
        actor: ManagementActor,
        operation: &str,
        target_ref: &str,
        confirmation_token: &str,
        audit: F,
    ) -> Result<types::MemoryOperationResult, MemoryManagementError>
    where
        F: Fn(&Transaction<'_>, Option<u64>) -> Result<(), MemoryManagementError>,
    {
        commit_confirmation(
            self,
            actor,
            operation,
            target_ref,
            confirmation_token,
            audit,
        )
    }

    fn visible_targets(&self) -> Result<Vec<ResolvedTarget>, MemoryManagementError> {
        Ok(self
            .store
            .management_target_candidates()
            .map_err(MemoryManagementError::from)?
            .into_iter()
            .filter_map(|target| resolved_target(target).ok())
            .collect())
    }

    pub(super) fn resolve_target_ref(
        &self,
        target_ref: &str,
    ) -> Result<ResolvedTarget, MemoryManagementError> {
        validate_ref(target_ref, types::TARGET_REF_PREFIX)?;
        self.visible_targets()?
            .into_iter()
            .find(|target| target.summary.target_ref == target_ref)
            .ok_or(MemoryManagementError::NotFound)
    }

    fn resolve_memory_ref(
        &self,
        target: &ResolvedTarget,
        memory_ref: &str,
    ) -> Result<MemoryRecord, MemoryManagementError> {
        validate_ref(memory_ref, types::MEMORY_REF_PREFIX)?;
        self.store
            .management_records_for_target(&target.target)
            .map_err(MemoryManagementError::from)?
            .into_iter()
            .find(|record| memory_ref_for(&target.summary.target_ref, &record.id) == memory_ref)
            .filter(|record| record.memory_type.parse::<MemoryCategory>().is_ok())
            .ok_or(MemoryManagementError::NotFound)
    }

    fn resolve_list_targets(
        &self,
        filter: &MemoryListFilter,
    ) -> Result<Vec<ResolvedTarget>, MemoryManagementError> {
        let targets = self.visible_targets()?;
        if let Some(target_ref) = filter.target_ref.as_deref()
            && !targets
                .iter()
                .any(|target| target.summary.target_ref == target_ref)
        {
            return Err(MemoryManagementError::NotFound);
        }
        Ok(targets
            .into_iter()
            .filter(|target| {
                filter
                    .target_ref
                    .as_deref()
                    .is_none_or(|target_ref| target.summary.target_ref == target_ref)
                    && target_matches_filter(
                        target,
                        &MemoryTargetFilter {
                            scope: filter.scope,
                            platform: filter.platform.clone(),
                            account_ref: filter.account_ref.clone(),
                            group_ref: filter.group_ref.clone(),
                            subject_ref: filter.subject_ref.clone(),
                        },
                    )
            })
            .collect())
    }

    fn present_mutation(
        &self,
        target: &ResolvedTarget,
        result: super::storage::PersistMemoryResult,
    ) -> Result<MemoryManagementMutationResult, MemoryManagementError> {
        let archived_count = result.archived_ids.len();
        Ok(MemoryManagementMutationResult {
            memory: memory_item(target, result.record, result.profile_enabled)?,
            archived_count,
        })
    }

    fn present(
        &self,
        target: &ResolvedTarget,
        record: MemoryRecord,
    ) -> Result<MemoryManagementItem, MemoryManagementError> {
        let profile_enabled = self
            .store
            .management_snapshot(&target.target)
            .map_err(MemoryManagementError::from)?
            .profile_enabled
            .unwrap_or(true);
        memory_item(target, record, profile_enabled)
    }

    fn present_page(
        &self,
        page: super::storage::ManagementPage,
        targets: Vec<ResolvedTarget>,
    ) -> Result<MemoryManagementPage, MemoryManagementError> {
        let by_target = targets
            .into_iter()
            .map(|target| (target.target.clone(), target))
            .collect::<Vec<_>>();
        let mut items = Vec::with_capacity(page.items.len());
        for record in page.items {
            let target = by_target
                .iter()
                .find(|(target, _)| record_matches_target(&record, target))
                .map(|(_, target)| target)
                .ok_or(MemoryManagementError::Internal)?;
            items.push(self.present(target, record)?);
        }
        Ok(MemoryManagementPage {
            items,
            total_count: page.total_count,
        })
    }
}
