use super::*;

#[test]
fn secret_is_encrypted_and_survives_reopen_with_same_master_key() {
    let (center, database, directory) = test_center();
    center
        .replace_secret(
            "provider.openai.api_key",
            "test-secret-value",
            SECRET_MISSING_REVISION,
        )
        .unwrap();

    let connection = database.connection().unwrap();
    let (nonce, ciphertext): (Vec<u8>, Vec<u8>) = connection
        .query_row(
            "SELECT nonce, ciphertext FROM config_secrets WHERE key = ?1",
            ["provider.openai.api_key"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(nonce.len(), 24);
    assert_ne!(ciphertext, b"test-secret-value");
    assert!(ciphertext.len() > b"test-secret-value".len());
    drop(connection);

    let resolved = center.resolved_environment(&HashMap::new()).unwrap();
    assert_eq!(resolved["OPENAI_API_KEY"], "test-secret-value");
    drop(center);

    let reopened = ConfigCenter::open(
        fields(),
        ConfigCenterPaths {
            managed_config_file: directory.join("config/runtime.toml"),
            master_key_file: directory.join("config/secrets/master.key"),
        },
        database,
    )
    .unwrap();
    assert_eq!(
        reopened.resolved_environment(&HashMap::new()).unwrap()["OPENAI_API_KEY"],
        "test-secret-value"
    );
}

#[test]
fn secret_replace_rejects_masked_placeholder_and_clear_is_explicit() {
    let (center, _database, _directory) = test_center();
    let error = center
        .replace_secret(
            "provider.openai.api_key",
            "********",
            SECRET_MISSING_REVISION,
        )
        .unwrap_err();
    assert_eq!(error.code(), "invalid_config");
    assert_eq!(
        center
            .clear_secret("provider.openai.api_key", SECRET_MISSING_REVISION)
            .unwrap(),
        SECRET_MISSING_REVISION
    );
}

#[test]
fn secret_revision_rejects_second_stale_replace() {
    let (center, _database, _directory) = test_center();
    center
        .replace_secret(
            "provider.openai.api_key",
            "first-value",
            SECRET_MISSING_REVISION,
        )
        .unwrap();

    let error = center
        .replace_secret(
            "provider.openai.api_key",
            "stale-second-value",
            SECRET_MISSING_REVISION,
        )
        .unwrap_err();

    assert_eq!(error.code(), "config_conflict");
    assert_eq!(
        center.current_snapshot().unwrap().fields[2]
            .revision
            .as_deref()
            .map(|revision| revision.starts_with("sha256:")),
        Some(true)
    );
    assert_eq!(
        center.resolved_environment(&HashMap::new()).unwrap()["OPENAI_API_KEY"],
        "first-value"
    );
}

#[test]
fn stale_clear_does_not_delete_rotated_secret() {
    let (center, _database, _directory) = test_center();
    let first_revision = center
        .replace_secret(
            "provider.openai.api_key",
            "first-value",
            SECRET_MISSING_REVISION,
        )
        .unwrap();
    let second_revision = center
        .replace_secret("provider.openai.api_key", "rotated-value", &first_revision)
        .unwrap();

    let error = center
        .clear_secret("provider.openai.api_key", &first_revision)
        .unwrap_err();

    assert_eq!(error.code(), "config_conflict");
    assert_eq!(
        secret_revision(&center, "provider.openai.api_key"),
        second_revision
    );
    assert_eq!(
        center.resolved_environment(&HashMap::new()).unwrap()["OPENAI_API_KEY"],
        "rotated-value"
    );
}

#[test]
fn related_secrets_validate_and_commit_as_one_transaction() {
    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-config-related", &[CONFIG_SECRET_SCHEMA_V1])
            .unwrap();
    let related_fields = vec![
        ManagedConfigField::secret(
            "platform.qq.app_id",
            "QQ_BOT_APP_ID",
            "gateway.qq",
            ManagedConfigApplyMode::Restart,
        ),
        ManagedConfigField::secret(
            "platform.qq.app_secret",
            "QQ_BOT_APP_SECRET",
            "gateway.qq",
            ManagedConfigApplyMode::Restart,
        ),
    ];
    let center = ConfigCenter::open(
        related_fields,
        ConfigCenterPaths {
            managed_config_file: directory.join("config/runtime.toml"),
            master_key_file: directory.join("config/secrets/master.key"),
        },
        database,
    )
    .unwrap()
    .with_candidate_validator(|environment| {
        let app_id = environment.contains_key("QQ_BOT_APP_ID");
        let app_secret = environment.contains_key("QQ_BOT_APP_SECRET");
        (app_id == app_secret)
            .then_some(())
            .ok_or_else(|| "QQ credentials must be configured together".to_owned())
    });

    let error = center
        .replace_secret("platform.qq.app_id", "qq-app-id", SECRET_MISSING_REVISION)
        .unwrap_err();
    assert_eq!(error.code(), "invalid_config");
    assert_eq!(
        secret_revision(&center, "platform.qq.app_id"),
        SECRET_MISSING_REVISION
    );

    let revisions = center
        .update_secrets(&[
            SecretConfigChange::Replace {
                key: "platform.qq.app_id".to_owned(),
                value: "qq-app-id".to_owned(),
                expected_revision: SECRET_MISSING_REVISION.to_owned(),
            },
            SecretConfigChange::Replace {
                key: "platform.qq.app_secret".to_owned(),
                value: "qq-app-secret".to_owned(),
                expected_revision: SECRET_MISSING_REVISION.to_owned(),
            },
        ])
        .unwrap();

    assert!(revisions.values().all(|value| value.starts_with("sha256:")));
    let serialized = serde_json::to_string(&center.current_snapshot().unwrap()).unwrap();
    assert!(!serialized.contains("qq-app-id"));
    assert!(!serialized.contains("qq-app-secret"));
}

#[test]
fn candidate_validation_failure_rolls_back_runtime_and_secret() {
    let (center, _database, directory) = test_center();
    let center = center.with_candidate_validator(|environment| {
        if environment.get("RSS_ENABLED").map(String::as_str) == Some("false")
            || environment.contains_key("OPENAI_API_KEY")
        {
            Err("candidate rejected".to_owned())
        } else {
            Ok(())
        }
    });

    let runtime_error = center
        .update_managed(
            &center.current_snapshot().unwrap().revision,
            &[ManagedConfigChange::Set {
                key: "features.rss.enabled".to_owned(),
                value: Value::Boolean(false),
            }],
        )
        .unwrap_err();
    assert_eq!(runtime_error.code(), "invalid_config");
    let runtime_text = std::fs::read_to_string(directory.join("config/runtime.toml")).unwrap();
    assert!(!runtime_text.contains("features.rss.enabled"));

    let secret_error = center
        .replace_secret(
            "provider.openai.api_key",
            "must-rollback",
            SECRET_MISSING_REVISION,
        )
        .unwrap_err();
    assert_eq!(secret_error.code(), "invalid_config");
    assert_eq!(
        secret_revision(&center, "provider.openai.api_key"),
        SECRET_MISSING_REVISION
    );
    assert!(
        !center
            .resolved_environment(&HashMap::new())
            .unwrap()
            .contains_key("OPENAI_API_KEY")
    );
}

#[test]
fn snapshot_valid_uses_candidate_validator_without_exposing_secret() {
    let (center, _database, _directory) = test_center();
    let center = center.with_candidate_validator(|environment| {
        environment
            .contains_key("OPENAI_API_KEY")
            .then_some(())
            .ok_or_else(|| "provider credential is missing".to_owned())
    });
    let invalid = center.current_snapshot().unwrap();
    assert!(invalid.fields.iter().all(|field| !field.valid));
    assert_eq!(
        invalid.fields[2].revision.as_deref(),
        Some(SECRET_MISSING_REVISION)
    );

    center
        .replace_secret(
            "provider.openai.api_key",
            "snapshot-secret",
            SECRET_MISSING_REVISION,
        )
        .unwrap();
    let valid = center.current_snapshot().unwrap();
    assert!(valid.fields.iter().all(|field| field.valid));
    let serialized = serde_json::to_string(&valid).unwrap();
    assert!(!serialized.contains("snapshot-secret"));
}

#[test]
fn snapshot_hides_secret_and_reports_managed_override() {
    let (center, _database, _directory) = test_center();
    center
        .replace_secret(
            "provider.openai.api_key",
            "encrypted-secret",
            SECRET_MISSING_REVISION,
        )
        .unwrap();
    let initial = center.snapshot(&HashMap::new()).unwrap();
    let secret = initial
        .fields
        .iter()
        .find(|field| field.key == "provider.openai.api_key")
        .unwrap();
    assert!(secret.configured);
    assert!(secret.revision.as_deref().unwrap().starts_with("sha256:"));
    assert_eq!(secret.source, ConfigValueSource::EncryptedSecret);
    assert_eq!(secret.effective_value, None);
    assert!(secret.pending_restart);

    let external = HashMap::from([
        ("OPENAI_API_KEY".to_owned(), "external-secret".to_owned()),
        ("RSS_ENABLED".to_owned(), "false".to_owned()),
    ]);
    let snapshot = center.snapshot(&external).unwrap();
    let secret = snapshot
        .fields
        .iter()
        .find(|field| field.key == "provider.openai.api_key")
        .unwrap();
    assert_eq!(secret.source, ConfigValueSource::EncryptedSecret);
    assert!(secret.overridden);
    assert_eq!(secret.effective_value, None);
    assert!(secret.editable);
    assert!(secret.pending_restart);
    let rss = snapshot
        .fields
        .iter()
        .find(|field| field.key == "features.rss.enabled")
        .unwrap();
    assert_eq!(rss.effective_value, Some(Value::Boolean(false)));
}

#[test]
fn resolved_environment_prefers_saved_values_and_keeps_unregistered_environment() {
    let (center, _database, _directory) = test_center();
    let initial = center.snapshot(&HashMap::new()).unwrap();
    center
        .update_managed(
            &initial.revision,
            &[ManagedConfigChange::Set {
                key: "features.rss.enabled".to_owned(),
                value: Value::Boolean(false),
            }],
        )
        .unwrap();
    let external = HashMap::from([
        ("RSS_ENABLED".to_owned(), "true".to_owned()),
        ("UNREGISTERED_VALUE".to_owned(), "kept".to_owned()),
    ]);
    let resolved = center.resolved_environment(&external).unwrap();
    assert_eq!(resolved["RSS_ENABLED"], "false");
    assert_eq!(resolved["UNREGISTERED_VALUE"], "kept");
}

#[test]
fn external_console_disable_overrides_previously_saved_enable() {
    let (center, _database, _directory) = test_center();
    let initial = center.snapshot(&HashMap::new()).unwrap();
    center
        .update_managed(
            &initial.revision,
            &[ManagedConfigChange::Set {
                key: "console.enabled".to_owned(),
                value: Value::Boolean(true),
            }],
        )
        .unwrap();
    let external = HashMap::from([("WEB_CONSOLE_ENABLED".to_owned(), "false".to_owned())]);

    let resolved = center.resolved_environment(&external).unwrap();
    assert_eq!(resolved["WEB_CONSOLE_ENABLED"], "false");
    let snapshot = center.snapshot(&external).unwrap();
    let console = snapshot
        .fields
        .iter()
        .find(|field| field.key == "console.enabled")
        .unwrap();
    assert_eq!(console.source, ConfigValueSource::Environment);
    assert_eq!(console.effective_value, Some(Value::Boolean(false)));
    assert_eq!(console.saved_value, Some(Value::Boolean(true)));
}

#[cfg(unix)]
#[test]
fn master_key_has_strict_permissions_and_symlink_is_rejected() {
    use std::os::unix::fs::{MetadataExt, symlink};

    let (center, database, directory) = test_center();
    drop(center);
    let key_path = directory.join("config/secrets/master.key");
    assert_eq!(std::fs::metadata(&key_path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(key_path.parent().unwrap())
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );

    let link = directory.join("config/secrets/linked.key");
    symlink(&key_path, &link).unwrap();
    let error = match ConfigCenter::open(
        fields(),
        ConfigCenterPaths {
            managed_config_file: directory.join("config/runtime.toml"),
            master_key_file: link,
        },
        database,
    ) {
        Ok(_) => panic!("symbolic-link master key must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "secret_storage_error");
    assert!(error.message().contains("symbolic link"));
}

#[cfg(unix)]
#[test]
fn damaged_or_unsafe_existing_master_key_is_never_overwritten() {
    use std::os::unix::fs::PermissionsExt;

    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-config-bad-key", &[CONFIG_SECRET_SCHEMA_V1])
            .unwrap();
    let key_path = directory.join("config/secrets/master.key");
    std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
    std::fs::set_permissions(
        key_path.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(&key_path, b"broken-key\n").unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let paths = ConfigCenterPaths {
        managed_config_file: directory.join("config/runtime.toml"),
        master_key_file: key_path.clone(),
    };

    let error = match ConfigCenter::open(fields(), paths.clone(), database.clone()) {
        Ok(_) => panic!("damaged master key must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "secret_storage_error");
    assert_eq!(std::fs::read(&key_path).unwrap(), b"broken-key\n");

    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let error = match ConfigCenter::open(fields(), paths, database.clone()) {
        Ok(_) => panic!("unsafe master key permissions must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "secret_storage_error");
    assert!(error.message().contains("permissions"));
    assert_eq!(std::fs::read(&key_path).unwrap(), b"broken-key\n");
}

#[test]
fn tampered_ciphertext_fails_authentication_without_returning_plaintext() {
    let (center, database, _directory) = test_center();
    center
        .replace_secret(
            "provider.openai.api_key",
            "never-print-this",
            SECRET_MISSING_REVISION,
        )
        .unwrap();
    database
        .connection()
        .unwrap()
        .execute(
            "UPDATE config_secrets SET ciphertext = X'00010203' WHERE key = ?1",
            ["provider.openai.api_key"],
        )
        .unwrap();

    let error = center.resolved_environment(&HashMap::new()).unwrap_err();
    assert_eq!(error.code(), "secret_storage_error");
    assert!(!error.to_string().contains("never-print-this"));
}
