use super::*;

#[test]
fn toml_config_overrides_routes_profiles_and_scenes() {
    let text = r#"
version = 1

[model_routes.main]
candidates = ["openai:gpt-main", "deepseek:deepseek-chat"]

[model_routes.fast]
candidates = ["openai:gpt-fast"]

[model_routes.aux]
candidates = ["openai:gpt-aux"]

[search_routes.search]
model = "gpt-search"

[profiles.fast]
main_route = "fast"
aux_route = "aux"
reasoning_effort = "low"
max_tool_rounds = 2
max_output_tokens = 800

[profiles.balanced]
main_route = "main"
aux_route = "aux"
reasoning_effort = "medium"
max_tool_rounds = 6
max_output_tokens = 1600

[profiles.deep]
main_route = "main"
aux_route = "aux"
reasoning_effort = "high"
max_tool_rounds = 8
max_output_tokens = 3200

[scenes.private]
enabled = true
profile = "deep"
tool_calling_enabled = true
enabled_tools = ["get_weather"]

[scenes.group]
enabled = true
profile = "fast"
tool_calling_enabled = false
enabled_tools = ["get_weather", "list_todos", "get_weather"]
"#;

    let config = AgentRuntimeConfig::from_toml(
        text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap();

    let private = config.resolve(ChatScene::Private).unwrap();
    let group = config.resolve(ChatScene::Group).unwrap();
    assert_eq!(private.profile, "deep");
    assert_eq!(private.main_model, "openai:gpt-main,deepseek:deepseek-chat");
    assert_eq!(private.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(private.max_tool_rounds, 8);
    assert_eq!(private.max_output_tokens, Some(3200));
    assert_eq!(group.profile, "fast");
    assert_eq!(group.main_model, "openai:gpt-fast");
    assert!(!group.group_tool_calling_enabled);
    assert_eq!(group.enabled_tools, vec!["get_weather", "list_todos"]);
}

#[test]
fn toml_config_accepts_openai_compatible_mimo_provider() {
    let text = r#"
version = 1

[providers.mimo]
kind = "openai_compatible"
base_url = "https://api.xiaomimimo.com/v1"
api_key_env = "MIMO_API_KEY"
auth_header = "Authorization"
auth_scheme = "Bearer"
request_timeout_seconds = 45

[model_routes.private_main]
candidates = ["mimo:mimo-v2.5-pro", "deepseek:deepseek-chat"]

[search_routes.search]
model = "gpt-search"

[profiles.fast]
main_route = "private_main"
max_tool_rounds = 2

[profiles.balanced]
main_route = "private_main"
max_tool_rounds = 5

[profiles.deep]
main_route = "private_main"
max_tool_rounds = 8

[scenes.private]
enabled = true
profile = "balanced"
tool_calling_enabled = true
enabled_tools = ["get_weather"]

[scenes.group]
enabled = true
profile = "fast"
tool_calling_enabled = false
enabled_tools = ["save_memory"]
"#;

    let config = AgentRuntimeConfig::from_toml(
        text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap();

    let providers = config.provider_configs();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, ModelProvider::Custom("mimo".to_owned()));
    assert_eq!(providers[0].api_key_env, "MIMO_API_KEY");
    assert_eq!(providers[0].request_timeout_seconds, Some(45));
    let private = config.resolve(ChatScene::Private).unwrap();
    assert_eq!(
        private.main_model,
        "mimo:mimo-v2.5-pro,deepseek:deepseek-chat"
    );
    assert_eq!(private.aux_model, None);
    assert_eq!(
        private.resolve_auxiliary_model(None).as_deref(),
        Some("mimo:mimo-v2.5-pro,deepseek:deepseek-chat")
    );
    assert_eq!(
        private
            .resolve_auxiliary_model(Some("openai:explicit-aux"))
            .as_deref(),
        Some("openai:explicit-aux")
    );
}

#[test]
fn toml_config_accepts_gemini_search_route() {
    let text = r#"
version = 1

[model_routes.private_main]
candidates = ["openai:gpt-private"]

[model_routes.group_main]
candidates = ["openai:gpt-group"]

[search_routes.search]
model = "gpt-search"

[search_routes.private_search]
model = "gemini:gemini-2.5-flash"

[profiles.fast]
main_route = "group_main"
max_tool_rounds = 2

[profiles.balanced]
main_route = "private_main"
max_tool_rounds = 5

[profiles.deep]
main_route = "private_main"
max_tool_rounds = 8

[scenes.private]
enabled = true
profile = "balanced"
search_route = "private_search"
tool_calling_enabled = true
enabled_tools = ["get_weather"]

[scenes.group]
enabled = true
profile = "fast"
tool_calling_enabled = false
enabled_tools = ["save_memory"]
"#;

    let config = AgentRuntimeConfig::from_toml(
        text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap();

    let private = config.resolve(ChatScene::Private).unwrap();
    assert_eq!(private.search_model, "gemini:gemini-2.5-flash");
}

#[test]
fn toml_config_rejects_removed_provider_retry_fields() {
    let text = r#"
version = 1

[providers.mimo]
kind = "openai_compatible"
base_url = "https://api.xiaomimimo.com/v1"
api_key_env = "MIMO_API_KEY"
max_retries = 3

[profiles.fast]
main_route = "group_main"
max_tool_rounds = 2

[profiles.balanced]
main_route = "private_main"
max_tool_rounds = 5

[profiles.deep]
main_route = "private_main"
max_tool_rounds = 8

[scenes.private]
enabled = true
profile = "balanced"

[scenes.group]
enabled = true
profile = "fast"
"#;

    let err = AgentRuntimeConfig::from_toml(
        text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap_err();

    assert_eq!(err.stage, "config");
    assert!(err.message.contains("unknown field `max_retries`"));
}

#[test]
fn default_agent_toml_prefers_luna_and_keeps_provider_fallbacks() {
    let text = include_str!("../../../../runtime/config/agent.toml");
    let config = AgentRuntimeConfig::from_toml(
        text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap();

    let private = config.resolve(ChatScene::Private).unwrap();
    let group = config.resolve(ChatScene::Group).unwrap();
    assert_eq!(
        private.main_model,
        "openai:gpt-5.6-luna,gemini:gemini-2.5-pro,mimo:mimo-v2.5-pro,deepseek:deepseek-chat"
    );
    assert_eq!(
        private.aux_model.as_deref(),
        Some("openai:gpt-5.6-luna,gemini:gemini-2.5-flash,mimo:mimo-v2.5,deepseek:deepseek-chat")
    );
    assert_eq!(private.search_model, "gpt-5.6-luna");
    assert_eq!(
        group.main_model,
        "openai:gpt-5.6-luna,gemini:gemini-2.5-flash,mimo:mimo-v2.5,deepseek:deepseek-chat"
    );
    assert_eq!(
        group.aux_model.as_deref(),
        Some("openai:gpt-5.6-luna,gemini:gemini-2.5-flash,mimo:mimo-v2.5,deepseek:deepseek-chat")
    );
    assert_eq!(group.search_model, "gpt-5.6-luna");
    assert_eq!(
        config.source,
        AgentConfigSource::File("config/agent.toml".to_owned())
    );
}

#[test]
fn default_agent_toml_declares_luna_first_without_embedding_secrets() {
    let text = include_str!("../../../../runtime/config/agent.toml");
    let active_config = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(active_config.contains("[model_routes.private_main]"));
    assert!(active_config.contains("[model_routes.group_main]"));
    assert!(active_config.contains("[model_routes.aux]"));
    assert!(active_config.contains("openai:gpt-5.6-luna"));
    assert!(active_config.contains("gemini:gemini-2.5-pro"));
    assert!(active_config.contains("mimo:mimo-v2.5-pro"));
    assert!(active_config.contains("deepseek:deepseek-chat"));
    assert!(active_config.contains("[search_routes.private_search]"));
    assert!(active_config.contains("[search_routes.group_search]"));
    assert!(!active_config.contains("bigmodel:"));
    assert!(!active_config.contains("glm-"));
    assert!(!active_config.contains("sk-"));
}

#[test]
fn default_agent_toml_preserves_private_and_group_scene_routes() {
    let text = include_str!("../../../../runtime/config/agent.toml");
    let config = AgentRuntimeConfig::from_toml(
        text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap();

    let private = config.resolve(ChatScene::Private).unwrap();
    let group = config.resolve(ChatScene::Group).unwrap();
    assert_eq!(private.profile, "balanced");
    assert!(private.main_model.starts_with("openai:gpt-5.6-luna,"));
    assert_eq!(private.reasoning_effort, Some(ReasoningEffort::Medium));
    assert_eq!(private.max_tool_rounds, 5);
    assert!(private.tool_calling_enabled);

    assert_eq!(group.profile, "fast");
    assert!(group.main_model.starts_with("openai:gpt-5.6-luna,"));
    assert_eq!(group.reasoning_effort, Some(ReasoningEffort::Low));
    assert_eq!(group.max_tool_rounds, 2);
    assert_eq!(
        group.enabled_tools,
        vec![
            "get_weather",
            "get_train_schedule",
            "get_rss_recent_items",
            "manage_rss_subscriptions",
            "web_search",
            "knowledge_search",
            "save_memory"
        ]
    );
    assert!(!group.group_tool_calling_enabled);
}

#[test]
fn default_agent_toml_exposes_expected_luna_first_route_displays() {
    let text = include_str!("../../../../runtime/config/agent.toml");
    let config = AgentRuntimeConfig::from_toml(
        text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap();

    let route_displays = config
        .configured_model_routes()
        .into_iter()
        .map(|(name, route)| (name, route.display()))
        .collect::<HashMap<_, _>>();

    assert_eq!(
        route_displays.get("agent.model_routes.private_main"),
        Some(
            &"openai:gpt-5.6-luna,gemini:gemini-2.5-pro,mimo:mimo-v2.5-pro,deepseek:deepseek-chat"
                .to_owned()
        )
    );
    assert_eq!(
        route_displays.get("agent.model_routes.group_main"),
        Some(
            &"openai:gpt-5.6-luna,gemini:gemini-2.5-flash,mimo:mimo-v2.5,deepseek:deepseek-chat"
                .to_owned()
        )
    );
    assert_eq!(
        route_displays.get("agent.model_routes.aux"),
        Some(
            &"openai:gpt-5.6-luna,gemini:gemini-2.5-flash,mimo:mimo-v2.5,deepseek:deepseek-chat"
                .to_owned()
        )
    );
}

#[test]
fn toml_config_rejects_unknown_profile() {
    let text = include_str!("../../../../runtime/config/agent.toml").replacen(
        "profile = \"balanced\"",
        "profile = \"missing\"",
        1,
    );

    let err = AgentRuntimeConfig::from_toml(
        &text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("unknown profile"));
}

#[test]
fn toml_config_rejects_unknown_search_route() {
    let text = include_str!("../../../../runtime/config/agent.toml").replacen(
        "search_route = \"private_search\"",
        "search_route = \"missing\"",
        1,
    );

    let err = AgentRuntimeConfig::from_toml(
        &text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("unknown search route"));
}

#[test]
fn toml_config_rejects_unknown_enabled_tool() {
    let text = include_str!("../../../../runtime/config/agent.toml").replacen(
        "enabled_tools = [\"get_weather\", \"get_train_schedule\"",
        "enabled_tools = [\"run_shell\", \"get_train_schedule\"",
        1,
    );

    let err = AgentRuntimeConfig::from_toml(
        &text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap_err();

    assert_eq!(err.code, "config");
    assert!(err.message.contains("unknown tool `run_shell`"));
}
