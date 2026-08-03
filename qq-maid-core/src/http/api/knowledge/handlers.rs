use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::{
    http::api::common::{
        ApiError, ApiRequestContext, PagedResponse, json_payload, respond, respond_error,
        respond_raw,
    },
    http::routes::OpsHttpState,
    management::{MAX_ORIGINAL_FILENAME_CHARS, UserFileContent},
    runtime::tools::knowledge::{KnowledgeFileError, KnowledgeFileService},
};

use super::dto::{
    EmptyRequest, FileIdRequest, KnowledgeCapabilitiesDto, KnowledgeFileDto, ListFilesRequest,
};

pub(super) async fn capabilities(
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
        let service = service(&state)?;
        Ok(KnowledgeCapabilitiesDto {
            supported_extensions: &[".md", ".markdown"],
            max_file_bytes: service.max_file_bytes(),
            max_filename_chars: MAX_ORIGINAL_FILENAME_CHARS,
        })
    })();
    respond(&state, &headers, &context, result)
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
        let (pagination, query) = json_payload(payload, &context)?.into_query()?;
        let page = service(&state)?
            .list(context.actor.admin_id(), &query)
            .map_err(map_knowledge_error)?;
        Ok(PagedResponse::new(
            page.items.into_iter().map(KnowledgeFileDto::from).collect(),
            pagination,
            page.total_count,
        ))
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
) -> Result<KnowledgeFileDto, ApiError> {
    let mut multipart = multipart.map_err(map_multipart_rejection)?;
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
        .unwrap_or_else(|| "text/markdown".to_owned());
    let bytes = field.bytes().await.map_err(map_multipart_error)?;
    let max_bytes = service(state)?.max_file_bytes();
    if bytes.len() > max_bytes {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "knowledge_file_too_large",
            format!("knowledge file must not exceed {max_bytes} bytes"),
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
        .upload(admin_id, filename, content_type, bytes.to_vec())
        .map(KnowledgeFileDto::from)
        .map_err(map_knowledge_error)
}

pub(super) async fn get_file(
    State(state): State<OpsHttpState>,
    Path(file_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let context = match ApiRequestContext::authenticate_read_only(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = service(&state)
        .and_then(|service| {
            service
                .read(context.actor.admin_id(), &file_id)
                .map_err(map_knowledge_error)
        })
        .and_then(file_response);
    respond_raw(&state, &headers, &context, result)
}

pub(super) async fn delete_file(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<FileIdRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        service(&state)?
            .delete(context.actor.admin_id(), &request.file_id)
            .map_err(map_knowledge_error)?;
        Ok(serde_json::json!({
            "file_id": request.file_id,
            "deleted": true,
        }))
    })();
    respond(&state, &headers, &context, result)
}

pub(super) async fn retry_file(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    payload: Result<Json<FileIdRequest>, JsonRejection>,
) -> Response {
    let context = match ApiRequestContext::authenticate(&state, &headers) {
        Ok(context) => context,
        Err(error) => return respond_error(&state, &headers, error),
    };
    let result = (|| {
        let request = json_payload(payload, &context)?;
        service(&state)?
            .retry(context.actor.admin_id(), &request.file_id)
            .map(KnowledgeFileDto::from)
            .map_err(map_knowledge_error)
    })();
    respond(&state, &headers, &context, result)
}

fn file_response(content: UserFileContent) -> Result<Response, ApiError> {
    let content_type = HeaderValue::from_str(&content.metadata.content_type)
        .map_err(|_| ApiError::internal("stored file content type is invalid"))?;
    let content_length = HeaderValue::from_str(&content.metadata.size.to_string())
        .map_err(|_| ApiError::internal("stored file size is invalid"))?;
    let disposition = HeaderValue::from_str(&content_disposition(&content.metadata.filename))
        .map_err(|_| ApiError::internal("stored file filename is invalid"))?;
    let mut response = Body::from(content.bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, content_length);
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(response)
}

pub(super) fn content_disposition(filename: &str) -> String {
    let fallback = filename
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') {
                char::from(byte)
            } else {
                '_'
            }
        })
        .take(96)
        .collect::<String>();
    let fallback = if fallback.is_empty() {
        "download.md"
    } else {
        fallback.as_str()
    };
    let encoded = filename
        .as_bytes()
        .iter()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'&'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
            {
                format!("{}", char::from(*byte)).into_bytes()
            } else {
                format!("%{byte:02X}").into_bytes()
            }
        })
        .map(char::from)
        .collect::<String>();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

fn service(state: &OpsHttpState) -> Result<&KnowledgeFileService, ApiError> {
    state.knowledge_files.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "knowledge_files_unavailable",
            "knowledge file service is unavailable",
        )
    })
}

fn map_knowledge_error(error: KnowledgeFileError) -> ApiError {
    match error.code() {
        "bad_request" => ApiError::validation(error.message()),
        "not_found" => ApiError::not_found("knowledge file not found"),
        "conflict" => ApiError::conflict(error.message()),
        "payload_too_large" => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "knowledge_file_too_large",
            error.message(),
        ),
        _ => {
            tracing::error!(error_code = error.code(), "knowledge file operation failed");
            ApiError::internal("knowledge file service failed")
        }
    }
}

fn map_multipart_error(error: axum::extract::multipart::MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "knowledge_file_too_large",
            "knowledge multipart body exceeds the configured limit",
        )
    } else {
        ApiError::new(
            error.status(),
            "invalid_multipart",
            "request must use multipart/form-data with a valid boundary",
        )
    }
}

fn map_multipart_rejection(error: axum::extract::multipart::MultipartRejection) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "knowledge_file_too_large",
            "knowledge multipart body exceeds the configured limit",
        )
    } else {
        ApiError::new(
            error.status(),
            "invalid_multipart",
            "request must use multipart/form-data with a valid boundary",
        )
    }
}
