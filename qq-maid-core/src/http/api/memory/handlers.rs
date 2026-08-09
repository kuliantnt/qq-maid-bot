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
use sha2::{Digest, Sha256};

use crate::{
    http::api::common::{
        ApiError, ApiRequestContext, PagedResponse, json_payload, respond, respond_error,
    },
    http::routes::OpsHttpState,
    management::ManagementAudit,
    runtime::tools::memory::{ManagementActor, MemoryManagementError, MemoryManagementService},
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
        let result = service(&state)?.create(input).map_err(map_memory_error);
        audit_action(
            &state,
            &context,
            "memory.create",
            Some(&target_ref),
            None,
            result.as_ref().ok().map(|value| value.memory.version),
            &result,
        )?;
        result
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
        let result = service(&state)?
            .update(&target_ref, &memory_ref, expected_version, patch)
            .map_err(map_memory_error);
        audit_action(
            &state,
            &context,
            "memory.update",
            Some(&target_ref),
            Some(expected_version),
            result.as_ref().ok().map(|value| value.memory.version),
            &result,
        )?;
        result
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
        let result = service(&state)?
            .archive(&target_ref, &memory_ref, expected_version)
            .map_err(map_memory_error);
        audit_action(
            &state,
            &context,
            "memory.archive",
            Some(&target_ref),
            Some(expected_version),
            result.as_ref().ok().map(|value| value.memory.version),
            &result,
        )?;
        result
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
        let result = service(&state)?
            .restore(&target_ref, &memory_ref, expected_version)
            .map_err(map_memory_error);
        audit_action(
            &state,
            &context,
            "memory.restore",
            Some(&target_ref),
            Some(expected_version),
            result.as_ref().ok().map(|value| value.memory.version),
            &result,
        )?;
        result
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
        let (operation, target_ref) = request.into_parts()?;
        let actor = management_actor(&context);
        let result = service(&state)?
            .prepare(actor, &operation, &target_ref)
            .map_err(map_memory_error);
        audit_action(
            &state,
            &context,
            "memory.operation_prepare",
            Some(&target_ref),
            None,
            None,
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
        let actor = management_actor(&context);
        let result = service(&state)?
            .commit(actor, &operation, &target_ref, &confirmation_token)
            .map_err(map_memory_error);
        audit_action(
            &state,
            &context,
            "memory.operation_commit",
            Some(&target_ref),
            None,
            None,
            &result,
        )?;
        result
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
        _ => {
            tracing::error!(code = error.code(), "Memory 管理服务失败");
            ApiError::internal("memory service failed")
        }
    }
}

fn audit_action<T>(
    state: &OpsHttpState,
    context: &ApiRequestContext,
    action: &str,
    target_ref: Option<&str>,
    before_version: Option<u64>,
    after_version: Option<u64>,
    result: &Result<T, ApiError>,
) -> Result<(), ApiError> {
    let Some(auth) = state.admin_auth.as_ref() else {
        return Err(ApiError::unavailable(
            "auth_unavailable",
            "administrator authentication is unavailable",
        ));
    };
    let target_digest = target_ref.and_then(safe_target_digest);
    let outcome = if result.is_ok() { "success" } else { "denied" };
    let error_code = result.as_ref().err().map(|error| error.code());
    auth.audit_management(ManagementAudit {
        actor_admin_id: context.actor.admin_id(),
        request_id: context.request_id.as_str(),
        action,
        result: outcome,
        target_digest: target_digest.as_deref(),
        before_version,
        after_version,
        safe_error_code: error_code,
    })
    .map_err(|error| {
        tracing::error!(code = error.code(), "Memory 管理审计写入失败");
        ApiError::internal("management audit is unavailable")
    })
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
