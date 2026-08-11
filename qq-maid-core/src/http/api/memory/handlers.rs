//! Memory 管理 API Handler。
//!
//! Handler 只做认证、请求 DTO 解析、管理 Service 调用、安全 DTO 返回和稳定错误映射；
//! target 权限、revision、快照和 Memory 生命周期全部留在领域 Service/storage。

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::HeaderMap,
    response::Response,
};
use rusqlite::Transaction;
use sha2::{Digest, Sha256};

use crate::{
    http::api::common::{
        ApiError, ApiRequestContext, PagedResponse, json_payload, respond, respond_error,
    },
    http::routes::OpsHttpState,
    management::ManagementAudit,
    runtime::tools::memory::{
        ManagementActor, MemoryCommitAudit, MemoryManagementError, MemoryManagementService,
    },
};

use super::dto::{
    CommitOperationRequest, CreateMemoryRequest, GetMemoryRequest, ListMemoriesRequest,
    ListTargetsRequest, MemoryPatchRequest, PrepareOperationRequest, VersionedMemoryRequest,
};

pub(super) async fn targets(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<ListTargetsRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let (pagination, filter) = request.into_parts()?;
        let service = service(&state)?;
        let limit = usize::try_from(pagination.page_size())
            .map_err(|_| ApiError::validation("page_size is too large"))?;
        let offset = usize::try_from(pagination.offset())
            .map_err(|_| ApiError::validation("pagination offset is too large"))?;
        let page = service
            .targets(filter, limit, offset)
            .map_err(map_memory_error)?;
        let total = u64::try_from(page.total_count)
            .map_err(|_| ApiError::internal("memory target count overflow"))?;
        Ok(PagedResponse::new(page.items, pagination, total))
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn list(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<ListMemoriesRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let (pagination, filter) = request.into_parts()?;
        let service = service(&state)?;
        let limit = usize::try_from(pagination.page_size())
            .map_err(|_| ApiError::validation("page_size is too large"))?;
        let offset = usize::try_from(pagination.offset())
            .map_err(|_| ApiError::validation("pagination offset is too large"))?;
        let page = service
            .list(filter, limit, offset)
            .map_err(map_memory_error)?;
        let total = u64::try_from(page.total_count)
            .map_err(|_| ApiError::internal("memory count overflow"))?;
        Ok(PagedResponse::new(page.items, pagination, total))
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn get(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<GetMemoryRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let target_ref = request.target_ref.trim().to_owned();
        let memory_ref = request.memory_ref.trim().to_owned();
        service(&state)?
            .get(&target_ref, &memory_ref)
            .map_err(map_memory_error)
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn create(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<CreateMemoryRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let target_ref = request.target_ref.trim().to_owned();
        let input = request.into_parts()?;
        let service_result = service(&state)?.create_with_audit(input, |transaction, version| {
            audit_success_in_transaction(
                &state,
                &context,
                transaction,
                "memory.create",
                Some(&target_ref),
                MemoryCommitAudit {
                    before_version: None,
                    after_version: version,
                    memory_ref: None,
                },
            )
        });
        finish_mutation(
            &state,
            &context,
            "memory.create",
            Some(&target_ref),
            MemoryCommitAudit::default(),
            service_result,
        )
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn update(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<MemoryPatchRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let (target_ref, memory_ref, expected_version, patch) = request.into_parts()?;
        let service_result = service(&state)?.update_with_audit(
            &target_ref,
            &memory_ref,
            expected_version,
            patch,
            |transaction, version| {
                audit_success_in_transaction(
                    &state,
                    &context,
                    transaction,
                    "memory.update",
                    Some(&target_ref),
                    MemoryCommitAudit {
                        before_version: Some(expected_version),
                        after_version: version,
                        memory_ref: None,
                    },
                )
            },
        );
        finish_mutation(
            &state,
            &context,
            "memory.update",
            Some(&target_ref),
            MemoryCommitAudit {
                before_version: Some(expected_version),
                ..MemoryCommitAudit::default()
            },
            service_result,
        )
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn archive(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<VersionedMemoryRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let (target_ref, memory_ref, expected_version) = request.into_parts()?;
        let service_result = service(&state)?.archive_with_audit(
            &target_ref,
            &memory_ref,
            expected_version,
            |transaction, version| {
                audit_success_in_transaction(
                    &state,
                    &context,
                    transaction,
                    "memory.archive",
                    Some(&target_ref),
                    MemoryCommitAudit {
                        before_version: Some(expected_version),
                        after_version: version,
                        memory_ref: None,
                    },
                )
            },
        );
        finish_mutation(
            &state,
            &context,
            "memory.archive",
            Some(&target_ref),
            MemoryCommitAudit {
                before_version: Some(expected_version),
                ..MemoryCommitAudit::default()
            },
            service_result,
        )
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn restore(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<VersionedMemoryRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let (target_ref, memory_ref, expected_version) = request.into_parts()?;
        let service_result = service(&state)?.restore_with_audit(
            &target_ref,
            &memory_ref,
            expected_version,
            |transaction, version| {
                audit_success_in_transaction(
                    &state,
                    &context,
                    transaction,
                    "memory.restore",
                    Some(&target_ref),
                    MemoryCommitAudit {
                        before_version: Some(expected_version),
                        after_version: version,
                        memory_ref: None,
                    },
                )
            },
        );
        finish_mutation(
            &state,
            &context,
            "memory.restore",
            Some(&target_ref),
            MemoryCommitAudit {
                before_version: Some(expected_version),
                ..MemoryCommitAudit::default()
            },
            service_result,
        )
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn prepare_operation(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<PrepareOperationRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let (operation, target_ref, memory) = request.into_parts()?;
        let action = memory_operation_action("prepare", &operation);
        let metadata = memory.as_ref().map_or_else(
            MemoryCommitAudit::default,
            |(memory_ref, expected_version)| MemoryCommitAudit {
                before_version: Some(*expected_version),
                after_version: None,
                memory_ref: Some(memory_ref.clone()),
            },
        );
        let actor = management_actor(&context);
        let result = service(&state)?
            .prepare(
                actor,
                &operation,
                &target_ref,
                memory
                    .as_ref()
                    .map(|(memory_ref, expected_version)| (memory_ref.as_str(), *expected_version)),
            )
            .map_err(map_memory_error);
        audit_action(
            &state,
            &context,
            action,
            Some(&target_ref),
            metadata,
            &result,
        )?;
        result
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn commit_operation(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<CommitOperationRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let (operation, target_ref, confirmation_token) = request.into_parts()?;
        let action = memory_operation_action("commit", &operation);
        let actor = management_actor(&context);
        let service_result = service(&state)?.commit_with_audit(
            actor,
            &operation,
            &target_ref,
            &confirmation_token,
            |transaction, metadata: MemoryCommitAudit| {
                audit_success_in_transaction(
                    &state,
                    &context,
                    transaction,
                    action,
                    Some(&target_ref),
                    metadata,
                )
            },
        );
        finish_mutation(
            &state,
            &context,
            action,
            Some(&target_ref),
            MemoryCommitAudit::default(),
            service_result,
        )
    })();
    respond(&state, &headers, &context, result)
}

fn service(state: &OpsHttpState) -> Result<&MemoryManagementService, ApiError> {
    state.memory_management.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "memory_unavailable",
            "memory management service is unavailable",
        )
    })
}

fn management_actor(context: &ApiRequestContext) -> ManagementActor {
    ManagementActor {
        admin_id: context.actor.admin_id(),
        session_digest: context.actor.session_digest(),
    }
}

fn map_memory_error(error: MemoryManagementError) -> ApiError {
    match error.code() {
        "validation_error" => ApiError::validation(error.message()),
        "not_found" => ApiError::not_found("memory not found"),
        "conflict" => ApiError::conflict(error.message()),
        "permission_denied" => ApiError::forbidden("permission_denied", error.message()),
        "profile_disabled" => ApiError::forbidden("profile_disabled", error.message()),
        "audit_unavailable" => ApiError::internal("management audit is unavailable"),
        _ => {
            tracing::error!(code = error.code(), "Memory 管理服务失败");
            ApiError::internal("memory service failed")
        }
    }
}

fn audit_success_in_transaction(
    state: &OpsHttpState,
    context: &ApiRequestContext,
    transaction: &Transaction<'_>,
    action: &str,
    target_ref: Option<&str>,
    metadata: MemoryCommitAudit,
) -> Result<(), MemoryManagementError> {
    let Some(auth) = state.admin_auth.as_ref() else {
        tracing::error!(action, "Memory 管理审计写入失败：管理员认证不可用");
        return Err(MemoryManagementError::AuditUnavailable);
    };
    let target_digest = target_ref.and_then(safe_target_digest);
    let resource_digest = metadata.memory_ref.as_deref().and_then(safe_memory_digest);
    auth.audit_management_in_transaction(
        transaction,
        ManagementAudit {
            actor_admin_id: context.actor.admin_id(),
            request_id: context.request_id.as_str(),
            action,
            result: "success",
            target_digest: target_digest.as_deref(),
            resource_digest: resource_digest.as_deref(),
            before_version: metadata.before_version,
            after_version: metadata.after_version,
            safe_error_code: None,
        },
    )
    .map_err(|error| {
        tracing::error!(code = error.code(), action, "Memory 管理审计写入失败");
        MemoryManagementError::AuditUnavailable
    })
}

/// 成功审计已经随业务事务原子提交；这里只有普通领域失败需要补写事务外 denied 审计。
fn finish_mutation<T>(
    state: &OpsHttpState,
    context: &ApiRequestContext,
    action: &str,
    target_ref: Option<&str>,
    metadata: MemoryCommitAudit,
    service_result: Result<T, MemoryManagementError>,
) -> Result<T, ApiError> {
    match service_result {
        Ok(value) => Ok(value),
        Err(error @ MemoryManagementError::AuditUnavailable) => Err(map_memory_error(error)),
        Err(error) => {
            let metadata = error.audit_metadata().unwrap_or(metadata);
            let result = Err(map_memory_error(error));
            audit_action(state, context, action, target_ref, metadata, &result)?;
            result
        }
    }
}

fn audit_action<T>(
    state: &OpsHttpState,
    context: &ApiRequestContext,
    action: &str,
    target_ref: Option<&str>,
    metadata: MemoryCommitAudit,
    result: &Result<T, ApiError>,
) -> Result<(), ApiError> {
    let Some(auth) = state.admin_auth.as_ref() else {
        return Err(ApiError::unavailable(
            "auth_unavailable",
            "administrator authentication is unavailable",
        ));
    };
    let target_digest = target_ref.and_then(safe_target_digest);
    let resource_digest = metadata.memory_ref.as_deref().and_then(safe_memory_digest);
    let outcome = if result.is_ok() { "success" } else { "denied" };
    let error_code = result.as_ref().err().map(|error| error.code());
    auth.audit_management(ManagementAudit {
        actor_admin_id: context.actor.admin_id(),
        request_id: context.request_id.as_str(),
        action,
        result: outcome,
        target_digest: target_digest.as_deref(),
        resource_digest: resource_digest.as_deref(),
        before_version: metadata.before_version,
        after_version: metadata.after_version,
        safe_error_code: error_code,
    })
    .map_err(|error| {
        tracing::error!(code = error.code(), "Memory 管理审计写入失败");
        ApiError::internal("management audit is unavailable")
    })
}

fn memory_operation_action(phase: &str, operation: &str) -> &'static str {
    match (phase, operation.trim()) {
        ("prepare", "clear_target") => "memory.clear_target_prepare",
        ("prepare", "disable_group_profile") => "memory.disable_group_profile_prepare",
        ("prepare", "delete_memory") => "memory.delete_prepare",
        ("commit", "clear_target") => "memory.clear_target_commit",
        ("commit", "disable_group_profile") => "memory.disable_group_profile_commit",
        ("commit", "delete_memory") => "memory.delete_commit",
        ("prepare", _) => "memory.operation_prepare",
        _ => "memory.operation_commit",
    }
}

fn safe_target_digest(value: &str) -> Option<String> {
    let (_, digest) = value.split_once(":v1:")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let digest = Sha256::digest(value.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    Some(encoded)
}

fn safe_memory_digest(value: &str) -> Option<String> {
    let suffix = value.strip_prefix("memory:v1:")?;
    if suffix.len() != 64
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let digest = Sha256::digest(value.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    Some(encoded)
}
