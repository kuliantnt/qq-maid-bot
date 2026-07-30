use super::*;

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
fn agent_knowledge_embedding_save_uses_agent_toml_and_reports_running_value() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();

    let saved = file
        .update(
            &initial.revision,
            &[AgentConfigChange::SetKnowledge {
                mode: KnowledgeRetrievalMode::Preflight,
                embedding: KnowledgeEmbeddingConfig {
                    enabled: true,
                    cache_dir: "cache/knowledge-embedding".to_owned(),
                },
            }],
        )
        .unwrap();

    assert_eq!(saved.source, ConfigValueSource::AgentToml);
    assert!(saved.pending_restart);
    assert_eq!(
        saved
            .saved_value
            .as_ref()
            .and_then(|value| value.get("knowledge"))
            .and_then(|value| value.get("embedding"))
            .and_then(|value| value.get("enabled"))
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        saved
            .running_value
            .as_ref()
            .and_then(|value| value.get("knowledge"))
            .and_then(|value| value.get("embedding"))
            .and_then(|value| value.get("enabled"))
            .and_then(Value::as_bool),
        Some(false)
    );

    let environment = HashMap::from([(
        crate::config::agent::AGENT_CONFIG_FILE_ENV.to_owned(),
        path.to_string_lossy().into_owned(),
    )]);
    let reloaded = AgentRuntimeConfig::load_from_environment(&environment).unwrap();
    assert!(reloaded.knowledge_embedding().enabled);
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
    private.enabled_tools = vec!["web_search".to_owned(), "save_memory".to_owned()];
    let mut group = running.document().unwrap().scenes.group.clone();
    group.tool_calling_enabled = true;
    group.enabled_tools = vec!["knowledge_search".to_owned()];
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
    assert_eq!(
        reloaded.resolve(ChatScene::Private).unwrap().enabled_tools,
        vec!["web_search", "save_memory"]
    );
    let group = reloaded.resolve(ChatScene::Group).unwrap();
    assert!(group.tool_calling_enabled);
    assert!(group.group_tool_calling_enabled);
    assert_eq!(group.enabled_tools, vec!["knowledge_search"]);
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
            model: "openai:gpt-after-partial-save".to_owned(),
        }],
    )
    .unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[providers.mimo]"));
    assert!(text.contains("[model_routes.custom_route]"));
    assert!(text.contains("[profiles.custom_profile]"));
    assert!(text.contains("[tools.web_search.routes.private_search]"));
    assert!(text.contains("list_todos"));
    let reloaded = AgentRuntimeConfig::from_toml(
        &text,
        AgentConfigSource::File(path.to_string_lossy().into_owned()),
    )
    .unwrap();
    assert_eq!(
        reloaded.resolve(ChatScene::Private).unwrap().search_model,
        "openai:gpt-after-partial-save"
    );
}

#[test]
fn agent_web_search_backend_switches_preserve_routes_and_parameters() {
    let (file, _running, _database, path) = test_agent_file();
    let mut revision = file.snapshot().unwrap().revision;

    for (backend, expected) in [
        ("tavily", WebSearchBackend::Tavily),
        ("disabled", WebSearchBackend::Disabled),
        ("provider_native", WebSearchBackend::ProviderNative),
    ] {
        let saved = file
            .update(
                &revision,
                &[AgentConfigChange::SetWebSearch {
                    backend: backend.to_owned(),
                    max_results: 8,
                    search_depth: "advanced".to_owned(),
                    topic: "news".to_owned(),
                    time_range: Some("week".to_owned()),
                    connect_timeout_seconds: 5,
                    first_response_timeout_seconds: 15,
                    total_timeout_seconds: 45,
                }],
            )
            .unwrap();
        revision = saved.revision;

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[tools.web_search.routes.private_search]"));
        assert!(text.contains("[tools.web_search.routes.group_search]"));
        let reloaded = AgentRuntimeConfig::from_toml(
            &text,
            AgentConfigSource::File(path.to_string_lossy().into_owned()),
        )
        .unwrap();
        assert_eq!(reloaded.web_search().default_backend, expected);
        assert_eq!(reloaded.web_search().max_results, 8);
        assert_eq!(
            reloaded.resolve(ChatScene::Private).unwrap().search_model,
            "gpt-5.6-luna"
        );
    }
}

#[test]
fn agent_web_search_update_rejects_invalid_parameters_without_replacing_file() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let before = std::fs::read(&path).unwrap();

    let error = file
        .update(
            &initial.revision,
            &[AgentConfigChange::SetWebSearch {
                backend: "tavily".to_owned(),
                max_results: 0,
                search_depth: "advanced".to_owned(),
                topic: "news".to_owned(),
                time_range: Some("week".to_owned()),
                connect_timeout_seconds: 20,
                first_response_timeout_seconds: 10,
                total_timeout_seconds: 30,
            }],
        )
        .unwrap_err();

    assert_eq!(error.code(), "invalid_config");
    assert!(error.message().contains("max_results"));
    assert_eq!(std::fs::read(path).unwrap(), before);
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
