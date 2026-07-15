use super::*;

#[test]
fn model_id_parse_accepts_custom_mimo_provider_prefix() {
    let model = ModelId::parse_config("mimo:mimo-v2.5-pro", "LLM_MODEL").unwrap();

    assert_eq!(
        model.provider,
        Some(ModelProvider::Custom("mimo".to_owned()))
    );
    assert_eq!(model.name, "mimo-v2.5-pro");
    assert_eq!(model.to_request_model(), "mimo:mimo-v2.5-pro");
}

#[test]
fn model_id_parse_accepts_gemini_provider_prefix() {
    let model = ModelId::parse_config("gemini:gemini-2.5-flash", "LLM_MODEL").unwrap();

    assert_eq!(model.provider, Some(ModelProvider::Gemini));
    assert_eq!(model.name, "gemini-2.5-flash");
    assert_eq!(model.to_request_model(), "gemini:gemini-2.5-flash");
}

#[test]
fn model_route_provider_forwards_supports_vision_to_selected_candidate() {
    let openai = Arc::new(MockProvider::new("openai", Vec::new()).with_vision());
    let deepseek = Arc::new(MockProvider::new("deepseek", Vec::new()));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-vision,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai),
            (ModelProvider::DeepSeek, deepseek),
        ],
    )
    .unwrap();

    assert!(provider.supports_vision(None));
    assert!(provider.supports_vision(Some("openai:gpt-vision")));
    assert!(!provider.supports_vision(Some("deepseek:deepseek-chat")));
}

#[test]
fn limiting_provider_forwards_supports_vision() {
    let vision = Arc::new(MockProvider::new("openai", Vec::new()).with_vision());
    let text = Arc::new(MockProvider::new("deepseek", Vec::new()));

    assert!(
        limiter::LimitingLlmProvider::new(vision, None).supports_vision(Some("openai:gpt-vision"))
    );
    assert!(
        !limiter::LimitingLlmProvider::new(text, None)
            .supports_vision(Some("deepseek:deepseek-chat"))
    );
}

#[test]
fn auto_default_route_appends_deepseek_fallback_for_single_openai_model() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());

    let route = auto_default_route(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        route.display(),
        "openai:gpt-5.4-mini,deepseek:deepseek-chat"
    );
    assert_eq!(
        provider.model(),
        "openai:gpt-5.4-mini,deepseek:deepseek-chat"
    );
}

#[test]
fn auto_default_route_keeps_explicit_candidate_order() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());

    let route = auto_default_route(&config).unwrap();

    assert_eq!(route.display(), "openai:gpt-5.4-mini,openai:gpt-5.4");
}

#[test]
fn auto_provider_set_includes_deepseek_from_translation_model() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(&mut config, "TRANSLATION_MODEL", "deepseek:deepseek-chat");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        providers,
        vec![ModelProvider::OpenAi, ModelProvider::DeepSeek]
    );
    assert_eq!(provider.model(), "openai:gpt-5.4-mini,openai:gpt-5.4");
}

#[test]
fn auto_provider_set_includes_bigmodel_from_translation_model() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    config.bigmodel_api_key = Some("test-bigmodel-key".to_owned());
    set_configured_route(&mut config, "TRANSLATION_MODEL", "bigmodel:glm-5.2");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        providers,
        vec![ModelProvider::OpenAi, ModelProvider::BigModel]
    );
    assert_eq!(provider.model(), "openai:gpt-5.4-mini");
}

#[test]
fn auto_provider_set_includes_specialty_deepseek_with_explicit_openai_main_chain() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(
        &mut config,
        "TRANSLATION_MODEL",
        "deepseek:deepseek-chat,openai:gpt-5.4-mini",
    );

    let default_route = auto_default_route(&config).unwrap();
    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        default_route.display(),
        "openai:gpt-5.4-mini,openai:gpt-5.4"
    );
    assert_eq!(
        providers,
        vec![ModelProvider::OpenAi, ModelProvider::DeepSeek]
    );
    assert_eq!(provider.model(), "openai:gpt-5.4-mini,openai:gpt-5.4");
}

#[test]
fn auto_provider_set_skips_specialty_deepseek_without_api_key() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "deepseek:deepseek-chat");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::OpenAi]);
    assert_eq!(provider.name(), "auto");
}

#[test]
fn auto_provider_set_skips_bigmodel_without_api_key() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "bigmodel:glm-5.2");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::OpenAi]);
    assert_eq!(provider.name(), "auto");
}

#[test]
fn auto_provider_set_keeps_openai_only_without_deepseek_key() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    set_configured_route(&mut config, "TITLE_MODEL", "openai:gpt-5.4-mini");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "openai:gpt-5.4-mini");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::OpenAi]);
    assert_eq!(provider.name(), "auto");
    assert_eq!(provider.model(), "openai:gpt-5.4-mini");
}

#[test]
fn auto_provider_set_deduplicates_repeated_specialty_providers() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(&mut config, "TITLE_MODEL", "deepseek:deepseek-chat");
    set_configured_route(
        &mut config,
        "COMPACT_MODEL",
        "deepseek:deepseek-chat,openai:gpt-5.4-mini",
    );
    set_configured_route(&mut config, "MEMORY_MODEL", "deepseek:deepseek-chat");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "deepseek:deepseek-chat");

    let providers = auto_required_provider_kinds(&config).unwrap();

    assert_eq!(
        providers,
        vec![ModelProvider::OpenAi, ModelProvider::DeepSeek]
    );
}

#[test]
fn auto_deepseek_only_does_not_require_openai_provider() {
    let mut config = app_config(ProviderMode::Auto, "deepseek:deepseek-chat");
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::DeepSeek]);
    assert_eq!(provider.name(), "auto");
    assert_eq!(provider.model(), "deepseek:deepseek-chat");
}

#[test]
fn auto_deepseek_only_agent_routes_do_not_initialize_openai() {
    let mut config = app_config(ProviderMode::Auto, "deepseek:deepseek-chat");
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "deepseek:deepseek-chat",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "deepseek:deepseek-chat",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.aux",
        "deepseek:deepseek-chat",
    );

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::DeepSeek]);
    assert_eq!(provider.name(), "auto");
    assert_eq!(provider.model(), "deepseek:deepseek-chat");
}

#[test]
fn auto_bigmodel_only_agent_routes_do_not_initialize_openai() {
    let mut config = app_config(ProviderMode::Auto, "bigmodel:glm-5.2");
    config.openai_api_key = None;
    config.bigmodel_api_key = Some("test-bigmodel-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "bigmodel:glm-5.2",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "bigmodel:glm-5.2",
    );
    set_configured_route(&mut config, "agent.model_routes.aux", "bigmodel:glm-5.2");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::BigModel]);
    assert_eq!(provider.name(), "auto");
    assert_eq!(provider.model(), "bigmodel:glm-5.2");
}

#[test]
fn auto_gemini_only_agent_routes_do_not_initialize_openai() {
    let mut config = app_config(ProviderMode::Auto, "gemini:gemini-2.5-flash");
    config.openai_api_key = None;
    config.gemini_api_key = Some("test-gemini-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "gemini:gemini-2.5-flash",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "gemini:gemini-2.5-flash",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.aux",
        "gemini:gemini-2.5-flash",
    );

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::Gemini]);
    assert_eq!(provider.name(), "auto");
    assert_eq!(provider.model(), "gemini:gemini-2.5-flash");
}

#[test]
fn auto_provider_set_includes_configured_mimo_provider() {
    let mut config = app_config(
        ProviderMode::Auto,
        "mimo:mimo-v2.5-pro,deepseek:deepseek-chat",
    );
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    config
        .openai_compatible_providers
        .push(mimo_provider_config(Some("test-mimo-key")));

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        providers,
        vec![
            ModelProvider::DeepSeek,
            ModelProvider::Custom("mimo".to_owned())
        ]
    );
    assert_eq!(provider.name(), "auto");
    assert_eq!(
        provider.model(),
        "mimo:mimo-v2.5-pro,deepseek:deepseek-chat"
    );
}

#[test]
fn auto_provider_set_skips_mimo_without_api_key() {
    let mut config = app_config(
        ProviderMode::Auto,
        "mimo:mimo-v2.5-pro,deepseek:deepseek-chat",
    );
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    config
        .openai_compatible_providers
        .push(mimo_provider_config(None));

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::DeepSeek]);
    assert_eq!(provider.name(), "auto");
}

#[test]
fn auto_provider_rejects_undeclared_custom_provider() {
    let mut config = app_config(
        ProviderMode::Auto,
        "mimmo:mimo-v2.5-pro,deepseek:deepseek-chat",
    );
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject undeclared custom provider"),
        Err(err) => err,
    };

    assert_eq!(err.stage, "config");
    assert!(
        err.message.contains("providers.mimmo is not configured"),
        "{}",
        err.message
    );
}

#[test]
fn auto_requires_at_least_one_referenced_provider_api_key() {
    let mut config = app_config(ProviderMode::Auto, "deepseek:deepseek-chat");
    config.openai_api_key = None;
    config.deepseek_api_key = None;
    config.bigmodel_api_key = Some("unused-bigmodel-key".to_owned());

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject auto routes with no available provider"),
        Err(err) => err,
    };

    assert_eq!(err.code, "config");
    assert!(err.message.contains("no LLM provider is available"));
    assert!(!err.message.contains("BIGMODEL_API_KEY"));
}

#[test]
fn fixed_provider_modes_validate_specialty_routes_at_startup() {
    let mut config = app_config(ProviderMode::OpenAi, "openai:gpt-5.4-mini");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "deepseek:deepseek-chat");

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject cross-provider specialty route"),
        Err(err) => err,
    };

    assert_eq!(err.code, "config");
    assert!(err.message.contains("TRANSLATION_MODEL"));
    assert!(err.message.contains("requires provider `deepseek`"));
}

#[test]
fn fixed_deepseek_provider_accepts_deepseek_only_agent_routes() {
    let mut config = app_config(ProviderMode::DeepSeek, "deepseek:deepseek-chat");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "deepseek:deepseek-chat",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "deepseek:deepseek-chat",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.aux",
        "deepseek:deepseek-chat",
    );

    let provider = build_provider(&config).unwrap();

    assert_eq!(provider.name(), "deepseek");
    assert_eq!(provider.model(), "deepseek:deepseek-chat");
}

#[test]
fn fixed_bigmodel_provider_validates_specialty_routes_at_startup() {
    let mut config = app_config(ProviderMode::BigModel, "bigmodel:glm-5.2");
    config.bigmodel_api_key = Some("test-bigmodel-key".to_owned());
    set_configured_route(&mut config, "TRANSLATION_MODEL", "openai:gpt-5.4-mini");

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject cross-provider specialty route"),
        Err(err) => err,
    };

    assert_eq!(err.code, "config");
    assert!(err.message.contains("TRANSLATION_MODEL"));
    assert!(err.message.contains("requires provider `openai`"));
}

#[test]
fn fixed_bigmodel_provider_accepts_bigmodel_only_agent_routes() {
    let mut config = app_config(ProviderMode::BigModel, "bigmodel:glm-5.2");
    config.bigmodel_api_key = Some("test-bigmodel-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "bigmodel:glm-5.2",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "bigmodel:glm-5.2",
    );
    set_configured_route(&mut config, "agent.model_routes.aux", "bigmodel:glm-5.2");

    let provider = build_provider(&config).unwrap();

    assert_eq!(provider.name(), "bigmodel");
    assert_eq!(provider.model(), "bigmodel:glm-5.2");
}

#[test]
fn fixed_gemini_provider_validates_specialty_routes_at_startup() {
    let mut config = app_config(ProviderMode::Gemini, "gemini:gemini-2.5-flash");
    config.gemini_api_key = Some("test-gemini-key".to_owned());
    set_configured_route(&mut config, "TRANSLATION_MODEL", "openai:gpt-5.4-mini");

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject cross-provider specialty route"),
        Err(err) => err,
    };

    assert_eq!(err.code, "config");
    assert!(err.message.contains("TRANSLATION_MODEL"));
    assert!(err.message.contains("requires provider `openai`"));
}

#[test]
fn fixed_gemini_provider_accepts_gemini_only_agent_routes() {
    let mut config = app_config(ProviderMode::Gemini, "gemini:gemini-2.5-flash");
    config.gemini_api_key = Some("test-gemini-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "gemini:gemini-2.5-flash",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "gemini:gemini-2.5-flash",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.aux",
        "gemini:gemini-2.5-flash",
    );

    let provider = build_provider(&config).unwrap();

    assert_eq!(provider.name(), "gemini");
    assert_eq!(provider.model(), "gemini:gemini-2.5-flash");
}

#[test]
fn auto_rejects_only_custom_provider_without_configured_key() {
    let mut config = app_config(ProviderMode::Auto, "anthropic:claude");
    config.openai_api_key = None;
    config
        .openai_compatible_providers
        .push(OpenAiCompatibleProviderConfig {
            id: ModelProvider::Custom("anthropic".to_owned()),
            base_url: "https://api.anthropic.example/v1".to_owned(),
            api_key_env: "ANTHROPIC_API_KEY".to_owned(),
            api_key: None,
            auth: HttpAuthConfig::default(),
            request_timeout_seconds: None,
        });

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject routes with no available provider"),
        Err(err) => err,
    };

    assert_eq!(err.code, "config");
    assert!(err.message.contains("no LLM provider is available"));
}
