use std::collections::HashMap;

use qq_maid_llm::web_search::WebSearchBackend;
use toml::Value;

use crate::{
    config::{
        AgentProfileConfig, AgentRuntimeConfig, AgentSceneConfig, ChatScene,
        agent::{
            AgentConfigSource, AgentProviderKind, KnowledgeEmbeddingConfig, KnowledgeRetrievalMode,
        },
    },
    storage::database::SqliteDatabase,
};

use super::*;

mod agent_file;
mod opencode;
mod provider_routes;
mod secret_storage;

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
        ManagedConfigField::public(
            "console.enabled",
            "WEB_CONSOLE_ENABLED",
            "core.console",
            ManagedConfigValueType::Boolean,
            ManagedConfigApplyMode::Restart,
            Some("true"),
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
    let text = include_str!("../../../../../runtime/config/agent.example.toml");
    std::fs::write(&path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // agent.toml 的受管写入要求目录和文件都不能由组或其他用户写入；显式设置
        // 测试权限，避免宿主机 umask 0002 让夹具被误判为不安全。
        std::fs::set_permissions(
            path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
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
fn tavily_api_key_is_managed_as_restart_secret() {
    let fields = crate::config::managed_config_fields();
    let field = fields
        .iter()
        .find(|field| field.key == "tools.web_search.tavily.api_key")
        .expect("managed Tavily API key field");

    assert_eq!(field.env_name, "TAVILY_API_KEY");
    assert_eq!(field.sensitivity, ManagedConfigSensitivity::Secret);
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
