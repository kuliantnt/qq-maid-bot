//! Todo 管理 API Handler；只负责认证、DTO 映射和领域错误到 API 错误的转换。

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::HeaderMap,
    response::Response,
};

use crate::{
    http::{
        api::common::{
            ApiError, ApiRequestContext, PagedResponse, json_payload, respond, respond_error,
        },
        routes::OpsHttpState,
    },
    runtime::tools::todo::TodoManagementError,
};

use super::dto::{
    CreateTodoRequest, DeleteTodoRequest, DeleteTodoResponse, GetTodoRequest, ListTodoRequest,
    ListTodoTargetsRequest, TodoDto, TodoTargetDiscoveryDto, UpdateTodoRequest,
};

pub(super) async fn create(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<CreateTodoRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    tracing::debug!(
        actor = context.actor.subject(),
        action = "create",
        "Todo 管理请求已认证"
    );
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let (target_ref, draft) = request.into_parts()?;
        service(&state)?
            .create(&target_ref, draft)
            .map(TodoDto::from)
            .map_err(map_todo_error)
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn list(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<ListTodoRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    tracing::debug!(
        actor = context.actor.subject(),
        action = "list",
        "Todo 管理请求已认证"
    );
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let pagination = request.pagination()?;
        let (query, filter) = request.into_parts(pagination)?;
        let page = service(&state)?
            .list(&query, filter)
            .map_err(map_todo_error)?;
        let total = u64::try_from(page.total_count)
            .map_err(|_| ApiError::internal("todo count overflow"))?;
        Ok(PagedResponse::new(
            page.items.into_iter().map(TodoDto::from).collect(),
            pagination,
            total,
        ))
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn targets(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<ListTodoTargetsRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    tracing::debug!(
        actor = context.actor.subject(),
        action = "targets",
        "Todo 管理请求已认证"
    );
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let pagination = request.pagination()?;
        let filter = request.into_filter()?;
        let limit = usize::try_from(pagination.page_size())
            .map_err(|_| ApiError::validation("page_size is too large"))?;
        let offset = usize::try_from(pagination.offset())
            .map_err(|_| ApiError::validation("pagination offset is too large"))?;
        let page = service(&state)?
            .targets(filter, limit, offset)
            .map_err(map_todo_error)?;
        let items = page
            .items
            .into_iter()
            .map(TodoTargetDiscoveryDto::try_from_target)
            .collect::<Result<Vec<_>, _>>()?;
        let total = u64::try_from(page.total_count)
            .map_err(|_| ApiError::internal("todo target count overflow"))?;
        Ok(PagedResponse::new(items, pagination, total))
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn get(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<GetTodoRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    tracing::debug!(
        actor = context.actor.subject(),
        action = "get",
        "Todo 管理请求已认证"
    );
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let id = request.id.into_string()?;
        service(&state)?
            .get(&id)
            .map(TodoDto::from)
            .map_err(map_todo_error)
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn update(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<UpdateTodoRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    tracing::debug!(
        actor = context.actor.subject(),
        action = "update",
        "Todo 管理请求已认证"
    );
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let (id, update) = request.into_parts()?;
        service(&state)?
            .update(&id, update)
            .map(TodoDto::from)
            .map_err(map_todo_error)
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn delete(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<DeleteTodoRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    tracing::debug!(
        actor = context.actor.subject(),
        action = "delete",
        "Todo 管理请求已认证"
    );
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let id = request.id.into_string()?;
        service(&state)?.delete(&id).map_err(map_todo_error)?;
        Ok(DeleteTodoResponse { id, deleted: true })
    })();
    respond(&state, &headers, &context, result)
}

fn service(
    state: &OpsHttpState,
) -> Result<&crate::runtime::tools::todo::TodoManagementService, ApiError> {
    state.todo_management.as_ref().ok_or_else(|| {
        ApiError::unavailable("todo_unavailable", "todo management service is unavailable")
    })
}

fn map_todo_error(error: TodoManagementError) -> ApiError {
    match error.code() {
        "bad_request" => ApiError::validation(error.message()),
        "not_found" => ApiError::not_found("todo not found"),
        "permission_denied" => ApiError::forbidden("permission_denied", error.message()),
        "conflict" => ApiError::conflict(error.message()),
        _ => {
            tracing::error!(code = error.code(), "Todo 管理操作失败");
            ApiError::internal("todo service failed")
        }
    }
}
