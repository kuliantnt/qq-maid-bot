use super::*;

#[test]
fn save_and_reload_preserves_provider_qualified_same_model_candidates() {
    let (file, _running, _database, path) = test_agent_file();
    let initial = file.snapshot().unwrap();
    let provider = |id: &str| AgentConfigChange::SetProvider {
        id: id.to_owned(),
        provider: AgentProviderUpdate {
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
