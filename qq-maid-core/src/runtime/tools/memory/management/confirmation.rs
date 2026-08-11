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
        CONFIRMATION_PREFIX, CONFIRMATION_TTL_SECONDS, ConfirmationEntry, ConfirmationPayload,
        DeleteMemoryConfirmation, MAX_CONFIRMATIONS, ManagementActor, ManagementOperation,
        MemoryCommitAudit, MemoryManagementError, MemoryOperationResult, PreparedMemoryOperation,
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
    let (payload, affected_count, profile_enabled) = match operation {
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
                .management_profile_enabled(&target.target)
                .map_err(MemoryManagementError::from)?;
            if record.memory_kind == MemoryKind::GroupProfile && !profile_enabled {
                return Err(MemoryManagementError::ProfileDisabled);
            }
            (
                ConfirmationPayload::Delete(DeleteMemoryConfirmation {
                    memory_ref: memory_ref.to_owned(),
                    expected_version,
                }),
                1,
                profile_enabled,
            )
        }
        ManagementOperation::ClearTarget | ManagementOperation::DisableGroupProfile => {
            if memory.is_some() {
                return Err(MemoryManagementError::Validation(
                    "memory_ref and expected_version are only valid for delete_memory".to_owned(),
                ));
            }
            let snapshot = service
                .store
                .management_snapshot(&target.target)
                .map_err(MemoryManagementError::from)?;
            let profile_enabled = snapshot.profile_enabled.unwrap_or(true);
            (
                ConfirmationPayload::Bulk {
                    target: target.target.clone(),
                    snapshot: snapshot.clone(),
                },
                snapshot.active.len(),
                profile_enabled,
            )
        }
    };
    target.summary.capabilities = operation_capabilities(&target.target, profile_enabled);
    let expires_at = now_seconds().saturating_add(CONFIRMATION_TTL_SECONDS);
    let token = format!("{CONFIRMATION_PREFIX}{}", Uuid::new_v4());
    let digest = token_digest(&token);
    let entry = ConfirmationEntry {
        actor_id: actor.admin_id,
        session_digest: actor.session_digest,
        operation,
        target_ref: target.summary.target_ref.clone(),
        payload,
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
    audit: impl Fn(&Transaction<'_>, MemoryCommitAudit) -> Result<(), MemoryManagementError>,
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
    let delete_audit = match &entry.payload {
        ConfirmationPayload::Delete(delete) => Some(MemoryCommitAudit {
            before_version: Some(delete.expected_version),
            after_version: None,
            memory_ref: Some(delete.memory_ref.clone()),
        }),
        ConfirmationPayload::Bulk { .. } => None,
    };
    let result = (|| {
        let mut target = service.resolve_target_ref(target_ref)?;
        let payload = entry.payload;
        let (affected_count, profile_enabled, memory_ref, deleted) = match operation {
            ManagementOperation::ClearTarget => {
                let ConfirmationPayload::Bulk {
                    target: expected_target,
                    snapshot,
                } = payload
                else {
                    return Err(MemoryManagementError::Internal);
                };
                if target.target != expected_target {
                    return Err(MemoryManagementError::Conflict(
                        "memory target changed after confirmation was prepared".to_owned(),
                    ));
                }
                let mutation = service
                    .store
                    .management_clear_if_unchanged_with_audit(
                        &target.target,
                        &snapshot.active,
                        |tx, version| {
                            audit(
                                tx,
                                MemoryCommitAudit {
                                    before_version: None,
                                    after_version: version,
                                    memory_ref: None,
                                },
                            )
                            .map_err(|error| {
                                super::super::MemoryError::audit_failed(error.message())
                            })
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
                let ConfirmationPayload::Bulk {
                    target: expected_target,
                    snapshot,
                } = payload
                else {
                    return Err(MemoryManagementError::Internal);
                };
                if target.target != expected_target {
                    return Err(MemoryManagementError::Conflict(
                        "memory target changed after confirmation was prepared".to_owned(),
                    ));
                }
                let mutation = service
                    .store
                    .management_disable_group_profile_if_unchanged_with_audit(
                        &target.target,
                        snapshot.profile_enabled.unwrap_or(true),
                        &snapshot.active,
                        |tx, version| {
                            audit(
                                tx,
                                MemoryCommitAudit {
                                    before_version: None,
                                    after_version: version,
                                    memory_ref: None,
                                },
                            )
                            .map_err(|error| {
                                super::super::MemoryError::audit_failed(error.message())
                            })
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
                let ConfirmationPayload::Delete(delete) = payload else {
                    return Err(MemoryManagementError::Internal);
                };
                let current = service.resolve_memory_ref(&target, &delete.memory_ref)?;
                ensure_expected_version(&current, delete.expected_version)?;
                if current.status != MemoryStatus::Active {
                    return Err(MemoryManagementError::Conflict(
                        "only active memory can be permanently deleted".to_owned(),
                    ));
                }
                let profile_enabled = service
                    .store
                    .management_profile_enabled(&target.target)
                    .map_err(MemoryManagementError::from)?;
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
                                memory_ref: Some(delete.memory_ref.clone()),
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
    })();
    result.map_err(|error| {
        if let Some(metadata) = delete_audit
            && !matches!(&error, MemoryManagementError::AuditUnavailable)
        {
            return MemoryManagementError::WithAudit {
                source: Box::new(error),
                metadata,
            };
        }
        error
    })
}
