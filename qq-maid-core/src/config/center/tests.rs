use std::collections::HashMap;

use qq_maid_common::managed_config::{
    ManagedConfigApplyMode, ManagedConfigField, ManagedConfigValueType,
};
use toml::Value;

use crate::storage::database::SqliteDatabase;

use super::*;

fn fields() -> Vec<ManagedConfigField> {
    vec![
        ManagedConfigField::public(
            "features.rss.enabled",
            "RSS_ENABLED",
            "core.rss",
            ManagedConfigValueType::Boolean,
            ManagedConfigApplyMode::Restart,
            Some("true"),
        ),
        ManagedConfigField::public(
            "console.allowed_origins",
            "WEB_CONSOLE_ALLOWED_ORIGINS",
            "core.console",
            ManagedConfigValueType::StringList,
            ManagedConfigApplyMode::Restart,
            None,
        ),
        ManagedConfigField::secret(
            "provider.openai.api_key",
            "OPENAI_API_KEY",
            "core.provider",
            ManagedConfigApplyMode::Restart,
        ),
    ]
}

fn test_center() -> (ConfigCenter, SqliteDatabase, std::path::PathBuf) {
    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-config-center", &[CONFIG_SECRET_SCHEMA_V1])
            .unwrap();
    let paths = ConfigCenterPaths {
        managed_config_file: directory.join("config/runtime.toml"),
        master_key_file: directory.join("config/secrets/master.key"),
    };
    let center = ConfigCenter::open(fields(), paths, database.clone()).unwrap();
    (center, database, directory)
}

#[test]
fn paths_default_master_key_relative_to_managed_config_directory() {
    let paths = ConfigCenterPaths::from_environment(&HashMap::new());
    assert_eq!(
        paths.managed_config_file,
        std::path::Path::new("config/runtime.toml")
    );
    assert_eq!(
        paths.master_key_file,
        std::path::Path::new("config/secrets/master.key")
    );

    let environment = HashMap::from([(
        RUNTIME_CONFIG_FILE_ENV.to_owned(),
        "/srv/maid/runtime.toml".to_owned(),
    )]);
    let paths = ConfigCenterPaths::from_environment(&environment);
    assert_eq!(
        paths.master_key_file,
        std::path::Path::new("/srv/maid/secrets/master.key")
    );
}

#[test]
fn registry_rejects_duplicate_keys_and_environment_mappings() {
    let duplicate_key = vec![fields()[0], fields()[0]];
    assert_eq!(
        ConfigRegistry::new(duplicate_key).unwrap_err().code(),
        "invalid_config"
    );

    let mut duplicate_env = fields();
    duplicate_env.push(ManagedConfigField::public(
        "features.other.enabled",
        "RSS_ENABLED",
        "core.other",
        ManagedConfigValueType::Boolean,
        ManagedConfigApplyMode::Restart,
        Some("false"),
    ));
    assert_eq!(
        ConfigRegistry::new(duplicate_env).unwrap_err().code(),
        "invalid_config"
    );
}

#[test]
fn registry_rejects_semantically_invalid_managed_values() {
    let registry = ConfigRegistry::new(vec![ManagedConfigField::public(
        "provider.mode",
        "LLM_PROVIDER",
        "core.provider",
        ManagedConfigValueType::String,
        ManagedConfigApplyMode::Restart,
        Some("openai"),
    )])
    .unwrap();
    let field = registry.require("provider.mode").unwrap();

    let error = registry
        .validate_managed_value(field, &Value::String("unknown-provider".to_owned()))
        .unwrap_err();
    assert_eq!(error.code(), "invalid_config");
}

#[test]
fn compatibility_environment_alias_is_a_real_external_override() {
    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-config-alias", &[CONFIG_SECRET_SCHEMA_V1])
            .unwrap();
    let alias_fields = vec![
        ManagedConfigField::secret(
            "platform.qq.app_id",
            "QQ_BOT_APP_ID",
            "gateway.qq",
            ManagedConfigApplyMode::Restart,
        )
        .with_env_aliases(&["QQ_APPID"]),
    ];
    let center = ConfigCenter::open(
        alias_fields,
        ConfigCenterPaths {
            managed_config_file: directory.join("config/runtime.toml"),
            master_key_file: directory.join("config/secrets/master.key"),
        },
        database,
    )
    .unwrap();
    let external = HashMap::from([("QQ_APPID".to_owned(), "legacy-id".to_owned())]);

    let snapshot = center.snapshot(&external).unwrap();
    assert_eq!(snapshot.fields[0].source, ConfigValueSource::Environment);
    assert!(snapshot.fields[0].configured);
    assert!(!snapshot.fields[0].editable);
    let resolved = center.resolved_environment(&external).unwrap();
    assert_eq!(resolved["QQ_APPID"], "legacy-id");
    assert!(!resolved.contains_key("QQ_BOT_APP_ID"));
}

#[test]
fn managed_file_uses_revision_and_never_accepts_secret_values() {
    let (center, _database, directory) = test_center();
    let initial = center.snapshot(&HashMap::new()).unwrap();
    assert_eq!(initial.revision, "missing");
    assert!(!initial.file_exists);

    let saved = center
        .update_managed(
            &initial.revision,
            &[ManagedConfigChange::Set {
                key: "features.rss.enabled".to_owned(),
                value: Value::Boolean(false),
            }],
        )
        .unwrap();
    assert!(saved.revision.starts_with("sha256:"));
    assert_eq!(
        saved.values.get("features.rss.enabled"),
        Some(&Value::Boolean(false))
    );
    let pending = center.snapshot(&HashMap::new()).unwrap();
    let rss = pending
        .fields
        .iter()
        .find(|field| field.key == "features.rss.enabled")
        .unwrap();
    assert_eq!(rss.saved_value, Some(Value::Boolean(false)));
    assert_eq!(rss.effective_value, Some(Value::Boolean(false)));
    assert_eq!(rss.running_value, Some(Value::Boolean(true)));
    assert!(rss.pending_restart);

    let conflict = center
        .update_managed(
            "missing",
            &[ManagedConfigChange::Set {
                key: "features.rss.enabled".to_owned(),
                value: Value::Boolean(true),
            }],
        )
        .unwrap_err();
    assert_eq!(conflict.code(), "config_conflict");

    let secret_in_toml = center
        .update_managed(
            &saved.revision,
            &[ManagedConfigChange::Set {
                key: "provider.openai.api_key".to_owned(),
                value: Value::String("must-not-be-written".to_owned()),
            }],
        )
        .unwrap_err();
    assert_eq!(secret_in_toml.code(), "invalid_config");

    let text = std::fs::read_to_string(directory.join("config/runtime.toml")).unwrap();
    assert!(text.contains("features.rss.enabled"));
    assert!(!text.contains("must-not-be-written"));
}

#[cfg(unix)]
#[test]
fn managed_file_can_be_read_but_not_falsely_saved_when_read_only() {
    use std::os::unix::fs::PermissionsExt;

    let (center, _database, directory) = test_center();
    let saved = center
        .update_managed(
            "missing",
            &[ManagedConfigChange::Set {
                key: "features.rss.enabled".to_owned(),
                value: Value::Boolean(false),
            }],
        )
        .unwrap();
    let path = directory.join("config/runtime.toml");
    let before = std::fs::read_to_string(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();

    assert_eq!(
        center.snapshot(&HashMap::new()).unwrap().revision,
        saved.revision
    );
    let error = center
        .update_managed(
            &saved.revision,
            &[ManagedConfigChange::Set {
                key: "features.rss.enabled".to_owned(),
                value: Value::Boolean(true),
            }],
        )
        .unwrap_err();
    assert_eq!(error.code(), "config_io_error");
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[test]
fn secret_is_encrypted_and_survives_reopen_with_same_master_key() {
    let (center, database, directory) = test_center();
    center
        .replace_secret("provider.openai.api_key", "test-secret-value")
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
        .replace_secret("provider.openai.api_key", "********")
        .unwrap_err();
    assert_eq!(error.code(), "invalid_config");
    assert!(!center.clear_secret("provider.openai.api_key").unwrap());
}

#[test]
fn snapshot_hides_secret_and_reports_external_override() {
    let (center, _database, _directory) = test_center();
    center
        .replace_secret("provider.openai.api_key", "encrypted-secret")
        .unwrap();
    let initial = center.snapshot(&HashMap::new()).unwrap();
    let secret = initial
        .fields
        .iter()
        .find(|field| field.key == "provider.openai.api_key")
        .unwrap();
    assert!(secret.configured);
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
    assert_eq!(secret.source, ConfigValueSource::Environment);
    assert!(secret.overridden);
    assert_eq!(secret.effective_value, None);
    assert!(!secret.editable);
    assert!(!secret.pending_restart);
    let rss = snapshot
        .fields
        .iter()
        .find(|field| field.key == "features.rss.enabled")
        .unwrap();
    assert_eq!(rss.effective_value, Some(Value::Boolean(false)));
}

#[test]
fn resolved_environment_prefers_external_values() {
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
    assert_eq!(resolved["RSS_ENABLED"], "true");
    assert_eq!(resolved["UNREGISTERED_VALUE"], "kept");
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
        .replace_secret("provider.openai.api_key", "never-print-this")
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
