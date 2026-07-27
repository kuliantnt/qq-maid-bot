use qq_maid_llm::provider::types::ModelProvider;

use crate::config::agent::AgentProviderKind;

use super::*;

#[test]
fn agent_provider_add_modify_and_remove_preserves_other_providers() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let added = file
        .update(
            &initial.revision,
            &[AgentConfigChange::SetProvider {
                id: "opencode_zen".to_owned(),
                provider: AgentProviderUpdate {
                    kind: AgentProviderKind::OpenAiResponses,
                    base_url: "https://opencode.ai/zen/v1".to_owned(),
                    api_key_env: "OPENCODE_API_KEY".to_owned(),
                    auth_header: "Authorization".to_owned(),
                    auth_scheme: Some("Bearer".to_owned()),
                    request_timeout_seconds: None,
                    chat_fallback: Some(false),
                },
            }],
        )
        .unwrap();
    let added_text = std::fs::read_to_string(&path).unwrap();
    assert!(added_text.contains("[providers.opencode_zen]"));
    assert!(added_text.contains("kind = \"openai_responses\""));
    assert!(added_text.contains("chat_fallback = false"));
    assert!(added_text.contains("[providers.mimo]"));

    let modified = file
        .update(
            &added.revision,
            &[AgentConfigChange::SetProvider {
                id: "opencode_zen".to_owned(),
                provider: AgentProviderUpdate {
                    kind: AgentProviderKind::OpenAiResponses,
                    base_url: "https://gateway.example/open-code/v1".to_owned(),
                    api_key_env: "OPENCODE_API_KEY".to_owned(),
                    auth_header: "Authorization".to_owned(),
                    auth_scheme: Some("Bearer".to_owned()),
                    request_timeout_seconds: Some(30),
                    chat_fallback: Some(false),
                },
            }],
        )
        .unwrap();
    let modified_text = std::fs::read_to_string(&path).unwrap();
    assert!(modified_text.contains("https://gateway.example/open-code/v1"));
    assert!(modified_text.contains("request_timeout_seconds = 30"));
    assert!(modified_text.contains("[providers.mimo]"));

    file.update(
        &modified.revision,
        &[AgentConfigChange::RemoveProvider {
            id: "opencode_zen".to_owned(),
        }],
    )
    .unwrap();
    let removed_text = std::fs::read_to_string(path).unwrap();
    assert!(!removed_text.contains("[providers.opencode_zen]"));
    assert!(removed_text.contains("[providers.mimo]"));
}

#[test]
fn invalid_agent_provider_change_does_not_replace_file() {
    let (file, _running, _database, path) = test_agent_file();
    for (id, base_url) in [
        ("openai", "https://example.com/v1"),
        ("opencode_zen", "invalid-url"),
    ] {
        let before = std::fs::read(&path).unwrap();
        let revision = file.snapshot().unwrap().revision;
        let error = file
            .update(
                &revision,
                &[AgentConfigChange::SetProvider {
                    id: id.to_owned(),
                    provider: AgentProviderUpdate {
                        kind: AgentProviderKind::OpenAiResponses,
                        base_url: base_url.to_owned(),
                        api_key_env: "OPENCODE_API_KEY".to_owned(),
                        auth_header: "Authorization".to_owned(),
                        auth_scheme: Some("Bearer".to_owned()),
                        request_timeout_seconds: None,
                        chat_fallback: Some(false),
                    },
                }],
            )
            .unwrap_err();
        assert_eq!(error.code(), "invalid_config");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn chat_fallback_true_is_rejected_without_replacing_agent_file() {
    let (file, _running, _database, path) = test_agent_file();
    let before = std::fs::read(&path).unwrap();
    let revision = file.snapshot().unwrap().revision;
    let error = file
        .update(
            &revision,
            &[AgentConfigChange::SetProvider {
                id: "custom_responses".to_owned(),
                provider: AgentProviderUpdate {
                    kind: AgentProviderKind::OpenAiResponses,
                    base_url: "https://example.com/v1".to_owned(),
                    api_key_env: "CUSTOM_API_KEY".to_owned(),
                    auth_header: "Authorization".to_owned(),
                    auth_scheme: Some("Bearer".to_owned()),
                    request_timeout_seconds: None,
                    chat_fallback: Some(true),
                },
            }],
        )
        .unwrap_err();

    assert_eq!(error.code(), "invalid_config");
    assert!(error.message().contains("chat_fallback=true"));
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn provider_changes_support_all_three_opencode_presets_with_one_key_env() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let changes = [
        (
            "opencode_zen",
            AgentProviderKind::OpenAiResponses,
            "https://opencode.ai/zen/v1",
        ),
        (
            "opencode_zen_chat",
            AgentProviderKind::OpenAiCompatible,
            "https://opencode.ai/zen/v1",
        ),
        (
            "opencode_go",
            AgentProviderKind::OpenAiCompatible,
            "https://opencode.ai/zen/go/v1",
        ),
    ]
    .into_iter()
    .map(|(id, kind, base_url)| AgentConfigChange::SetProvider {
        id: id.to_owned(),
        provider: AgentProviderUpdate {
            kind,
            base_url: base_url.to_owned(),
            api_key_env: "OPENCODE_API_KEY".to_owned(),
            auth_header: "Authorization".to_owned(),
            auth_scheme: Some("Bearer".to_owned()),
            request_timeout_seconds: None,
            chat_fallback: (kind == AgentProviderKind::OpenAiResponses).then_some(false),
        },
    })
    .collect::<Vec<_>>();

    file.update(&initial.revision, &changes).unwrap();
    let reloaded = AgentRuntimeConfig::from_toml(
        &std::fs::read_to_string(&path).unwrap(),
        AgentConfigSource::File(path.to_string_lossy().into_owned()),
    )
    .unwrap();
    let providers = reloaded.provider_configs();
    for id in ["opencode_zen", "opencode_zen_chat", "opencode_go"] {
        let provider = providers
            .iter()
            .find(|provider| provider.id == ModelProvider::Custom(id.to_owned()))
            .unwrap();
        assert_eq!(provider.api_key_env, "OPENCODE_API_KEY");
    }
}

#[test]
fn opencode_api_key_is_managed_as_restart_secret_without_plaintext_snapshot() {
    let fields = crate::config::managed_config_fields();
    let field = fields
        .iter()
        .find(|field| field.key == "provider.opencode.api_key")
        .expect("managed OpenCode API key field");
    assert_eq!(field.env_name, "OPENCODE_API_KEY");
    assert_eq!(field.sensitivity, ManagedConfigSensitivity::Secret);
    assert_eq!(field.apply_mode, ManagedConfigApplyMode::Restart);

    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-opencode-secret", &[CONFIG_SECRET_SCHEMA_V1])
            .unwrap();
    let center = ConfigCenter::open(
        fields,
        ConfigCenterPaths {
            managed_config_file: directory.join("config/runtime.toml"),
            master_key_file: directory.join("config/secrets/master.key"),
        },
        database,
    )
    .unwrap();
    center
        .replace_secret(
            "provider.opencode.api_key",
            "must-never-appear",
            SECRET_MISSING_REVISION,
        )
        .unwrap();
    let snapshot = center.current_snapshot().unwrap();
    let field = snapshot
        .fields
        .iter()
        .find(|field| field.key == "provider.opencode.api_key")
        .unwrap();
    assert!(field.configured);
    assert_eq!(field.saved_value, None);
    assert_eq!(field.effective_value, None);
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("must-never-appear")
    );
}
