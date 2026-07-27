use super::*;

#[test]
fn toml_config_accepts_configured_openai_responses_provider() {
    let text = format!(
        "{DEFAULT_AGENT_CONFIG}\n\
[providers.opencode_zen]\n\
kind = \"openai_responses\"\n\
base_url = \"https://opencode.ai/zen/v1\"\n\
api_key_env = \"OPENCODE_API_KEY\"\n\
auth_header = \"Authorization\"\n\
auth_scheme = \"Bearer\"\n\
request_timeout_seconds = 45\n\
chat_fallback = false\n"
    );

    let config = AgentRuntimeConfig::from_toml(
        &text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap();
    let provider = config
        .provider_configs()
        .into_iter()
        .find(|provider| provider.id.as_str() == "opencode_zen")
        .unwrap();

    assert_eq!(provider.kind, AgentProviderKind::OpenAiResponses);
    assert_eq!(provider.base_url, "https://opencode.ai/zen/v1");
    assert_eq!(provider.api_key_env, "OPENCODE_API_KEY");
    assert_eq!(provider.request_timeout_seconds, Some(45));
    assert!(!provider.chat_fallback);
}

#[test]
fn chat_provider_rejects_responses_only_field() {
    let text = format!(
        "{DEFAULT_AGENT_CONFIG}\n\
[providers.opencode_go]\n\
kind = \"openai_compatible\"\n\
base_url = \"https://opencode.ai/zen/go/v1\"\n\
api_key_env = \"OPENCODE_API_KEY\"\n\
chat_fallback = false\n"
    );

    let error = AgentRuntimeConfig::from_toml(
        &text,
        AgentConfigSource::File("config/agent.toml".to_owned()),
    )
    .unwrap_err();

    assert!(error.message.contains("chat_fallback"));
    assert!(error.message.contains("openai_responses"));
}

#[test]
fn provider_connection_metadata_is_strictly_validated() {
    for (id, base_url, api_key_env, expected) in [
        ("openai", "https://example.com/v1", "KEY", "built-in"),
        ("9bad", "https://example.com/v1", "KEY", "invalid providers"),
        ("custom", "not-a-url", "KEY", "valid URL"),
        (
            "custom",
            "https://example.com/v1?token=unsafe",
            "KEY",
            "without credentials, query, or fragment",
        ),
        (
            "custom",
            "https://example.com/v1",
            "BAD-KEY",
            "environment variable name",
        ),
    ] {
        let text = format!(
            "{DEFAULT_AGENT_CONFIG}\n\
[providers.{id}]\n\
kind = \"openai_responses\"\n\
base_url = \"{base_url}\"\n\
api_key_env = \"{api_key_env}\"\n"
        );
        let error = AgentRuntimeConfig::from_toml(
            &text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
        )
        .unwrap_err();
        assert!(error.message.contains(expected), "{}", error.message);
    }
}

#[test]
fn provider_auth_scheme_rejects_non_token_or_control_characters() {
    for auth_scheme in ["Bearer bad", "Bearer\nInjected"] {
        let text = format!(
            "{DEFAULT_AGENT_CONFIG}\n\
[providers.custom]\n\
kind = \"openai_responses\"\n\
base_url = \"https://example.com/v1\"\n\
api_key_env = \"CUSTOM_API_KEY\"\n\
auth_scheme = {auth_scheme:?}\n"
        );
        let error = AgentRuntimeConfig::from_toml(
            &text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
        )
        .unwrap_err();
        assert!(error.message.contains("auth_scheme"), "{}", error.message);
    }
}
