use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    BackgroundMode, ConsoleUserDataError, ConsoleUserDataService, MAX_BACKGROUND_FILES,
    MAX_CUSTOM_COLOR_CHARS, MAX_CUSTOM_COLORS, PreferenceValuePatch, UserPreferences,
    UserPreferencesPatch, now_rfc3339, validate_file_id,
};

impl ConsoleUserDataService {
    pub fn get_preferences(&self, admin_id: i64) -> Result<UserPreferences, ConsoleUserDataError> {
        let connection = self.database.connection().map_err(storage_error)?;
        read_preferences(&connection, admin_id).map(|value| value.unwrap_or_default())
    }

    pub fn update_preferences(
        &self,
        admin_id: i64,
        patch: UserPreferencesPatch,
    ) -> Result<UserPreferences, ConsoleUserDataError> {
        validate_patch(&patch)?;
        let mut connection = self.database.connection().map_err(storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let mut preferences = read_preferences(&transaction, admin_id)?.unwrap_or_default();
        if patch.is_empty() {
            return Ok(preferences);
        }

        if let Some(custom_colors) = patch.custom_colors {
            preferences.custom_colors = custom_colors;
        }
        if let Some(background_file_ids) = patch.background_file_ids {
            for file_id in &background_file_ids {
                if !file_belongs_to(&transaction, admin_id, file_id)? {
                    return Err(ConsoleUserDataError::invalid(
                        "every background_file_id must identify a file owned by the current user",
                    ));
                }
            }
            preferences.background_file_ids = background_file_ids;
        }

        let active_was_unchanged = matches!(
            &patch.active_background_file_id,
            PreferenceValuePatch::Unchanged
        );
        preferences.active_background_file_id = match patch.active_background_file_id {
            PreferenceValuePatch::Unchanged => preferences.active_background_file_id,
            PreferenceValuePatch::Clear => None,
            PreferenceValuePatch::Set(file_id) => Some(file_id),
        };
        if preferences
            .active_background_file_id
            .as_ref()
            .is_some_and(|active| !preferences.background_file_ids.contains(active))
        {
            if active_was_unchanged {
                // 整体替换图库时，已移出的当前背景按协议自动恢复默认背景。
                preferences.active_background_file_id = None;
            } else {
                return Err(ConsoleUserDataError::invalid(
                    "active_background_file_id must be present in background_file_ids",
                ));
            }
        }
        if let Some(background_mode) = patch.background_mode {
            // 特殊九宫格不引用自定义文件：切换后清空活动背景，避免服务端状态分裂。
            if background_mode == BackgroundMode::Special {
                preferences.active_background_file_id = None;
            }
            preferences.background_mode = background_mode;
        }
        // 一致性约束：活动自定义背景由 active_background_file_id 表达，模式字段只能是 default。
        if preferences.active_background_file_id.is_some() {
            preferences.background_mode = BackgroundMode::Default;
        }
        if let Some(kuliantnt) = patch.kuliantnt {
            preferences.kuliantnt = kuliantnt;
        }

        upsert_preferences(&transaction, admin_id, &preferences)?;
        transaction.commit().map_err(storage_error)?;
        Ok(preferences)
    }
}

pub(super) fn read_preferences(
    connection: &Connection,
    admin_id: i64,
) -> Result<Option<UserPreferences>, ConsoleUserDataError> {
    let row = connection
        .query_row(
            "SELECT custom_colors_json, background_file_ids_json,
                    active_background_file_id, background_mode, kuliantnt
             FROM console_user_preferences WHERE admin_id = ?1",
            [admin_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            },
        )
        .optional()
        .map_err(storage_error)?;
    let Some((
        custom_colors,
        background_file_ids,
        active_background_file_id,
        background_mode,
        kuliantnt,
    )) = row
    else {
        return Ok(None);
    };
    let background_mode = background_mode
        .parse::<BackgroundMode>()
        .map_err(storage_error)?;
    Ok(Some(UserPreferences {
        custom_colors: serde_json::from_str(&custom_colors).map_err(storage_error)?,
        background_file_ids: serde_json::from_str(&background_file_ids).map_err(storage_error)?,
        active_background_file_id,
        background_mode,
        kuliantnt,
    }))
}

pub(super) fn write_cleaned_preferences(
    connection: &Connection,
    admin_id: i64,
    preferences: &UserPreferences,
) -> Result<(), ConsoleUserDataError> {
    connection
        .execute(
            "UPDATE console_user_preferences
             SET background_file_ids_json = ?1,
                 active_background_file_id = ?2,
                 updated_at = ?3
             WHERE admin_id = ?4",
            params![
                serde_json::to_string(&preferences.background_file_ids).map_err(storage_error)?,
                preferences.active_background_file_id.as_deref(),
                now_rfc3339(),
                admin_id,
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn upsert_preferences(
    connection: &Connection,
    admin_id: i64,
    preferences: &UserPreferences,
) -> Result<(), ConsoleUserDataError> {
    let now = now_rfc3339();
    connection
        .execute(
            "INSERT INTO console_user_preferences
             (admin_id, custom_colors_json, background_file_ids_json,
              active_background_file_id, background_mode, kuliantnt, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(admin_id) DO UPDATE SET
               custom_colors_json = excluded.custom_colors_json,
               background_file_ids_json = excluded.background_file_ids_json,
               active_background_file_id = excluded.active_background_file_id,
               background_mode = excluded.background_mode,
               kuliantnt = excluded.kuliantnt,
               updated_at = excluded.updated_at",
            params![
                admin_id,
                serde_json::to_string(&preferences.custom_colors).map_err(storage_error)?,
                serde_json::to_string(&preferences.background_file_ids).map_err(storage_error)?,
                preferences.active_background_file_id.as_deref(),
                preferences.background_mode.as_str(),
                preferences.kuliantnt,
                now,
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn validate_patch(patch: &UserPreferencesPatch) -> Result<(), ConsoleUserDataError> {
    if let Some(colors) = patch.custom_colors.as_ref() {
        if colors.len() > MAX_CUSTOM_COLORS {
            return Err(ConsoleUserDataError::invalid(format!(
                "custom_colors must not contain more than {MAX_CUSTOM_COLORS} values"
            )));
        }
        if colors
            .iter()
            .any(|color| color.chars().count() > MAX_CUSTOM_COLOR_CHARS)
        {
            return Err(ConsoleUserDataError::invalid(format!(
                "each custom color must not exceed {MAX_CUSTOM_COLOR_CHARS} characters"
            )));
        }
    }
    if let Some(file_ids) = patch.background_file_ids.as_ref() {
        if file_ids.len() > MAX_BACKGROUND_FILES {
            return Err(ConsoleUserDataError::invalid(format!(
                "background_file_ids must not contain more than {MAX_BACKGROUND_FILES} values"
            )));
        }
        let mut unique = HashSet::with_capacity(file_ids.len());
        for file_id in file_ids {
            validate_file_id(file_id)?;
            if !unique.insert(file_id) {
                return Err(ConsoleUserDataError::invalid(
                    "background_file_ids must not contain duplicates",
                ));
            }
        }
    }
    if let PreferenceValuePatch::Set(file_id) = &patch.active_background_file_id {
        validate_file_id(file_id)?;
    }
    Ok(())
}

fn file_belongs_to(
    connection: &Connection,
    admin_id: i64,
    file_id: &str,
) -> Result<bool, ConsoleUserDataError> {
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM console_user_files WHERE admin_id = ?1 AND file_id = ?2
             )",
            params![admin_id, file_id],
            |row| row.get(0),
        )
        .map_err(storage_error)
}

fn storage_error(error: impl std::fmt::Display) -> ConsoleUserDataError {
    ConsoleUserDataError::storage(format!("console preference storage failed: {error}"))
}
