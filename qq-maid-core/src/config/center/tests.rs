use std::collections::HashMap;

use toml::Value;

use crate::{
    config::{
        AgentProfileConfig, AgentRuntimeConfig, AgentSceneConfig, ChatScene,
        agent::AgentConfigSource,
    },
    storage::database::SqliteDatabase,
};

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
        ManagedConfigField::public(
            "weather.qweather.geo_host",
            "QWEATHER_GEO_HOST",
            "core.weather",
            ManagedConfigValueType::String,
            ManagedConfigApplyMode::Restart,
            None,
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

fn secret_revision(center: &ConfigCenter, key: &str) -> String {
    center
        .current_snapshot()
        .unwrap()
        .fields
        .into_iter()
        .find(|field| field.key == key)
        .and_then(|field| field.revision)
        .unwrap()
}

fn test_agent_file() -> (
    AgentConfigFile,
    AgentRuntimeConfig,
    SqliteDatabase,
    std::path::PathBuf,
) {
    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-agent-config", &[CONFIG_SECRET_SCHEMA_V1])
            .unwrap();
    let path = directory.join("config/agent.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text = include_str!("../../../../runtime/config/agent.toml");
    std::fs::write(&path, text).unwrap();
    let running = AgentRuntimeConfig::from_toml(
        text,
        AgentConfigSource::File(path.to_string_lossy().into_owned()),
    )
    .unwrap();
    let file = AgentConfigFile::new(running.clone()).unwrap();
    (file, running, database, path)
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
        "provider.openai.api_mode",
        "OPENAI_API_MODE",
        "core.provider",
        ManagedConfigValueType::String,
        ManagedConfigApplyMode::Restart,
        Some("auto"),
    )])
    .unwrap();
    let field = registry.require("provider.openai.api_mode").unwrap();

    let error = registry
        .validate_managed_value(field, &Value::String("unknown-provider".to_owned()))
        .unwrap_err();
    assert_eq!(error.code(), "invalid_config");
}

#[test]
fn registry_validates_managed_command_prefix() {
    let registry = ConfigRegistry::new(crate::config::managed_config_fields()).unwrap();
    let field = registry.require("command.prefix").unwrap();

    for value in ["/", "#", "*"] {
        registry
            .validate_managed_value(field, &Value::String(value.to_owned()))
            .unwrap();
    }
    for value in [" ", "\n", "##", "ab"] {
        let error = registry
            .validate_managed_value(field, &Value::String(value.to_owned()))
            .unwrap_err();
        assert_eq!(error.code(), "invalid_config");
    }
}

#[test]
fn command_prefix_is_public_editable_restart_field_with_slash_default() {
    let fields = crate::config::managed_config_fields();
    let field = fields
        .iter()
        .find(|field| field.key == "command.prefix")
        .expect("managed command prefix field");

    assert_eq!(field.env_name, "CHAT_COMMAND_PREFIX");
    assert_eq!(field.default_value, Some("/"));
    assert_eq!(field.sensitivity, ManagedConfigSensitivity::Public);
    assert_eq!(field.apply_mode, ManagedConfigApplyMode::Restart);
    assert!(field.web_editable);
}

#[test]
fn compatibility_environment_alias_is_an_editable_fallback() {
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
    assert!(snapshot.fields[0].editable);
    let resolved = center.resolved_environment(&external).unwrap();
    assert_eq!(resolved["QQ_APPID"], "legacy-id");
    assert!(!resolved.contains_key("QQ_BOT_APP_ID"));

    center
        .replace_secret(
            "platform.qq.app_id",
            "encrypted-id",
            SECRET_MISSING_REVISION,
        )
        .unwrap();
    let resolved = center.resolved_environment(&external).unwrap();
    assert_eq!(resolved["QQ_BOT_APP_ID"], "encrypted-id");
    assert!(!resolved.contains_key("QQ_APPID"));
}

#[test]
fn blank_external_values_are_unset_and_do_not_break_configuration_snapshot() {
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
        ("RSS_ENABLED".to_owned(), "  ".to_owned()),
        ("QWEATHER_GEO_HOST".to_owned(), String::new()),
    ]);

    let snapshot = center.snapshot(&external).unwrap();
    let rss = snapshot
        .fields
        .iter()
        .find(|field| field.key == "features.rss.enabled")
        .unwrap();
    assert_eq!(rss.source, ConfigValueSource::ManagedToml);
    assert_eq!(rss.effective_value, Some(Value::Boolean(false)));
    assert!(rss.editable);
    let geo_host = snapshot
        .fields
        .iter()
        .find(|field| field.key == "weather.qweather.geo_host")
        .unwrap();
    assert_eq!(geo_host.source, ConfigValueSource::NotConfigured);
    assert!(!geo_host.configured);
    assert!(geo_host.editable);

    let resolved = center.resolved_environment(&external).unwrap();
    assert_eq!(resolved["RSS_ENABLED"], "false");
    assert!(!resolved.contains_key("QWEATHER_GEO_HOST"));
}

#[test]
fn domain_writes_override_registered_environment_fallbacks() {
    let (center, _database, _directory) = test_center();
    let center = center.with_external_environment(HashMap::from([
        ("RSS_ENABLED".to_owned(), "false".to_owned()),
        ("OPENAI_API_KEY".to_owned(), "external-secret".to_owned()),
    ]));
    let snapshot = center.current_snapshot().unwrap();
    let rss = snapshot
        .fields
        .iter()
        .find(|field| field.key == "features.rss.enabled")
        .unwrap();
    let secret = snapshot
        .fields
        .iter()
        .find(|field| field.key == "provider.openai.api_key")
        .unwrap();
    assert!(rss.editable);
    assert!(secret.editable);

    center
        .update_managed(
            &snapshot.revision,
            &[ManagedConfigChange::Set {
                key: "features.rss.enabled".to_owned(),
                value: Value::Boolean(true),
            }],
        )
        .unwrap();

    center
        .replace_secret(
            "provider.openai.api_key",
            "encrypted-secret",
            SECRET_MISSING_REVISION,
        )
        .unwrap();

    let resolved = center.current_resolved_environment().unwrap();
    assert_eq!(resolved["RSS_ENABLED"], "true");
    assert_eq!(resolved["OPENAI_API_KEY"], "encrypted-secret");
    let updated = center.current_snapshot().unwrap();
    let rss = updated
        .fields
        .iter()
        .find(|field| field.key == "features.rss.enabled")
        .unwrap();
    let secret = updated
        .fields
        .iter()
        .find(|field| field.key == "provider.openai.api_key")
        .unwrap();
    assert_eq!(rss.source, ConfigValueSource::ManagedToml);
    assert!(rss.overridden);
    assert_eq!(secret.source, ConfigValueSource::EncryptedSecret);
    assert!(secret.overridden);
}

#[test]
fn managed_file_uses_revision_and_never_accepts_secret_values() {
    let (center, _database, directory) = test_center();
    let initial = center.snapshot(&HashMap::new()).unwrap();
    assert!(initial.revision.starts_with("sha256:"));
    assert!(initial.file_exists);
    let initial_text = std::fs::read_to_string(directory.join("config/runtime.toml")).unwrap();
    assert!(initial_text.contains("version = 1"));
    assert!(initial_text.contains("[values]"));
    assert!(!initial_text.contains("api_key"));

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

#[test]
fn managed_save_rechecks_revision_after_candidate_validation() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let (center, _database, directory) = test_center();
    let path = directory.join("config/runtime.toml");
    let initial = center.current_snapshot().unwrap();
    let changed = Arc::new(AtomicBool::new(false));
    let validator_changed = Arc::clone(&changed);
    let validator_path = path.clone();
    let manual = "version = 1\n\n[values]\n\"features.rss.enabled\" = true\n# manual edit\n";
    let center = center.with_candidate_validator(move |_| {
        if !validator_changed.swap(true, Ordering::SeqCst) {
            std::fs::create_dir_all(validator_path.parent().unwrap()).unwrap();
            std::fs::write(&validator_path, manual).unwrap();
        }
        Ok(())
    });

    let error = center
        .update_managed(
            &initial.revision,
            &[ManagedConfigChange::Set {
                key: "features.rss.enabled".to_owned(),
                value: Value::Boolean(false),
            }],
        )
        .unwrap_err();

    assert_eq!(error.code(), "config_conflict");
    assert_eq!(std::fs::read_to_string(path).unwrap(), manual);
}

#[test]
fn concurrent_managed_update_allows_only_one_shared_revision() {
    use std::sync::{Arc, Barrier};

    let (center, _database, _directory) = test_center();
    let revision = center.current_snapshot().unwrap().revision;
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for value in [true, false] {
        let center = center.clone();
        let revision = revision.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            center.update_managed(
                &revision,
                &[ManagedConfigChange::Set {
                    key: "features.rss.enabled".to_owned(),
                    value: Value::Boolean(value),
                }],
            )
        }));
    }
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .next()
            .unwrap()
            .code(),
        "config_conflict"
    );
}

#[cfg(unix)]
#[test]
fn managed_file_can_be_read_but_not_falsely_saved_when_read_only() {
    use std::os::unix::fs::PermissionsExt;

    let (center, _database, directory) = test_center();
    let initial_revision = center.current_snapshot().unwrap().revision;
    let saved = center
        .update_managed(
            &initial_revision,
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

#[test]
fn agent_route_save_reloads_new_model_and_reports_pending_restart() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    assert_eq!(initial.source, ConfigValueSource::AgentToml);
    assert!(!initial.pending_restart);

    let saved = file
        .update(
            &initial.revision,
            &[AgentConfigChange::SetModelRoute {
                name: "private_main".to_owned(),
                candidates: vec!["deepseek:deepseek-chat".to_owned()],
            }],
        )
        .unwrap();
    assert!(saved.pending_restart);
    assert_ne!(saved.saved_value, saved.running_value);

    let environment = HashMap::from([(
        crate::config::agent::AGENT_CONFIG_FILE_ENV.to_owned(),
        path.to_string_lossy().into_owned(),
    )]);
    let reloaded = AgentRuntimeConfig::load_from_environment(&environment).unwrap();
    assert_eq!(
        reloaded.resolve(ChatScene::Private).unwrap().main_model,
        "deepseek:deepseek-chat"
    );

    let reopened = AgentConfigFile::new(reloaded).unwrap().snapshot().unwrap();
    assert_eq!(reopened.saved_value, reopened.running_value);
    assert!(!reopened.pending_restart);
}

#[test]
fn configuration_snapshot_exposes_agent_domain_with_independent_revision() {
    let (center, _database, _directory) = test_center();
    let (_file, running, _agent_database, _agent_path) = test_agent_file();
    let center = center.with_running_agent_config(running).unwrap();

    let initial = center.current_snapshot().unwrap();
    let agent = initial.agent.unwrap();
    assert_eq!(agent.source, ConfigValueSource::AgentToml);
    assert!(agent.editable);
    assert!(!agent.read_only);
    assert!(!agent.pending_restart);
    assert_ne!(agent.revision, initial.revision);

    let saved = center
        .update_agent(
            &agent.revision,
            &[AgentConfigChange::SetModelRoute {
                name: "private_main".to_owned(),
                candidates: vec!["openai:gpt-snapshot-test".to_owned()],
            }],
        )
        .unwrap();
    assert!(saved.pending_restart);
    assert_eq!(saved.source, ConfigValueSource::AgentToml);
}

#[test]
fn agent_scene_tool_calling_save_reloads_private_and_group_policy() {
    let (file, running, _database, path) = test_agent_file();
    let mut private = running.document().unwrap().scenes.private.clone();
    private.tool_calling_enabled = false;
    let mut group = running.document().unwrap().scenes.group.clone();
    group.tool_calling_enabled = true;
    let initial = file.snapshot().unwrap();

    file.update(
        &initial.revision,
        &[
            AgentConfigChange::SetScene {
                scene: ChatScene::Private,
                config: private,
            },
            AgentConfigChange::SetScene {
                scene: ChatScene::Group,
                config: group,
            },
        ],
    )
    .unwrap();

    let environment = HashMap::from([(
        crate::config::agent::AGENT_CONFIG_FILE_ENV.to_owned(),
        path.to_string_lossy().into_owned(),
    )]);
    let reloaded = AgentRuntimeConfig::load_from_environment(&environment).unwrap();
    assert!(
        !reloaded
            .resolve(ChatScene::Private)
            .unwrap()
            .tool_calling_enabled
    );
    let group = reloaded.resolve(ChatScene::Group).unwrap();
    assert!(group.tool_calling_enabled);
    assert!(group.group_tool_calling_enabled);
}

#[test]
fn agent_save_rejects_stale_revision_without_overwriting_manual_change() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let mut manual = std::fs::read_to_string(&path).unwrap();
    manual.push_str("\n# manual concurrent edit\n");
    std::fs::write(&path, &manual).unwrap();

    let error = file
        .update(
            &initial.revision,
            &[AgentConfigChange::SetSearchRoute {
                name: "private_search".to_owned(),
                model: "gpt-concurrent".to_owned(),
            }],
        )
        .unwrap_err();
    assert_eq!(error.code(), "config_conflict");
    assert_eq!(std::fs::read_to_string(path).unwrap(), manual);
}

#[test]
fn invalid_agent_references_are_rejected_before_replacing_file() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let before = std::fs::read(&path).unwrap();
    let invalid_profile = AgentProfileConfig {
        main_route: "missing-route".to_owned(),
        aux_route: None,
        reasoning_effort: None,
        max_tool_rounds: 3,
        max_output_tokens: Some(1000),
    };

    let error = file
        .update(
            &initial.revision,
            &[AgentConfigChange::SetProfile {
                name: "broken".to_owned(),
                profile: invalid_profile,
            }],
        )
        .unwrap_err();
    assert_eq!(error.code(), "invalid_config");
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn partial_agent_save_preserves_custom_provider_routes_profiles_scenes_and_tools() {
    let (file, running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let custom_profile = AgentProfileConfig {
        main_route: "custom_route".to_owned(),
        aux_route: Some("aux".to_owned()),
        reasoning_effort: None,
        max_tool_rounds: 4,
        max_output_tokens: Some(1800),
    };
    let mut group: AgentSceneConfig = running.document().unwrap().scenes.group.clone();
    group.enabled_tools = vec!["save_memory".to_owned(), "list_todos".to_owned()];
    let first = file
        .update(
            &initial.revision,
            &[
                AgentConfigChange::SetModelRoute {
                    name: "custom_route".to_owned(),
                    candidates: vec!["mimo:mimo-v2.5".to_owned()],
                },
                AgentConfigChange::SetProfile {
                    name: "custom_profile".to_owned(),
                    profile: custom_profile,
                },
                AgentConfigChange::SetScene {
                    scene: ChatScene::Group,
                    config: group,
                },
            ],
        )
        .unwrap();

    file.update(
        &first.revision,
        &[AgentConfigChange::SetSearchRoute {
            name: "private_search".to_owned(),
            model: "gpt-after-partial-save".to_owned(),
        }],
    )
    .unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[providers.mimo]"));
    assert!(text.contains("[model_routes.custom_route]"));
    assert!(text.contains("[profiles.custom_profile]"));
    assert!(text.contains("list_todos"));
    let reloaded = AgentRuntimeConfig::from_toml(
        &text,
        AgentConfigSource::File(path.to_string_lossy().into_owned()),
    )
    .unwrap();
    assert_eq!(
        reloaded.resolve(ChatScene::Private).unwrap().search_model,
        "gpt-after-partial-save"
    );
}

#[cfg(unix)]
#[test]
fn agent_symlink_read_only_and_unsafe_permissions_are_not_writable() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let before = std::fs::read(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let read_only = file.snapshot().unwrap();
    assert!(read_only.read_only);
    assert!(!read_only.editable);
    let error = file
        .update(
            &initial.revision,
            &[AgentConfigChange::SetModelRoute {
                name: "private_main".to_owned(),
                candidates: vec!["openai:must-not-save".to_owned()],
            }],
        )
        .unwrap_err();
    assert_eq!(error.code(), "config_io_error");
    assert_eq!(std::fs::read(&path).unwrap(), before);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o622)).unwrap();
    assert!(file.snapshot().unwrap().read_only);

    let link = path.with_file_name("agent-linked.toml");
    symlink(&path, &link).unwrap();
    let linked_running = AgentRuntimeConfig::from_toml(
        std::str::from_utf8(&before).unwrap(),
        AgentConfigSource::File(link.to_string_lossy().into_owned()),
    )
    .unwrap();
    let error = AgentConfigFile::new(linked_running)
        .unwrap()
        .snapshot()
        .unwrap_err();
    assert_eq!(error.code(), "config_io_error");
}

#[test]
fn runtime_registry_has_no_agent_policy_duplicates() {
    let fields = crate::config::managed_config_fields();
    for forbidden in [
        "LLM_PROVIDER",
        "LLM_MODEL",
        "DEEPSEEK_MODEL",
        "BIGMODEL_MODEL",
        "GEMINI_MODEL",
        "TOOL_CALLING_ENABLED",
        "TOOL_CALLING_GROUP_ENABLED",
        "TOOL_CALLING_MAX_ROUNDS",
        "PRIVATE_LLM_MODEL",
        "GROUP_LLM_MODEL",
        "OPENAI_SEARCH_MODEL",
        "PRIVATE_OPENAI_SEARCH_MODEL",
        "GROUP_OPENAI_SEARCH_MODEL",
        "TITLE_MODEL",
        "MEMORY_MODEL",
        "COMPACT_MODEL",
        "TRANSLATION_MODEL",
        "LLM_MAX_OUTPUT_TOKENS",
    ] {
        assert!(
            fields.iter().all(|field| field.env_name != forbidden),
            "{forbidden} must not be persisted in runtime.toml"
        );
    }
}

#[test]
fn runtime_registry_qweather_hosts_match_runtime_defaults() {
    let fields = crate::config::managed_config_fields();
    let default_for = |key| {
        fields
            .iter()
            .find(|field| field.key == key)
            .and_then(|field| field.default_value)
    };

    assert_eq!(
        default_for("weather.qweather.api_host"),
        Some(crate::runtime::tools::weather::DEFAULT_QWEATHER_API_HOST)
    );
    assert_eq!(
        default_for("weather.qweather.geo_host"),
        Some(crate::runtime::tools::weather::DEFAULT_QWEATHER_GEO_HOST)
    );
}
