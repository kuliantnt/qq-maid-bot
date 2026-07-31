use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::{
    http::{
        api::common::{
            ApiError, ApiRequestContext, PagedResponse, json_payload, respond, respond_error,
            respond_raw,
        },
        routes::OpsHttpState,
    },
    management::{ConsoleUserDataError, MAX_CONSOLE_FILE_BYTES},
};

use super::dto::{
    DeleteFileRequest, DeleteFileResponse, EmptyRequest, ListFilesRequest,
    UpdatePreferencesRequest, UserFileDto,
};

pub(super) async fn get_preferences(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<EmptyRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        json_payload(payload, &context)?;
        service(&state)?
            .get_preferences(context.actor.admin_id())
            .map_err(map_user_data_error)
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn update_preferences(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<UpdatePreferencesRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let patch = json_payload(payload, &context)?.into_patch()?;
        service(&state)?
            .update_preferences(context.actor.admin_id(), patch)
            .map_err(map_user_data_error)
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn upload_file(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = upload_payload(multipart, &state, context.actor.admin_id()).await;
    respond(&state, &headers, &context, result)
}

async fn upload_payload(
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
    state: &OpsHttpState,
    admin_id: i64,
) -> Result<UserFileDto, ApiError> {
    let mut multipart = multipart.map_err(|error| {
        ApiError::new(
            error.status(),
            "invalid_multipart",
            "request must use multipart/form-data with a valid boundary",
        )
    })?;
    let field = multipart
        .next_field()
        .await
        .map_err(map_multipart_error)?
        .ok_or_else(|| ApiError::validation("multipart field `file` is required"))?;
    if field.name() != Some("file") {
        return Err(ApiError::validation(
            "multipart request must contain exactly one field named `file`",
        ));
    }
    let filename = field
        .file_name()
        .map(str::to_owned)
        .ok_or_else(|| ApiError::validation("uploaded file must include a filename"))?;
    let content_type = field
        .content_type()
        .map(str::to_owned)
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    let bytes = field.bytes().await.map_err(map_multipart_error)?;
    if bytes.len() > MAX_CONSOLE_FILE_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("file must not exceed {MAX_CONSOLE_FILE_BYTES} bytes"),
        ));
    }
    if multipart
        .next_field()
        .await
        .map_err(map_multipart_error)?
        .is_some()
    {
        return Err(ApiError::validation(
            "multipart request must contain exactly one file",
        ));
    }
    service(state)?
        .create_file(admin_id, filename, content_type, bytes.to_vec())
        .map(UserFileDto::from)
        .map_err(map_user_data_error)
}

pub(super) async fn list_files(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<ListFilesRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        let pagination = request.pagination()?;
        let limit = usize::try_from(pagination.page_size())
            .map_err(|_| ApiError::validation("page_size is too large"))?;
        let offset = usize::try_from(pagination.offset())
            .map_err(|_| ApiError::validation("pagination offset is too large"))?;
        let page = service(&state)?
            .list_files(context.actor.admin_id(), limit, offset)
            .map_err(map_user_data_error)?;
        Ok(PagedResponse::new(
            page.items.into_iter().map(UserFileDto::from).collect(),
            pagination,
            page.total_count,
        ))
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn get_file(
    State(state): State<OpsHttpState>,
    Path(file_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = service(&state)
        .and_then(|service| {
            service
                .read_file(context.actor.admin_id(), &file_id)
                .map_err(map_user_data_error)
        })
        .and_then(|content| {
            let content_type = HeaderValue::from_str(&content.metadata.content_type)
                .map_err(|_| ApiError::internal("stored file content type is invalid"))?;
            let content_length = HeaderValue::from_str(&content.metadata.size.to_string())
                .map_err(|_| ApiError::internal("stored file size is invalid"))?;
            let mut response = Body::from(content.bytes).into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, content_type);
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, content_length);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            );
            Ok(response)
        });
    respond_raw(&state, &headers, &context, result)
}

pub(super) async fn delete_file(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<DeleteFileRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        service(&state)?
            .delete_file(context.actor.admin_id(), &request.file_id)
            .map_err(map_user_data_error)?;
        Ok(DeleteFileResponse {
            file_id: request.file_id,
            deleted: true,
        })
    })();
    respond(&state, &headers, &context, result)
}

fn service(state: &OpsHttpState) -> Result<&crate::management::ConsoleUserDataService, ApiError> {
    state.console_user_data.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "console_user_data_unavailable",
            "console user data service is unavailable",
        )
    })
}

fn map_user_data_error(error: ConsoleUserDataError) -> ApiError {
    match error.code() {
        "bad_request" => ApiError::validation(error.message()),
        "not_found" => ApiError::not_found("file not found"),
        _ => {
            tracing::error!(code = error.code(), "console user data operation failed");
            ApiError::internal("console user data service failed")
        }
    }
}

fn map_multipart_error(error: axum::extract::multipart::MultipartError) -> ApiError {
    let code = if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        "payload_too_large"
    } else {
        "invalid_multipart"
    };
    ApiError::new(error.status(), code, error.body_text())
}
