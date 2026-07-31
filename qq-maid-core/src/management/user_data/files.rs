use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    ConsoleUserDataError, ConsoleUserDataService, MAX_CONSOLE_FILE_BYTES, MAX_CONTENT_TYPE_CHARS,
    MAX_ORIGINAL_FILENAME_CHARS, UserFile, UserFileContent, UserFilePage, now_rfc3339,
    preferences::read_preferences, preferences::write_cleaned_preferences, validate_file_id,
};

impl ConsoleUserDataService {
    pub fn create_file(
        &self,
        admin_id: i64,
        filename: String,
        content_type: String,
        bytes: Vec<u8>,
    ) -> Result<UserFile, ConsoleUserDataError> {
        validate_upload(&filename, &content_type, bytes.len())?;
        ensure_file_root(&self.file_root)?;

        let file_id = uuid::Uuid::new_v4().hyphenated().to_string();
        let storage_filename = format!("{}.blob", uuid::Uuid::new_v4().hyphenated());
        let temporary_filename = format!(".upload-{}.tmp", uuid::Uuid::new_v4().hyphenated());
        let temporary_path = self.file_root.join(&temporary_filename);
        let final_path = self.file_root.join(&storage_filename);
        write_new_file(&temporary_path, &bytes)?;
        if let Err(error) = fs::rename(&temporary_path, &final_path) {
            let _ = fs::remove_file(&temporary_path);
            return Err(ConsoleUserDataError::storage(format!(
                "failed to finalize uploaded file: {error}"
            )));
        }

        let file = UserFile {
            file_id,
            filename,
            content_type,
            size: u64::try_from(bytes.len())
                .map_err(|_| ConsoleUserDataError::invalid("file size is too large"))?,
            created_at: now_rfc3339(),
            storage_filename,
        };
        if let Err(error) = insert_file(&self.database, admin_id, &file) {
            if let Err(cleanup_error) = fs::remove_file(&final_path) {
                tracing::warn!(
                    error = %cleanup_error,
                    file_id = %file.file_id,
                    "上传元数据写入失败后无法清理磁盘文件"
                );
            }
            return Err(error);
        }
        Ok(file)
    }

    pub fn list_files(
        &self,
        admin_id: i64,
        limit: usize,
        offset: usize,
    ) -> Result<UserFilePage, ConsoleUserDataError> {
        let limit = i64::try_from(limit)
            .map_err(|_| ConsoleUserDataError::invalid("file list limit is too large"))?;
        let offset = i64::try_from(offset)
            .map_err(|_| ConsoleUserDataError::invalid("file list offset is too large"))?;
        let connection = self.database.connection().map_err(storage_error)?;
        let total: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM console_user_files WHERE admin_id = ?1",
                [admin_id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        let mut statement = connection
            .prepare(
                "SELECT file_id, original_filename, content_type, size,
                        created_at, storage_filename
                 FROM console_user_files
                 WHERE admin_id = ?1
                 ORDER BY created_at DESC, file_id DESC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(storage_error)?;
        let items = statement
            .query_map(params![admin_id, limit, offset], file_from_row)
            .map_err(storage_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)?;
        Ok(UserFilePage {
            items,
            total_count: u64::try_from(total)
                .map_err(|_| ConsoleUserDataError::storage("negative file count"))?,
        })
    }

    pub fn read_file(
        &self,
        admin_id: i64,
        file_id: &str,
    ) -> Result<UserFileContent, ConsoleUserDataError> {
        validate_file_id(file_id)?;
        let connection = self.database.connection().map_err(storage_error)?;
        let metadata = find_owned_file(&connection, admin_id, file_id)?
            .ok_or_else(|| ConsoleUserDataError::not_found("file not found"))?;
        let path = storage_path(&self.file_root, &metadata.storage_filename)?;
        let bytes = fs::read(&path).map_err(|error| {
            ConsoleUserDataError::storage(format!("failed to read stored file: {error}"))
        })?;
        if u64::try_from(bytes.len()).ok() != Some(metadata.size) {
            return Err(ConsoleUserDataError::storage(
                "stored file size does not match its metadata",
            ));
        }
        Ok(UserFileContent { metadata, bytes })
    }

    pub fn delete_file(&self, admin_id: i64, file_id: &str) -> Result<(), ConsoleUserDataError> {
        validate_file_id(file_id)?;
        let mut connection = self.database.connection().map_err(storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let metadata = find_owned_file(&transaction, admin_id, file_id)?
            .ok_or_else(|| ConsoleUserDataError::not_found("file not found"))?;
        let original_path = storage_path(&self.file_root, &metadata.storage_filename)?;
        let tombstone_path = self
            .file_root
            .join(format!(".delete-{}.tmp", uuid::Uuid::new_v4().hyphenated()));

        let cleaned_preferences = read_preferences(&transaction, admin_id)?.map(|mut value| {
            value
                .background_file_ids
                .retain(|background_id| background_id != file_id);
            if value.active_background_file_id.as_deref() == Some(file_id) {
                value.active_background_file_id = None;
            }
            value
        });
        fs::rename(&original_path, &tombstone_path).map_err(|error| {
            ConsoleUserDataError::storage(format!("failed to stage stored file deletion: {error}"))
        })?;

        let database_result = (|| {
            if let Some(preferences) = cleaned_preferences.as_ref() {
                write_cleaned_preferences(&transaction, admin_id, preferences)?;
            }
            let deleted = transaction
                .execute(
                    "DELETE FROM console_user_files WHERE admin_id = ?1 AND file_id = ?2",
                    params![admin_id, file_id],
                )
                .map_err(storage_error)?;
            if deleted != 1 {
                return Err(ConsoleUserDataError::storage(
                    "file metadata changed during deletion",
                ));
            }
            transaction.commit().map_err(storage_error)
        })();
        if let Err(error) = database_result {
            if let Err(restore_error) = fs::rename(&tombstone_path, &original_path) {
                tracing::error!(
                    error = %restore_error,
                    file_id,
                    "文件删除事务失败后无法恢复已暂存的磁盘文件"
                );
            }
            return Err(error);
        }
        if let Err(error) = fs::remove_file(&tombstone_path) {
            // 数据库已提交且临时名不可通过 API 访问；记录明确告警，避免伪装磁盘清理完成。
            tracing::warn!(error = %error, file_id, "文件记录已删除，但磁盘暂存文件清理失败");
        }
        Ok(())
    }
}

fn insert_file(
    database: &crate::storage::database::SqliteDatabase,
    admin_id: i64,
    file: &UserFile,
) -> Result<(), ConsoleUserDataError> {
    let size = i64::try_from(file.size)
        .map_err(|_| ConsoleUserDataError::invalid("file size is too large"))?;
    database
        .connection()
        .map_err(storage_error)?
        .execute(
            "INSERT INTO console_user_files
             (file_id, admin_id, original_filename, content_type, size,
              storage_filename, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                file.file_id,
                admin_id,
                file.filename,
                file.content_type,
                size,
                file.storage_filename,
                file.created_at,
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn find_owned_file(
    connection: &Connection,
    admin_id: i64,
    file_id: &str,
) -> Result<Option<UserFile>, ConsoleUserDataError> {
    connection
        .query_row(
            "SELECT file_id, original_filename, content_type, size,
                    created_at, storage_filename
             FROM console_user_files
             WHERE admin_id = ?1 AND file_id = ?2",
            params![admin_id, file_id],
            file_from_row,
        )
        .optional()
        .map_err(storage_error)
}

fn file_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserFile> {
    let stored_size = row.get::<_, i64>(3)?;
    let size = u64::try_from(stored_size).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    Ok(UserFile {
        file_id: row.get(0)?,
        filename: row.get(1)?,
        content_type: row.get(2)?,
        size,
        created_at: row.get(4)?,
        storage_filename: row.get(5)?,
    })
}

fn validate_upload(
    filename: &str,
    content_type: &str,
    size: usize,
) -> Result<(), ConsoleUserDataError> {
    if filename.is_empty() || filename.chars().count() > MAX_ORIGINAL_FILENAME_CHARS {
        return Err(ConsoleUserDataError::invalid(format!(
            "filename must contain 1 to {MAX_ORIGINAL_FILENAME_CHARS} characters"
        )));
    }
    if filename.chars().any(char::is_control) {
        return Err(ConsoleUserDataError::invalid(
            "filename must not contain control characters",
        ));
    }
    if content_type.is_empty() || content_type.chars().count() > MAX_CONTENT_TYPE_CHARS {
        return Err(ConsoleUserDataError::invalid(format!(
            "content_type must contain 1 to {MAX_CONTENT_TYPE_CHARS} characters"
        )));
    }
    if axum::http::HeaderValue::from_str(content_type).is_err() {
        return Err(ConsoleUserDataError::invalid("content_type is invalid"));
    }
    if size > MAX_CONSOLE_FILE_BYTES {
        return Err(ConsoleUserDataError::invalid(format!(
            "file must not exceed {MAX_CONSOLE_FILE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn ensure_file_root(path: &Path) -> Result<(), ConsoleUserDataError> {
    fs::create_dir_all(path).map_err(|error| {
        ConsoleUserDataError::storage(format!("failed to create file storage directory: {error}"))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            ConsoleUserDataError::storage(format!(
                "failed to protect file storage directory: {error}"
            ))
        })?;
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), ConsoleUserDataError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        ConsoleUserDataError::storage(format!("failed to create uploaded file: {error}"))
    })?;
    let result = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            ConsoleUserDataError::storage(format!("failed to persist uploaded file: {error}"))
        });
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn storage_path(
    root: &Path,
    storage_filename: &str,
) -> Result<std::path::PathBuf, ConsoleUserDataError> {
    let mut components = Path::new(storage_filename).components();
    let valid = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && storage_filename.ends_with(".blob")
        && storage_filename[..storage_filename.len() - 5]
            .parse::<uuid::Uuid>()
            .is_ok();
    if !valid {
        return Err(ConsoleUserDataError::storage("stored filename is invalid"));
    }
    Ok(root.join(storage_filename))
}

fn storage_error(error: impl std::fmt::Display) -> ConsoleUserDataError {
    ConsoleUserDataError::storage(format!("console file storage failed: {error}"))
}
