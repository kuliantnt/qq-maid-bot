use super::*;
use serde_json::json;

fn model_center() -> (ConfigCenter, SqliteDatabase, std::path::PathBuf) {
    let (center, database, directory) = test_center();
    let path = directory.join("agent.toml");
    let text = include_str!("../../../../../runtime/config/agent.example.toml");
    std::fs::write(&path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let running = AgentRuntimeConfig::from_toml(
        text,
        AgentConfigSource::File(path.to_string_lossy().into_owned()),
    )
    .unwrap();
    let center = center
        .with_external_environment(HashMap::from([
            (
                "MODELS_CONFIG_FILE".into(),
                directory.join("models.json").to_string_lossy().into_owned(),
            ),
            (
                "AGENT_CONFIG_FILE".into(),
                path.to_string_lossy().into_owned(),
            ),
            ("OPENAI_API_KEY".into(), "test-key".into()),
        ]))
        .with_running_agent_config(running)
        .unwrap()
        .with_incomplete_setup_writes();
    (center, database, directory)
}

#[test]
fn model_overrides_roundtrip_cas_enrichment_and_security() {
    let (center, _database, directory) = model_center();
    let initial = center.connection_model_metadata("openai").unwrap();
    assert_eq!(initial["revision"], "missing");
    assert!(!initial["models"].as_array().unwrap().is_empty());
    center.update_model_override("openai", "missing", json!({"provider":"openai", "id":"private/model", "display_name":"本地测试", "enabled":false, "context_window":12345})).unwrap();
    let saved = center.connection_model_metadata("openai").unwrap();
    let model = saved["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "private/model")
        .unwrap();
    assert_eq!(model["enabled"], false);
    assert_eq!(model["context_window"], 12345);
    assert_eq!(model["provenance"]["context_window"], "local_override");
    assert_eq!(saved["verified_capabilities"], "unknown");
    assert_eq!(
        center
            .update_model_override("openai", "missing", json!({}))
            .unwrap_err()
            .code(),
        "config_conflict"
    );
    let revision = saved["revision"].as_str().unwrap();
    for entry in [
        json!({"provider":"google", "id":"wrong-identity"}),
        json!({"provider":"openai", "id":"x", "api_key":"not-allowed"}),
        json!({"provider":"openai", "id":"x", "capabilities":{"tool_calling":{"verified":true}}}),
        json!({"provider":"openai", "id":"x,openai:y"}),
    ] {
        assert!(
            center
                .update_model_override("openai", revision, entry)
                .is_err()
        );
    }
    assert_eq!(
        center.connection_model_metadata("openai").unwrap()["revision"],
        revision
    );
    assert!(directory.join("models.json").is_file());
}

#[test]
fn disabled_model_rejects_route_save_runtime_save_and_startup_without_fallback() {
    let (center, _database, directory) = model_center();
    center
        .update_model_override(
            "openai",
            "missing",
            json!({"provider":"openai", "id":"private/disabled", "enabled":false}),
        )
        .unwrap();
    let before = center.current_snapshot().unwrap();
    let change = AgentConfigChange::SetModelRoute {
        name: "unused".into(),
        candidates: vec!["openai:private/disabled,openai:unknown-working".into()],
    };
    let error = center
        .update_agent(&before.agent.as_ref().unwrap().revision, &[change])
        .unwrap_err();
    assert!(error.message().contains("unused.candidates[0]"));
    assert!(error.message().contains("disabled"));
    assert_eq!(
        center.current_snapshot().unwrap().agent.unwrap().revision,
        before.agent.unwrap().revision
    );
    // 手工编辑历史配置，启动与后续保存也必须拒绝；不能只在 Web 禁用按钮做校验。
    let path = center
        .external_environment
        .get("AGENT_CONFIG_FILE")
        .unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    std::fs::write(path, format!("{text}\n[model_routes.unused]\ncandidates = [\"openai:private/disabled\", \"openai:unknown-working\"]\n")).unwrap();
    let error = crate::config::AppConfig::preflight_environment(&center.external_environment, None)
        .unwrap_err();
    assert!(error.message.contains("private/disabled` is disabled"));
    assert!(
        center
            .update_managed(&before.revision, &[])
            .unwrap_err()
            .message()
            .contains("disabled")
    );
    // 清除显式禁用后，目录缺失与能力未知不影响历史路线。
    std::fs::write(directory.join("models.json"), r#"{"models":[]}"#).unwrap();
    crate::config::AppConfig::preflight_environment(&center.external_environment, None).unwrap();
    center.update_managed(&before.revision, &[]).unwrap();
}

#[test]
fn referenced_model_disable_is_atomic_and_search_aliases_are_checked() {
    let (center, _database, _) = model_center();
    let snapshot = center.current_snapshot().unwrap();
    center
        .update_agent(
            &snapshot.agent.unwrap().revision,
            &[AgentConfigChange::SetModelRoute {
                name: "unused".into(),
                candidates: vec!["gemini:local-only".into()],
            }],
        )
        .unwrap();
    let error = center
        .update_model_override(
            "gemini",
            "missing",
            json!({"provider":"google", "id":"local-only", "enabled":false}),
        )
        .unwrap_err();
    assert!(error.message().contains("gemini:local-only` is disabled"));
    assert_eq!(
        center.connection_model_metadata("gemini").unwrap()["revision"],
        "missing"
    );
    let agent = center
        .agent_file
        .as_ref()
        .unwrap()
        .current_runtime()
        .unwrap();
    let search = agent
        .document()
        .unwrap()
        .tools
        .web_search
        .routes
        .values()
        .next()
        .unwrap();
    let id = search
        .model
        .strip_prefix("openai:")
        .unwrap_or(&search.model);
    let error = center
        .update_model_override(
            "openai",
            "missing",
            json!({"provider":"openai", "id":id, "enabled":false}),
        )
        .unwrap_err();
    assert!(error.message().contains("tools.web_search.routes"));
}

#[test]
fn custom_connection_does_not_guess_catalog_and_keeps_local_metadata() {
    let (center, _database, _) = model_center();
    let snapshot = center.current_snapshot().unwrap();
    center
        .update_agent(
            &snapshot.agent.unwrap().revision,
            &[AgentConfigChange::SetProvider {
                id: "google_proxy".into(),
                provider: AgentProviderUpdate {
                    display_name: None,
                    enabled: true,
                    kind: AgentProviderKind::OpenAiCompatible,
                    base_url: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
                    api_key_env: String::new(),
                    auth_header: "Authorization".into(),
                    auth_scheme: Some("Bearer".into()),
                    request_timeout_seconds: None,
                    chat_fallback: None,
                },
            }],
        )
        .unwrap();
    let metadata = center.connection_model_metadata("google_proxy").unwrap();
    assert!(metadata["models"].as_array().unwrap().is_empty());
    assert!(metadata["catalog_source"].is_null());
    center.update_model_override("google_proxy", "missing", json!({"provider":"google_proxy", "id":"gemini-2.5-pro", "display_name":"明确本地名称"})).unwrap();
    let metadata = center.connection_model_metadata("google_proxy").unwrap();
    assert_eq!(metadata["models"][0]["display_name"], "明确本地名称");
    assert!(metadata["models"][0]["context_window"].is_null());
    assert!(
        center
            .connection_model_metadata("missing_connection")
            .is_err()
    );
}
