use serde::{Deserialize, Serialize};

use crate::{
    http::api::common::{ApiError, PaginationRequest, ValidatedPagination},
    runtime::tools::knowledge::{
        KnowledgeFileEntry, KnowledgeFileListQuery, KnowledgeFileSort, KnowledgeFileStatus,
    },
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyRequest {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListFilesRequest {
    #[serde(flatten)]
    pagination: PaginationRequest,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    order: Option<String>,
}

impl ListFilesRequest {
    pub(super) fn into_query(
        self,
    ) -> Result<(ValidatedPagination, KnowledgeFileListQuery), ApiError> {
        let pagination = self.pagination.validate()?;
        let search = self.search.unwrap_or_default();
        if search.chars().count() > 255 || search.chars().any(char::is_control) {
            return Err(ApiError::validation(
                "search must contain at most 255 non-control characters",
            ));
        }
        let status = match self
            .status
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            None => None,
            Some("pending") => Some(KnowledgeFileStatus::Pending),
            Some("processing") => Some(KnowledgeFileStatus::Processing),
            Some("ready") => Some(KnowledgeFileStatus::Ready),
            Some("failed") => Some(KnowledgeFileStatus::Failed),
            Some(_) => {
                return Err(ApiError::validation(
                    "status must be one of: pending, processing, ready, failed",
                ));
            }
        };
        let sort = match self.sort.as_deref().unwrap_or("updated_at") {
            "uploaded_at" => KnowledgeFileSort::UploadedAt,
            "updated_at" => KnowledgeFileSort::UpdatedAt,
            _ => {
                return Err(ApiError::validation(
                    "sort must be one of: uploaded_at, updated_at",
                ));
            }
        };
        let descending = match self.order.as_deref().unwrap_or("desc") {
            "asc" => false,
            "desc" => true,
            _ => return Err(ApiError::validation("order must be one of: asc, desc")),
        };
        Ok((
            pagination,
            KnowledgeFileListQuery {
                search,
                status,
                sort,
                descending,
                limit: usize::try_from(pagination.page_size())
                    .map_err(|_| ApiError::validation("page_size is too large"))?,
                offset: usize::try_from(pagination.offset())
                    .map_err(|_| ApiError::validation("pagination offset is too large"))?,
            },
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileIdRequest {
    pub file_id: String,
}

#[derive(Debug, Serialize)]
pub(super) struct KnowledgeFileDto {
    pub file_id: Option<String>,
    pub filename: String,
    pub content_type: String,
    pub size: Option<u64>,
    pub source: &'static str,
    pub source_label: String,
    pub status: &'static str,
    pub uploaded_at: Option<String>,
    pub processing_started_at: Option<String>,
    pub processed_at: Option<String>,
    pub updated_at: String,
    pub error_code: Option<String>,
    pub error_summary: Option<String>,
    pub chunk_count: Option<u64>,
    pub embedding_count: Option<u64>,
    pub downloadable: bool,
    pub download_url: Option<String>,
}

impl From<KnowledgeFileEntry> for KnowledgeFileDto {
    fn from(value: KnowledgeFileEntry) -> Self {
        let download_url = value
            .file_id
            .as_deref()
            .map(|file_id| format!("/api/v1/console/knowledge/files/get/{file_id}"));
        Self {
            downloadable: download_url.is_some(),
            file_id: value.file_id,
            filename: value.filename,
            content_type: value.content_type,
            size: value.size,
            source: value.source_kind,
            source_label: value.source_label,
            status: value.status.as_str(),
            uploaded_at: value.uploaded_at,
            processing_started_at: value.processing_started_at,
            processed_at: value.processed_at,
            updated_at: value.updated_at,
            error_code: value.error_code,
            error_summary: value.error_summary,
            chunk_count: value.chunk_count,
            embedding_count: value.embedding_count,
            download_url,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct KnowledgeCapabilitiesDto {
    pub supported_extensions: &'static [&'static str],
    pub max_file_bytes: usize,
    pub max_filename_chars: usize,
}
