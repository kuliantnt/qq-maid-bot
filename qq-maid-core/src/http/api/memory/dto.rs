//! Memory 管理 API DTO。
//!
//! `deny_unknown_fields` 是刻意的安全边界：scope key、raw account/group/user ID、
//! source detail 和角色字段不能通过浏览器请求进入管理领域。

use serde::Deserialize;
use serde_json::Value;

use crate::runtime::tools::memory::{MemoryListFilter, MemoryTargetFilter, MemoryUpdatePatch};

use super::super::common::{ApiError, PaginationRequest, ValidatedPagination};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListTargetsRequest {
    #[serde(flatten)]
    pub(super) pagination: PaginationRequest,
    pub(super) scope: Option<String>,
    pub(super) platform: Option<String>,
    pub(super) account_ref: Option<String>,
    pub(super) group_ref: Option<String>,
    pub(super) subject_ref: Option<String>,
}

impl ListTargetsRequest {
    pub(super) fn into_parts(self) -> Result<(ValidatedPagination, MemoryTargetFilter), ApiError> {
        let pagination = self.pagination.validate()?;
        Ok((
            pagination,
            MemoryTargetFilter {
                scope: parse_scope(self.scope)?,
                platform: optional_text(self.platform, "platform", 64)?,
                account_ref: optional_text(self.account_ref, "account_ref", 96)?,
                group_ref: optional_text(self.group_ref, "group_ref", 96)?,
                subject_ref: optional_text(self.subject_ref, "subject_ref", 96)?,
            },
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListMemoriesRequest {
    #[serde(flatten)]
    pub(super) pagination: PaginationRequest,
    pub(super) target_ref: Option<String>,
    pub(super) scope: Option<String>,
    pub(super) platform: Option<String>,
    pub(super) account_ref: Option<String>,
    pub(super) group_ref: Option<String>,
    pub(super) subject_ref: Option<String>,
    pub(super) category: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) status: Option<String>,
    pub(super) visibility: Option<String>,
    pub(super) pinned: Option<bool>,
    pub(super) keyword: Option<String>,
}

impl ListMemoriesRequest {
    pub(super) fn into_parts(self) -> Result<(ValidatedPagination, MemoryListFilter), ApiError> {
        let pagination = self.pagination.validate()?;
        let scope = parse_scope(self.scope)?;
        let kind = parse_scope(self.kind)?;
        if scope.is_some() && kind.is_some() && scope != kind {
            return Err(ApiError::validation("scope and kind must match"));
        }
        Ok((
            pagination,
            MemoryListFilter {
                target_ref: optional_text(self.target_ref, "target_ref", 96)?,
                scope: scope.or(kind),
                platform: optional_text(self.platform, "platform", 64)?,
                account_ref: optional_text(self.account_ref, "account_ref", 96)?,
                group_ref: optional_text(self.group_ref, "group_ref", 96)?,
                subject_ref: optional_text(self.subject_ref, "subject_ref", 96)?,
                category: parse_enum(self.category, "category")?,
                status: parse_enum(self.status, "status")?,
                visibility: parse_enum(self.visibility, "visibility")?,
                pinned: self.pinned,
                keyword: self.keyword,
            },
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GetMemoryRequest {
    pub(super) target_ref: String,
    pub(super) memory_ref: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateMemoryRequest {
    pub(super) target_ref: String,
    pub(super) content: String,
    pub(super) category: String,
    pub(super) visibility: String,
    #[serde(default)]
    pub(super) pinned: bool,
    pub(super) attribute_key: Option<String>,
}

impl CreateMemoryRequest {
    pub(super) fn into_parts(
        self,
    ) -> Result<crate::runtime::tools::memory::MemoryCreateInput, ApiError> {
        Ok(crate::runtime::tools::memory::MemoryCreateInput {
            target_ref: required_text(self.target_ref, "target_ref", 96)?,
            content: self.content,
            category: parse_required_enum(self.category, "category")?,
            visibility: parse_required_enum(self.visibility, "visibility")?,
            pinned: self.pinned,
            attribute_key: self.attribute_key,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryPatchRequest {
    pub(super) target_ref: String,
    pub(super) memory_ref: String,
    pub(super) expected_version: Value,
    pub(super) patch: MemoryPatch,
}

impl MemoryPatchRequest {
    pub(super) fn into_parts(self) -> Result<(String, String, u64, MemoryUpdatePatch), ApiError> {
        Ok((
            required_text(self.target_ref, "target_ref", 96)?,
            required_text(self.memory_ref, "memory_ref", 96)?,
            parse_version(self.expected_version)?,
            self.patch.into_domain()?,
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryPatch {
    #[serde(default)]
    pub(super) content: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) visibility: Option<String>,
    #[serde(default)]
    pub(super) pinned: Option<bool>,
    #[serde(default)]
    pub(super) attribute_key: Option<Option<String>>,
}

impl MemoryPatch {
    fn into_domain(self) -> Result<MemoryUpdatePatch, ApiError> {
        Ok(MemoryUpdatePatch {
            content: self.content,
            category: parse_enum(self.category, "category")?,
            visibility: parse_enum(self.visibility, "visibility")?,
            pinned: self.pinned,
            attribute_key: self.attribute_key,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VersionedMemoryRequest {
    pub(super) target_ref: String,
    pub(super) memory_ref: String,
    pub(super) expected_version: Value,
}

impl VersionedMemoryRequest {
    pub(super) fn into_parts(self) -> Result<(String, String, u64), ApiError> {
        Ok((
            required_text(self.target_ref, "target_ref", 96)?,
            required_text(self.memory_ref, "memory_ref", 96)?,
            parse_version(self.expected_version)?,
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareOperationRequest {
    pub(super) operation: String,
    pub(super) target_ref: String,
    pub(super) memory_ref: Option<String>,
    pub(super) expected_version: Option<Value>,
}

type PrepareOperationParts = (String, String, Option<(String, u64)>);

impl PrepareOperationRequest {
    pub(super) fn into_parts(self) -> Result<PrepareOperationParts, ApiError> {
        let memory_ref = self
            .memory_ref
            .map(|value| required_text(value, "memory_ref", 96))
            .transpose()?;
        let expected_version = self.expected_version.map(parse_version).transpose()?;
        match (memory_ref, expected_version) {
            (Some(memory_ref), Some(expected_version)) => Ok((
                required_text(self.operation, "operation", 64)?,
                required_text(self.target_ref, "target_ref", 96)?,
                Some((memory_ref, expected_version)),
            )),
            (None, None) => Ok((
                required_text(self.operation, "operation", 64)?,
                required_text(self.target_ref, "target_ref", 96)?,
                None,
            )),
            _ => Err(ApiError::validation(
                "memory_ref and expected_version must be provided together",
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommitOperationRequest {
    pub(super) operation: String,
    pub(super) target_ref: String,
    pub(super) confirmation_token: String,
}

impl CommitOperationRequest {
    pub(super) fn into_parts(self) -> Result<(String, String, String), ApiError> {
        Ok((
            required_text(self.operation, "operation", 64)?,
            required_text(self.target_ref, "target_ref", 96)?,
            required_text(self.confirmation_token, "confirmation_token", 128)?,
        ))
    }
}

fn parse_scope(
    value: Option<String>,
) -> Result<Option<crate::runtime::tools::memory::MemoryKind>, ApiError> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    value
        .parse()
        .map(Some)
        .map_err(|_| ApiError::validation("scope is invalid"))
}

fn parse_enum<T>(value: Option<String>, field: &str) -> Result<Option<T>, ApiError>
where
    T: std::str::FromStr,
{
    value
        .map(|value| {
            value
                .trim()
                .parse()
                .map_err(|_| ApiError::validation(format!("{field} is invalid")))
        })
        .transpose()
}

fn parse_required_enum<T>(value: String, field: &str) -> Result<T, ApiError>
where
    T: std::str::FromStr,
{
    value
        .trim()
        .parse()
        .map_err(|_| ApiError::validation(format!("{field} is invalid")))
}

fn parse_version(value: Value) -> Result<u64, ApiError> {
    match value {
        Value::Number(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| ApiError::validation("expected_version must be positive")),
        Value::String(value) => {
            let value = value.trim();
            if value.is_empty() || value.starts_with('0') {
                return Err(ApiError::validation("expected_version must be positive"));
            }
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| ApiError::validation("expected_version must be positive"))
        }
        _ => Err(ApiError::validation("expected_version must be positive")),
    }
}

fn required_text(value: String, field: &str, max_len: usize) -> Result<String, ApiError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(ApiError::validation(format!("{field} is required")));
    }
    if value.chars().count() > max_len {
        return Err(ApiError::validation(format!("{field} is too long")));
    }
    Ok(value)
}

fn optional_text(
    value: Option<String>,
    field: &str,
    max_len: usize,
) -> Result<Option<String>, ApiError> {
    value
        .map(|value| required_text(value, field, max_len))
        .transpose()
}
