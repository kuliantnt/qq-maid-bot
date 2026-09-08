use super::*;

fn managed_provider(enabled: bool) -> AgentProviderUpdate {
    AgentProviderUpdate {
        display_name: None,
        enabled,
        kind: AgentProviderKind::OpenAiResponses,
        base_url: "https://provider.example/v1".into(),
        api_key_env: String::new(),
        auth_header: "Authorization".into(),
        auth_scheme: Some("Bearer".into()),
        request_timeout_seconds: Some(15),
        chat_fallback: Some(false),
    }
}

#[test]
fn builtin_disable_is_rejected_even_during_incomplete_setup() {
    let (_old_center, database, directory) = test_center();
    let (_file, running, _agent_database, _path) = test_agent_file();
    let center = ConfigCenter::open(
        crate::config::managed_config_fields(),
        ConfigCenterPaths {
            managed_config_file: directory.join("config/runtime.toml"),
            master_key_file: directory.join("config/secrets/master.key"),
        },
        database,
    )
    .unwrap()
    .with_running_agent_config(running)
    .unwrap()
    .with_incomplete_setup_writes();
    let initial = center.current_snapshot().unwrap();
    let error = center
        .update_managed(
            &initial.revision,
            &[ManagedConfigChange::Set {
                key: "provider.openai.enabled".into(),
                value: Value::Boolean(false),
            }],
        )
        .unwrap_err();
    assert!(error.message().contains("disabled"));
    assert!(error.message().contains("model_routes"));
    assert!(error.message().contains("tools.web_search.routes"));
    assert_eq!(
        center.current_snapshot().unwrap().revision,
        initial.revision
    );
}

#[test]
fn trusted_presets_keep_existing_ids_and_supported_adapters_without_credentials() {
    let presets = provider_presets();
    for id in [
        "opencode_zen",
        "opencode_zen_chat",
        "opencode_go",
        "openai_custom",
        "deepseek_custom",
        "bigmodel_custom",
        "gemini_custom",
    ] {
        assert!(presets.iter().any(|preset| preset.id == id));
    }
    let serialized = serde_json::to_string(&presets).unwrap();
    assert!(!serialized.contains("api_key"));
    assert!(!serialized.contains("credential"));
    assert_eq!(presets[0].kind, AgentProviderKind::OpenAiResponses);
    assert_eq!(presets[1].kind, AgentProviderKind::OpenAiCompatible);
    assert_eq!(presets[2].base_url, "https://opencode.ai/zen/go/v1");
}

#[test]
fn managed_connection_credentials_are_isolated_revision_checked_and_never_reused() {
    let (center, _database, _directory) = test_center();
    let (_file, running, _agent_database, _path) = test_agent_file();
    let center = center.with_running_agent_config(running).unwrap();
    let initial = center.current_snapshot().unwrap().agent.unwrap();
    let saved = center
        .update_agent(
            &initial.revision,
            &[AgentConfigChange::SetProvider {
                id: "custom_test".into(),
                provider: managed_provider(true),
            }],
        )
        .unwrap();
    let slot = saved.saved_value.as_ref().unwrap()["providers"]["custom_test"]["api_key_env"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(slot.starts_with("QQ_MAID_CONNECTION_"));
    center
        .update_connection_credential(
            "custom_test",
            &saved.revision,
            "missing",
            Some("test-only-placeholder-key"),
        )
        .unwrap();
    let snapshot = center.current_snapshot().unwrap();
    assert!(snapshot.providers.credentials["custom_test"].configured);
    assert!(snapshot.providers.credentials["custom_test"].pending_restart);
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("test-only-placeholder-key")
    );
    assert_eq!(
        center.current_resolved_environment().unwrap()[&slot],
        "test-only-placeholder-key"
    );
    assert!(
        center
            .update_connection_credential(
                "custom_test",
                &saved.revision,
                "missing",
                Some("stale-key")
            )
            .is_err()
    );
    let removed = center
        .update_agent(
            &saved.revision,
            &[AgentConfigChange::RemoveProvider {
                id: "custom_test".into(),
            }],
        )
        .unwrap();
    let recreated = center
        .update_agent(
            &removed.revision,
            &[AgentConfigChange::SetProvider {
                id: "custom_test".into(),
                provider: managed_provider(true),
            }],
        )
        .unwrap();
    assert_ne!(
        recreated.saved_value.unwrap()["providers"]["custom_test"]["api_key_env"]
            .as_str()
            .unwrap(),
        slot
    );
    assert!(!center.provider_snapshot().unwrap().credentials["custom_test"].configured);
}

#[test]
fn management_api_updates_exact_legacy_key_but_rejects_case_rename() {
    let (center, _database, _directory) = test_center();
    let (file, _running, _agent_database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let mut historical = managed_provider(true);
    historical.api_key_env = "TEST_PROXY_KEY".into();
    file.update(
        &initial.revision,
        &[AgentConfigChange::SetProvider {
            id: "MyProxy".into(),
            provider: historical,
        }],
    )
    .unwrap();
    let center = center
        .with_external_environment(HashMap::from([(
            "TEST_PROXY_KEY".into(),
            "test-only-placeholder-key".into(),
        )]))
        .with_running_agent_config(file.current_runtime().unwrap())
        .unwrap();
    let initial = center.current_snapshot().unwrap().agent.unwrap();
    let mut update = managed_provider(false);
    update.display_name = Some("历史代理".into());
    update.base_url = "https://legacy-proxy.example/v1".into();
    let saved = center
        .update_agent(
            &initial.revision,
            &[AgentConfigChange::SetProvider {
                id: "MyProxy".into(),
                provider: update,
            }],
        )
        .unwrap();
    let provider = &saved.saved_value.unwrap()["providers"]["MyProxy"];
    assert_eq!(provider["display_name"].as_str(), Some("历史代理"));
    assert_eq!(provider["enabled"].as_bool(), Some(false));
    assert_eq!(
        provider["base_url"].as_str(),
        Some("https://legacy-proxy.example/v1")
    );
    assert_eq!(provider["api_key_env"].as_str(), Some("TEST_PROXY_KEY"));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("[providers.MyProxy]")
    );

    let error = center
        .update_agent(
            &saved.revision,
            &[AgentConfigChange::SetProvider {
                id: "myproxy".into(),
                provider: managed_provider(true),
            }],
        )
        .unwrap_err();
    assert!(error.message().contains("原始 ID"));

    let update = managed_provider(true);
    let error = center
        .update_agent(
            &saved.revision,
            &[
                AgentConfigChange::SetProvider {
                    id: "MyProxy".into(),
                    provider: update.clone(),
                },
                AgentConfigChange::SetProvider {
                    id: "myproxy".into(),
                    provider: update,
                },
            ],
        )
        .unwrap_err();
    assert!(error.message().contains("同一请求不能重复修改"));
}

#[test]
fn management_api_still_rejects_bad_new_connection_id() {
    let (center, _database, _directory) = test_center();
    let (_file, running, _agent_database, _path) = test_agent_file();
    let center = center.with_running_agent_config(running).unwrap();
    let initial = center.current_snapshot().unwrap().agent.unwrap();
    let error = center
        .update_agent(
            &initial.revision,
            &[AgentConfigChange::SetProvider {
                id: "BadID".into(),
                provider: managed_provider(true),
            }],
        )
        .unwrap_err();
    assert!(error.message().contains("小写"));
    assert_eq!(
        center.current_snapshot().unwrap().agent.unwrap().revision,
        initial.revision
    );
}

#[test]
fn managed_connection_rejects_arbitrary_credential_namespace_and_rebinding() {
    let (center, _database, _directory) = test_center();
    let (_file, running, _agent_database, _path) = test_agent_file();
    let center = center.with_running_agent_config(running).unwrap();
    let initial = center.current_snapshot().unwrap().agent.unwrap();
    let mut provider = managed_provider(true);
    provider.api_key_env = "QQ_BOT_APP_SECRET".into();
    assert!(
        center
            .update_agent(
                &initial.revision,
                &[AgentConfigChange::SetProvider {
                    id: "unsafe".into(),
                    provider
                }]
            )
            .is_err()
    );
    assert_eq!(
        center.current_snapshot().unwrap().agent.unwrap().revision,
        initial.revision
    );
}

#[test]
fn disabling_and_removing_connection_reports_all_route_references_without_writes() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let mut provider = managed_provider(true);
    provider.api_key_env = "TEST_API_KEY".into();
    let saved = file
        .update(
            &initial.revision,
            &[
                AgentConfigChange::SetProvider {
                    id: "test_connection".into(),
                    provider: provider.clone(),
                },
                AgentConfigChange::SetModelRoute {
                    name: "private_main".into(),
                    candidates: vec!["test_connection:test-model".into()],
                },
                AgentConfigChange::SetSearchRoute {
                    name: "unused_search".into(),
                    model: "test_connection:test-model".into(),
                },
            ],
        )
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    provider.enabled = false;
    for change in [
        AgentConfigChange::SetProvider {
            id: "test_connection".into(),
            provider,
        },
        AgentConfigChange::RemoveProvider {
            id: "test_connection".into(),
        },
    ] {
        let error = file.update(&saved.revision, &[change]).unwrap_err();
        assert!(error.message().contains("model_routes.private_main"));
        assert!(
            error
                .message()
                .contains("tools.web_search.routes.unused_search")
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    // 直接加载手工修改的配置也走相同预检，不依赖 Web 写接口。
    let text = String::from_utf8(before)
        .unwrap()
        .replace("enabled = true\nkind", "enabled = false\nkind");
    assert!(
        crate::config::agent::AgentRuntimeConfig::from_toml(
            &text,
            crate::config::agent::AgentConfigSource::File("test.toml".into())
        )
        .is_err()
    );
}

#[test]
fn save_and_reload_preserves_provider_qualified_same_model_candidates() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let provider = |id: &str| AgentConfigChange::SetProvider {
        id: id.to_owned(),
        provider: AgentProviderUpdate {
            display_name: None,
            enabled: true,
            kind: AgentProviderKind::OpenAiCompatible,
            base_url: format!("https://{id}.example/v1"),
            api_key_env: format!("{}_API_KEY", id.to_ascii_uppercase()),
            auth_header: "Authorization".to_owned(),
            auth_scheme: Some("Bearer".to_owned()),
            request_timeout_seconds: None,
            chat_fallback: None,
        },
    };
    let candidates = vec![
        "routera:gpt-5.6-luna".to_owned(),
        "routerb:gpt-5.6-luna".to_owned(),
    ];

    let saved = file
        .update(
            &initial.revision,
            &[
                provider("routera"),
                provider("routerb"),
                AgentConfigChange::SetModelRoute {
                    name: "private_main".to_owned(),
                    candidates: candidates.clone(),
                },
            ],
        )
        .unwrap();

    assert!(saved.pending_restart);
    let text = std::fs::read_to_string(&path).unwrap();
    let document: toml::Value = toml::from_str(&text).unwrap();
    let saved_candidates = document["model_routes"]["private_main"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(saved_candidates, candidates);

    let environment = HashMap::from([(
        crate::config::agent::AGENT_CONFIG_FILE_ENV.to_owned(),
        path.to_string_lossy().into_owned(),
    )]);
    let reloaded = AgentRuntimeConfig::load_from_environment(&environment).unwrap();
    assert_eq!(
        reloaded.resolve(ChatScene::Private).unwrap().main_model,
        "routera:gpt-5.6-luna,routerb:gpt-5.6-luna"
    );
}

#[test]
fn bare_route_disable_is_rejected_during_setup_without_writes() {
    let (center, _database, _directory) = test_center();
    let (file, _running, _agent_database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    file.update(
        &initial.revision,
        &[AgentConfigChange::SetModelRoute {
            name: "private_main".into(),
            candidates: vec!["test-model".into()],
        }],
    )
    .unwrap();
    let center = center
        .with_running_agent_config(file.current_runtime().unwrap())
        .unwrap()
        .with_incomplete_setup_writes();
    let initial = center.current_snapshot().unwrap().agent.unwrap();
    let before = std::fs::read(&path).unwrap();
    let environment = HashMap::from([
        ("OPENAI_ENABLED".into(), "false".into()),
        ("DEEPSEEK_API_KEY".into(), "test-key".into()),
    ]);
    let error = center
        .validate_candidate_for_write(&environment, None)
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("model_routes.private_main.candidates[0]: provider `openai` is disabled")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        center.current_snapshot().unwrap().agent.unwrap().revision,
        initial.revision
    );
}

#[tokio::test]
async fn builtin_diagnostics_use_formal_chat_only_aliases() {
    use axum::{Json, Router, routing::post};
    use qq_maid_llm::provider::openai::diagnostics::ProbeState;
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async { Json(serde_json::json!({"choices": [{"message": {"content": "OK"}}]})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for mode in ["chat_only", "CHAT_ONLY", "chat-only", "  ChAt-OnLy  "] {
        let (center, _database, _directory) = test_center();
        let center = center.with_external_environment(HashMap::from([
            ("OPENAI_API_MODE".into(), mode.into()),
            ("OPENAI_BASE_URLS".into(), base.clone()),
            ("OPENAI_API_KEY".into(), "test-key".into()),
        ]));
        let revision = center.managed_file.load().unwrap().revision;
        let result = center
            .test_provider_connection("openai", &revision, "test-model")
            .await
            .unwrap();
        assert_eq!(result.model_call, ProbeState::Success, "{mode}: {result:?}");
    }
    server.abort();
}

#[test]
fn console_still_rejects_new_mixed_case_connection_ids() {
    let (center, _database, _directory) = test_center();
    let (_file, running, _agent_database, _path) = test_agent_file();
    let center = center.with_running_agent_config(running).unwrap();
    let initial = center.current_snapshot().unwrap().agent.unwrap();
    let error = center
        .update_agent(
            &initial.revision,
            &[AgentConfigChange::SetProvider {
                id: "MyProxy".into(),
                provider: managed_provider(true),
            }],
        )
        .unwrap_err();
    assert!(error.message().contains("小写"));
    assert_eq!(
        center.current_snapshot().unwrap().agent.unwrap().revision,
        initial.revision
    );
}
