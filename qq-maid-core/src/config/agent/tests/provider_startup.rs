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
