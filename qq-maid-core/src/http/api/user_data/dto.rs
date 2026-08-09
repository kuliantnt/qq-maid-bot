use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};

use crate::{
    http::api::common::{ApiError, PaginationRequest, ValidatedPagination},
    management::{BackgroundMode, PreferenceValuePatch, UserFile, UserPreferencesPatch},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyRequest {}

#[derive(Debug, Default)]
pub(super) enum PatchField<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for PatchField<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdatePreferencesRequest {
    #[serde(default)]
    custom_colors: PatchField<Vec<String>>,
    #[serde(default)]
    background_file_ids: PatchField<Vec<String>>,
    #[serde(default)]
    active_background_file_id: PatchField<String>,
    #[serde(default)]
    background_mode: PatchField<BackgroundMode>,
    #[serde(default)]
    kuliantnt: PatchField<bool>,
}

impl UpdatePreferencesRequest {
    pub(super) fn into_patch(self) -> Result<UserPreferencesPatch, ApiError> {
        Ok(UserPreferencesPatch {
            custom_colors: required_patch(self.custom_colors, "custom_colors")?,
            background_file_ids: required_patch(self.background_file_ids, "background_file_ids")?,
            active_background_file_id: match self.active_background_file_id {
                PatchField::Missing => PreferenceValuePatch::Unchanged,
                PatchField::Null => PreferenceValuePatch::Clear,
                PatchField::Value(value) => PreferenceValuePatch::Set(value),
            },
            background_mode: required_patch(self.background_mode, "background_mode")?,
            kuliantnt: required_patch(self.kuliantnt, "kuliantnt")?,
        })
    }
}

fn required_patch<T>(field: PatchField<T>, name: &str) -> Result<Option<T>, ApiError> {
    match field {
        PatchField::Missing => Ok(None),
        PatchField::Null => Err(ApiError::validation(format!("{name} must not be null"))),
        PatchField::Value(value) => Ok(Some(value)),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListFilesRequest {
    #[serde(flatten)]
    pagination: PaginationRequest,
}

impl ListFilesRequest {
    pub(super) fn pagination(&self) -> Result<ValidatedPagination, ApiError> {
        self.pagination.clone().validate()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteFileRequest {
    pub file_id: String,
}

#[derive(Debug, Serialize)]
pub(super) struct UserFileDto {
    pub file_id: String,
    pub filename: String,
    pub content_type: String,
    pub module: &'static str,
    pub size: u64,
    pub created_at: String,
    pub url: String,
}

impl From<UserFile> for UserFileDto {
    fn from(value: UserFile) -> Self {
        let url = format!("/api/v1/console/files/get/{}", value.file_id);
        Self {
            file_id: value.file_id,
            filename: value.filename,
            content_type: value.content_type,
            module: value.module.as_str(),
            size: value.size,
            created_at: value.created_at,
            url,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct DeleteFileResponse {
    pub file_id: String,
    pub deleted: bool,
}
