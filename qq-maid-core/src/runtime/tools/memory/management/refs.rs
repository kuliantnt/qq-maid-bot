//! Memory 管理 opaque reference、target 回查和输入校验。

use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::identity::parse_stable_scope_key;

use super::super::{MemoryCategory, MemoryKind, MemoryRecord, MemoryStatus, MemoryTarget};
use super::types::*;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedTarget {
    pub(super) target: MemoryTarget,
    pub(super) summary: MemoryTargetSummary,
}

pub(super) fn resolved_target(
    target: MemoryTarget,
) -> Result<ResolvedTarget, MemoryManagementError> {
    let Some(parsed_scope) = parse_stable_scope_key(target.scope_id()) else {
        return Err(MemoryManagementError::NotFound);
    };
    if !safe_identity_segment(parsed_scope.platform, 64)
        || parsed_scope.platform == "unknown"
        || parsed_scope.account_id == "-"
        || !safe_identity_segment(parsed_scope.account_id, 256)
    {
        return Err(MemoryManagementError::NotFound);
    }
    let (group_ref, subject_ref) = match target.memory_kind() {
        MemoryKind::Personal if target.scope_type().as_str() == "personal" => {
            if target.subject_id().is_some() || parsed_scope.target_type != "private" {
                return Err(MemoryManagementError::NotFound);
            }
            (None, None)
        }
        MemoryKind::Group if target.scope_type().as_str() == "group" => {
            if target.subject_id().is_some() || parsed_scope.target_type != "group" {
                return Err(MemoryManagementError::NotFound);
            }
            (
                Some(identity_ref(
                    GROUP_REF_PREFIX,
                    &[
                        parsed_scope.platform,
                        parsed_scope.account_id,
                        parsed_scope.raw_target_id,
                    ],
                )),
                None,
            )
        }
        MemoryKind::GroupProfile if target.scope_type().as_str() == "group" => {
            if parsed_scope.target_type != "group" {
                return Err(MemoryManagementError::NotFound);
            }
            let subject_id = target.subject_id().ok_or(MemoryManagementError::NotFound)?;
            let Some(parsed_subject) = parse_stable_scope_key(subject_id) else {
                return Err(MemoryManagementError::NotFound);
            };
            if parsed_subject.target_type != "private"
                || parsed_subject.platform != parsed_scope.platform
                || parsed_subject.account_id != parsed_scope.account_id
                || !safe_identity_segment(parsed_subject.raw_target_id, 512)
            {
                return Err(MemoryManagementError::NotFound);
            }
            (
                Some(identity_ref(
                    GROUP_REF_PREFIX,
                    &[
                        parsed_scope.platform,
                        parsed_scope.account_id,
                        parsed_scope.raw_target_id,
                    ],
                )),
                Some(identity_ref(
                    SUBJECT_REF_PREFIX,
                    &[
                        parsed_subject.platform,
                        parsed_subject.account_id,
                        parsed_subject.raw_target_id,
                    ],
                )),
            )
        }
        _ => return Err(MemoryManagementError::NotFound),
    };
    let scope_type = target.scope_type().as_str().to_owned();
    let memory_kind = target.memory_kind().as_str().to_owned();
    let scope_id = target.scope_id().to_owned();
    let subject_id = target.subject_id().unwrap_or_default().to_owned();
    let platform = parsed_scope.platform.to_owned();
    let account_ref = identity_ref(ACCOUNT_REF_PREFIX, &[&platform, parsed_scope.account_id]);
    let target_ref = identity_ref(
        TARGET_REF_PREFIX,
        &[&scope_type, &memory_kind, &scope_id, &subject_id],
    );
    Ok(ResolvedTarget {
        target,
        summary: MemoryTargetSummary {
            target_ref,
            scope: memory_kind,
            platform,
            account_ref,
            group_ref,
            subject_ref,
        },
    })
}

pub(super) fn target_matches_filter(target: &ResolvedTarget, filter: &MemoryTargetFilter) -> bool {
    filter
        .scope
        .is_none_or(|scope| target.target.memory_kind() == scope)
        && filter
            .platform
            .as_deref()
            .is_none_or(|platform| target.summary.platform == platform)
        && filter
            .account_ref
            .as_deref()
            .is_none_or(|value| target.summary.account_ref == value)
        && filter
            .group_ref
            .as_deref()
            .is_none_or(|value| target.summary.group_ref.as_deref() == Some(value))
        && filter
            .subject_ref
            .as_deref()
            .is_none_or(|value| target.summary.subject_ref.as_deref() == Some(value))
}

pub(super) fn validate_target_filter(
    filter: &MemoryTargetFilter,
) -> Result<(), MemoryManagementError> {
    if let Some(platform) = filter.platform.as_deref()
        && !safe_identity_segment(platform, 64)
    {
        return Err(MemoryManagementError::Validation(
            "platform filter is invalid".to_owned(),
        ));
    }
    for (value, prefix) in [
        (filter.account_ref.as_deref(), ACCOUNT_REF_PREFIX),
        (filter.group_ref.as_deref(), GROUP_REF_PREFIX),
        (filter.subject_ref.as_deref(), SUBJECT_REF_PREFIX),
    ] {
        if let Some(value) = value {
            validate_ref(value, prefix)?;
        }
    }
    if filter.scope == Some(MemoryKind::LegacyUnassigned) {
        return Err(MemoryManagementError::Validation(
            "legacy_unassigned is not a manageable memory scope".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_list_filter(filter: &MemoryListFilter) -> Result<(), MemoryManagementError> {
    validate_target_filter(&MemoryTargetFilter {
        scope: filter.scope,
        platform: filter.platform.clone(),
        account_ref: filter.account_ref.clone(),
        group_ref: filter.group_ref.clone(),
        subject_ref: filter.subject_ref.clone(),
    })?;
    if let Some(target_ref) = filter.target_ref.as_deref() {
        validate_ref(target_ref, TARGET_REF_PREFIX)?;
    }
    Ok(())
}

pub(super) fn normalize_keyword(
    keyword: Option<String>,
) -> Result<Option<String>, MemoryManagementError> {
    let keyword = keyword
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if keyword
        .as_deref()
        .is_some_and(|value| value.chars().count() > MAX_KEYWORD_CHARS)
    {
        return Err(MemoryManagementError::Validation(
            "keyword is too long".to_owned(),
        ));
    }
    Ok(keyword)
}

pub(super) fn validate_content(content: &str) -> Result<(), MemoryManagementError> {
    if content.trim().is_empty() {
        return Err(MemoryManagementError::Validation(
            "content is required".to_owned(),
        ));
    }
    if content.chars().count() > MAX_CONTENT_CHARS {
        return Err(MemoryManagementError::Validation(
            "content is too long".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn ensure_expected_version(
    record: &MemoryRecord,
    expected_version: u64,
) -> Result<(), MemoryManagementError> {
    if record.revision == expected_version {
        Ok(())
    } else {
        Err(MemoryManagementError::Conflict(
            "memory version is stale; refresh and retry".to_owned(),
        ))
    }
}

pub(super) fn memory_item(
    target: &ResolvedTarget,
    record: MemoryRecord,
    profile_enabled: bool,
) -> Result<MemoryManagementItem, MemoryManagementError> {
    let category = record
        .memory_type
        .parse::<MemoryCategory>()
        .map(MemoryCategory::as_str)
        .map(str::to_owned)
        .map_err(|_| MemoryManagementError::NotFound)?;
    let active = record.status == MemoryStatus::Active;
    let profile_allowed = record.memory_kind != MemoryKind::GroupProfile || profile_enabled;
    Ok(MemoryManagementItem {
        memory_ref: memory_ref_for(&target.summary.target_ref, &record.id),
        version: record.revision,
        target: target.summary.clone(),
        content: record.content,
        kind: record.memory_kind.as_str().to_owned(),
        category,
        visibility: record.visibility.as_str().to_owned(),
        status: record.status.as_str().to_owned(),
        pinned: record.pinned,
        created_at: record.created_at,
        updated_at: record.updated_at,
        last_confirmed_at: record.last_confirmed_at,
        source_type: record.source_type.as_str().to_owned(),
        capabilities: MemoryCapabilities {
            can_update: active && profile_allowed,
            can_archive: active,
            can_restore: !active && profile_allowed,
        },
    })
}

pub(super) fn operation_capabilities(
    target: &MemoryTarget,
    profile_enabled: bool,
) -> MemoryOperationCapabilities {
    MemoryOperationCapabilities {
        can_clear_target: true,
        can_disable_group_profile: target.memory_kind() == MemoryKind::GroupProfile
            && profile_enabled,
    }
}

pub(super) fn record_matches_target(record: &MemoryRecord, target: &MemoryTarget) -> bool {
    record.scope_type == target.scope_type().as_str()
        && record.scope_id.as_deref() == Some(target.scope_id())
        && record.memory_kind == target.memory_kind()
        && record.subject_id.as_deref() == target.subject_id()
}

pub(super) fn memory_ref_for(target_ref: &str, id: &str) -> String {
    identity_ref(MEMORY_REF_PREFIX, &[target_ref, id])
}

pub(super) fn identity_ref(prefix: &str, values: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"qq-maid-memory-reference-v1\0");
    digest.update(prefix.as_bytes());
    for value in values {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    let digest = digest.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    format!("{prefix}{encoded}")
}

pub(super) fn validate_ref(value: &str, prefix: &str) -> Result<(), MemoryManagementError> {
    let digest = value.strip_prefix(prefix).filter(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if digest.is_some() {
        Ok(())
    } else {
        Err(MemoryManagementError::Validation(
            "opaque reference format is invalid".to_owned(),
        ))
    }
}

pub(super) fn validate_confirmation_token(value: &str) -> Result<(), MemoryManagementError> {
    let value = value
        .strip_prefix(CONFIRMATION_PREFIX)
        .filter(|value| uuid::Uuid::parse_str(value).is_ok());
    value.map(|_| ()).ok_or_else(|| {
        MemoryManagementError::Validation("confirmation token is invalid".to_owned())
    })
}

pub(super) fn token_digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

pub(super) fn prune_confirmations(
    confirmations: &mut std::collections::HashMap<[u8; 32], ConfirmationEntry>,
) {
    let now = now_seconds();
    confirmations.retain(|_, entry| entry.expires_at > now);
}

pub(super) fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn safe_identity_segment(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b':')
}
