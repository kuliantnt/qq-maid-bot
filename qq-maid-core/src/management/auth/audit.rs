//! 管理审计字段校验与 SQLite 写入。
//!
//! Memory 管理事务会把同一连接上的 transaction 传入这里；本模块不自行借连接，
//! 以便业务状态和审计事件能够一起提交或一起回滚。

use rusqlite::{Connection, params};

use super::{AdminAuthError, ManagementAudit, database_error, safe_audit_value, unix_seconds};

pub(super) struct ValidatedManagementAudit {
    actor_admin_id: i64,
    request_id: String,
    action: String,
    result: String,
    target_digest: Option<String>,
    before_version: Option<i64>,
    after_version: Option<i64>,
    safe_error_code: Option<String>,
}

pub(super) fn validate_management_audit(
    event: ManagementAudit<'_>,
) -> Result<ValidatedManagementAudit, AdminAuthError> {
    let ManagementAudit {
        actor_admin_id,
        request_id,
        action,
        result,
        target_digest,
        before_version,
        after_version,
        safe_error_code,
    } = event;
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        || !safe_audit_value(action)
        || !safe_audit_value(result)
        || target_digest.is_some_and(|value| {
            value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        || safe_error_code.is_some_and(|value| !safe_audit_value(value))
    {
        return Err(AdminAuthError::storage("invalid management audit metadata"));
    }
    let before_version = before_version
        .map(|value| i64::try_from(value).map_err(|_| ()))
        .transpose()
        .map_err(|_| AdminAuthError::storage("management audit version is too large"))?;
    let after_version = after_version
        .map(|value| i64::try_from(value).map_err(|_| ()))
        .transpose()
        .map_err(|_| AdminAuthError::storage("management audit version is too large"))?;
    Ok(ValidatedManagementAudit {
        actor_admin_id,
        request_id: request_id.to_owned(),
        action: action.to_owned(),
        result: result.to_owned(),
        target_digest: target_digest.map(str::to_owned),
        before_version,
        after_version,
        safe_error_code: safe_error_code.map(str::to_owned),
    })
}

pub(super) fn insert_management_audit(
    connection: &Connection,
    event: &ValidatedManagementAudit,
) -> Result<(), AdminAuthError> {
    connection
        .execute(
            "INSERT INTO console_audit_events
             (created_at, actor_admin_id, event_type, outcome, request_id,
              target_digest, before_version, after_version, safe_error_code)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                unix_seconds(),
                event.actor_admin_id,
                event.action,
                event.result,
                event.request_id,
                event.target_digest,
                event.before_version,
                event.after_version,
                event.safe_error_code,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}
