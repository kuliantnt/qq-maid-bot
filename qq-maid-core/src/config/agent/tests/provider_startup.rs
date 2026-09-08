use super::*;
use qq_maid_llm::provider::{build_provider, preflight_provider_config};

#[test]
fn formal_config_chain_preserves_same_model_across_responses_providers() {
    let directory = TestDirectory::new("same-model-provider-startup");
    let path = directory.0.join("agent.toml");
    std::fs::write(
        &path,
        r#"
version = 1

[providers.router4]
kind = "openai_responses"
base_url = "https://router4.example/v1"
api_key_env = "ROUTER4_API_KEY"

[providers.codexauv]
kind = "openai_responses"
base_url = "https://codexauv.example/v1"
api_key_env = "CODEXAUV_API_KEY"

[model_routes.private_main]
candidates = [
  "router4:gpt-5.6-luna",
  "codexauv:gpt-5.6-luna",
]

[tools.web_search]
backend = "disabled"

[profiles.balanced]
main_route = "private_main"

[scenes.private]
profile = "balanced"

[scenes.group]
profile = "balanced"
"#,
    )
    .unwrap();
    let environment = HashMap::from([
        (
            AGENT_CONFIG_FILE_ENV.to_owned(),
            path.to_string_lossy().into_owned(),
        ),
        ("ROUTER4_API_KEY".to_owned(), "router4-test-key".to_owned()),
        (
            "CODEXAUV_API_KEY".to_owned(),
            "codexauv-test-key".to_owned(),
        ),
    ]);

    // 先显式经过 agent.toml -> AgentRuntimeConfig，锁定 Provider ID 与候选身份。
    let agent = AgentRuntimeConfig::load_from_environment(&environment).unwrap();
    let policy = agent.resolve(ChatScene::Private).unwrap();
    assert_eq!(
        policy.main_model,
        "router4:gpt-5.6-luna,codexauv:gpt-5.6-luna"
    );
    let candidates = policy.main_route.candidates();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].provider.as_ref().unwrap().as_str(), "router4");
    assert_eq!(candidates[0].name, "gpt-5.6-luna");
    assert_eq!(
        candidates[1].provider.as_ref().unwrap().as_str(),
        "codexauv"
    );
    assert_eq!(candidates[1].name, "gpt-5.6-luna");

    // 再走正式 AppConfig / LlmConfig 装配、纯配置预检和 Provider 构建。
    let _guard = crate::config::ValidationEnvironmentGuard::install(environment);
    let app = crate::config::AppConfig::from_env().unwrap();
    let llm = app.llm_config();
    preflight_provider_config(&llm).unwrap();
    let provider = build_provider(&llm).unwrap();

    assert_eq!(
        provider.model(),
        "router4:gpt-5.6-luna,codexauv:gpt-5.6-luna"
    );
    assert_eq!(llm.openai_responses_providers.len(), 2);
    for (id, key_env) in [
        ("router4", "ROUTER4_API_KEY"),
        ("codexauv", "CODEXAUV_API_KEY"),
    ] {
        let configured = llm
            .openai_responses_providers
            .iter()
            .find(|entry| entry.id.as_str() == id)
            .unwrap();
        assert_eq!(configured.api_key_env, key_env);
        assert!(configured.api_key.is_some());
    }
}

#[test]
fn bare_routes_reject_disabled_default_provider_for_every_mode() {
    use qq_maid_llm::config::ProviderMode;
    let agent = AgentRuntimeConfig::from_toml(
        &format!("{DEFAULT_AGENT_CONFIG}\n[model_routes.unused]\ncandidates = [\"test-model\"]\n"),
        AgentConfigSource::File("test.toml".into()),
    )
    .unwrap();
    for (mode, disabled) in [
        (ProviderMode::Auto, "OPENAI"),
        (ProviderMode::OpenAi, "OPENAI"),
        (ProviderMode::DeepSeek, "DEEPSEEK"),
        (ProviderMode::BigModel, "BIGMODEL"),
        (ProviderMode::Gemini, "GEMINI"),
    ] {
        let _guard = crate::config::ValidationEnvironmentGuard::install(HashMap::from([
            (format!("{disabled}_ENABLED"), "false".into()),
            ("DEEPSEEK_API_KEY".into(), "test-key".into()),
        ]));
        let error = crate::config::provider_config::validate_builtin_connections(&agent, &mode)
            .unwrap_err();
        assert!(error.message.contains("unused.candidates[0]"));
        assert!(error.message.contains(&format!(
            "provider `{}` is disabled",
            disabled.to_ascii_lowercase()
        )));
    }
}

#[test]
fn legacy_mixed_case_provider_key_loads_without_rewriting() {
    let directory = TestDirectory::new("legacy-provider-case");
    let path = directory.0.join("agent.toml");
    let text = r#"
version = 1
[providers.MyProxy]
kind = "openai_responses"
base_url = "https://proxy.example/v1"
api_key_env = "TEST_PROXY_KEY"
[model_routes.private_main]
candidates = ["MyProxy:test-model"]
[tools.web_search.routes.search]
model = "MYPROXY:test-model"
[profiles.balanced]
main_route = "private_main"
[scenes.private]
profile = "balanced"
[scenes.group]
profile = "balanced"
"#;
    std::fs::write(&path, text).unwrap();
    let _guard = crate::config::ValidationEnvironmentGuard::install(HashMap::from([
        (
            AGENT_CONFIG_FILE_ENV.into(),
            path.to_string_lossy().into_owned(),
        ),
        ("TEST_PROXY_KEY".into(), "test-key".into()),
    ]));
    let app = crate::config::AppConfig::from_env().unwrap();
    let llm = app.llm_config();
    preflight_provider_config(&llm).unwrap();
    assert_eq!(build_provider(&llm).unwrap().model(), "myproxy:test-model");
    assert!(
        app.agent_config
            .document()
            .unwrap()
            .providers
            .contains_key("MyProxy")
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    let error = AgentRuntimeConfig::from_toml(
        &text.replace("kind =", "enabled = false\nkind ="),
        AgentConfigSource::File("test.toml".into()),
    )
    .unwrap_err();
    assert!(error.message.contains("provider `myproxy` is disabled"));
}

#[test]
fn startup_rejects_bare_openai_route_even_with_deepseek_fallback() {
    let directory = TestDirectory::new("disabled-bare-startup");
    let path = directory.0.join("agent.toml");
    std::fs::write(
        &path,
        format!("{DEFAULT_AGENT_CONFIG}\n[model_routes.unused]\ncandidates = [\"test-model\"]\n"),
    )
    .unwrap();
    let environment = HashMap::from([
        (
            AGENT_CONFIG_FILE_ENV.into(),
            path.to_string_lossy().into_owned(),
        ),
        ("OPENAI_ENABLED".into(), "false".into()),
        ("DEEPSEEK_API_KEY".into(), "test-key".into()),
    ]);
    let error = crate::config::AppConfig::validate_environment(&environment).unwrap_err();
    assert!(
        error
            .message
            .contains("unused.candidates[0]: provider `openai` is disabled")
    );
}

#[test]
fn canonical_provider_alias_cannot_hide_disabled_connection() {
    let text = format!(
        r#"{DEFAULT_AGENT_CONFIG}
[providers.MyProxy]
enabled = false
kind = "openai_responses"
base_url = "https://proxy.example/v1"
api_key_env = "TEST_KEY"
[providers.myproxy]
kind = "openai_responses"
base_url = "https://proxy.example/v1"
api_key_env = "TEST_KEY"
[model_routes.unused]
candidates = ["myproxy:test-model"]
"#
    );
    let error = AgentRuntimeConfig::from_toml(&text, AgentConfigSource::File("test.toml".into()))
        .unwrap_err();
    assert!(error.message.contains("duplicate provider `myproxy`"));
}
