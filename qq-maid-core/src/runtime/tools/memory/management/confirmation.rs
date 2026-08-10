//! Memory 高影响操作的 session-bound prepare/commit 协议。

use rusqlite::Transaction;
use uuid::Uuid;

use super::super::{MemoryKind, MemoryStatus};
use super::{
    MemoryManagementService,
    refs::{
        ensure_expected_version, now_seconds, operation_capabilities, prune_confirmations,
        token_digest, validate_confirmation_token,
    },
    types::{
        CONFIRMATION_PREFIX, CONFIRMATION_TTL_SECONDS, ConfirmationEntry, DeleteMemoryConfirmation,
        MAX_CONFIRMATIONS, ManagementActor, ManagementOperation, MemoryCommitAudit,
        MemoryManagementError, MemoryOperationResult, PreparedMemoryOperation,
    },
};

pub(super) fn prepare(
    service: &MemoryManagementService,
    actor: ManagementActor,
    operation: &str,
    target_ref: &str,
    memory: Option<(&str, u64)>,
) -> Result<PreparedMemoryOperation, MemoryManagementError> {
    let operation = ManagementOperation::parse(operation)?;
    let mut target = service.resolve_target_ref(target_ref)?;
    if operation == ManagementOperation::DisableGroupProfile
        && target.target.memory_kind() != MemoryKind::GroupProfile
    {
        return Err(MemoryManagementError::Validation(
            "disable_group_profile requires a group profile target".to_owned(),
        ));
    }
    let delete = match operation {
        ManagementOperation::DeleteMemory => {
            let Some((memory_ref, expected_version)) = memory else {
                return Err(MemoryManagementError::Validation(
                    "delete_memory requires memory_ref and expected_version".to_owned(),
                ));
            };
            let record = service.resolve_memory_ref(&target, memory_ref)?;
            ensure_expected_version(&record, expected_version)?;
            if record.status != MemoryStatus::Active {
                return Err(MemoryManagementError::Conflict(
                    "only active memory can be permanently deleted".to_owned(),
                ));
            }
            let profile_enabled = service
                .store
                .management_snapshot(&target.target)
                .map_err(MemoryManagementError::from)?
                .profile_enabled
                .unwrap_or(true);
            if record.memory_kind == MemoryKind::GroupProfile && !profile_enabled {
                return Err(MemoryManagementError::ProfileDisabled);
            }
            Some(DeleteMemoryConfirmation {
                memory_ref: memory_ref.to_owned(),
                expected_version,
            })
        }
        _ => {
            if memory.is_some() {
                return Err(MemoryManagementError::Validation(
                    "memory_ref and expected_version are only valid for delete_memory".to_owned(),
                ));
            }
            None
        }
    };
    let snapshot = service
        .store
        .management_snapshot(&target.target)
        .map_err(MemoryManagementError::from)?;
    target.summary.capabilities =
        operation_capabilities(&target.target, snapshot.profile_enabled.unwrap_or(true));
    let expires_at = now_seconds().saturating_add(CONFIRMATION_TTL_SECONDS);
    let token = format!("{CONFIRMATION_PREFIX}{}", Uuid::new_v4());
    let digest = token_digest(&token);
    let affected_count = delete.as_ref().map_or(snapshot.active.len(), |_| 1);
    let entry = ConfirmationEntry {
        actor_id: actor.admin_id,
        session_digest: actor.session_digest,
        operation,
        target_ref: target.summary.target_ref.clone(),
        target: target.target,
        snapshot: snapshot.clone(),
        delete,
        expires_at,
    };
    let mut confirmations = service
        .confirmations
        .lock()
        .map_err(|_| MemoryManagementError::Internal)?;
    prune_confirmations(&mut confirmations);
    if confirmations.len() >= MAX_CONFIRMATIONS {
        return Err(MemoryManagementError::Conflict(
            "too many pending confirmations".to_owned(),
        ));
    }
    confirmations.insert(digest, entry);
    Ok(PreparedMemoryOperation {
        confirmation_token: token,
        operation: operation.as_str().to_owned(),
        target: target.summary,
        affected_count,
        expires_at,
    })
}

pub(super) fn commit(
    service: &MemoryManagementService,
    actor: ManagementActor,
    operation: &str,
    target_ref: &str,
    confirmation_token: &str,
    audit: impl Fn(&Transaction<'_>, MemoryCommitAudit<'_>) -> Result<(), MemoryManagementError>,
) -> Result<MemoryOperationResult, MemoryManagementError> {
    let operation = ManagementOperation::parse(operation)?;
    validate_confirmation_token(confirmation_token)?;
    let digest = token_digest(confirmation_token);
    let entry = {
        let mut confirmations = service
            .confirmations
            .lock()
            .map_err(|_| MemoryManagementError::Internal)?;
        prune_confirmations(&mut confirmations);
        let entry = confirmations
            .get(&digest)
            .cloned()
            .ok_or(MemoryManagementError::NotFound)?;
        if entry.actor_id != actor.admin_id
            || entry.session_digest != actor.session_digest
            || entry.operation != operation
            || entry.target_ref != target_ref
        {
            return Err(MemoryManagementError::PermissionDenied);
        }
        // 正确绑定的 token 在领域执行前消费；冲突、目标漂移或数据库失败都不可重放。
        confirmations.remove(&digest);
        entry
    };
    let mut target = service.resolve_target_ref(target_ref)?;
    if target.target != entry.target {
        return Err(MemoryManagementError::Conflict(
            "memory target changed after confirmation was prepared".to_owned(),
        ));
    }
    let (affected_count, profile_enabled, memory_ref, deleted) = match operation {
        ManagementOperation::ClearTarget => {
            let mutation = service
                .store
                .management_clear_if_unchanged_with_audit(
                    &target.target,
                    &entry.snapshot.active,
                    |tx, version| {
                        audit(
                            tx,
                            MemoryCommitAudit {
                                before_version: None,
                                after_version: version,
                                memory_ref: None,
                            },
                        )
                        .map_err(|error| super::super::MemoryError::audit_failed(error.message()))
                    },
                )
                .map_err(MemoryManagementError::from)?;
            (
                mutation.affected_count,
                mutation.profile_enabled,
                None,
                None,
            )
        }
        ManagementOperation::DisableGroupProfile => {
            let mutation = service
                .store
                .management_disable_group_profile_if_unchanged_with_audit(
                    &target.target,
                    entry.snapshot.profile_enabled.unwrap_or(true),
                    &entry.snapshot.active,
                    |tx, version| {
                        audit(
                            tx,
                            MemoryCommitAudit {
                                before_version: None,
                                after_version: version,
                                memory_ref: None,
                            },
                        )
                        .map_err(|error| super::super::MemoryError::audit_failed(error.message()))
                    },
                )
                .map_err(MemoryManagementError::from)?;
            (
                mutation.affected_count,
                mutation.profile_enabled,
                None,
                None,
            )
        }
        ManagementOperation::DeleteMemory => {
            let delete = entry
                .delete
                .as_ref()
                .ok_or(MemoryManagementError::Internal)?;
            let current = service.resolve_memory_ref(&target, &delete.memory_ref)?;
            ensure_expected_version(&current, delete.expected_version)?;
            if current.status != MemoryStatus::Active {
                return Err(MemoryManagementError::Conflict(
                    "only active memory can be permanently deleted".to_owned(),
                ));
            }
            let profile_enabled = service
                .store
                .management_snapshot(&target.target)
                .map_err(MemoryManagementError::from)?
                .profile_enabled
                .unwrap_or(true);
            if current.memory_kind == MemoryKind::GroupProfile && !profile_enabled {
                return Err(MemoryManagementError::ProfileDisabled);
            }
            let profile_enabled =
                service.delete_confirmed_with_audit(&target.target, &current, |tx, _| {
                    audit(
                        tx,
                        MemoryCommitAudit {
                            before_version: Some(delete.expected_version),
                            after_version: None,
                            memory_ref: Some(delete.memory_ref.as_str()),
                        },
                    )
                })?;
            (
                1,
                profile_enabled,
                Some(delete.memory_ref.clone()),
                Some(true),
            )
        }
    };
    target.summary.capabilities = operation_capabilities(&target.target, profile_enabled);
    Ok(MemoryOperationResult {
        operation: operation.as_str().to_owned(),
        target: target.summary,
        affected_count,
        capabilities: operation_capabilities(&target.target, profile_enabled),
        memory_ref,
        deleted,
    })
}
