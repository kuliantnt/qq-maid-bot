//! 配置中心 HTTP 认证、CAS 与无敏感明文回归。
use super::*;

#[tokio::test]
async fn configuration_snapshot_never_returns_secret_plaintext() -> Result<(), Infallible> {
    let mut state = test_state();
    state.config.web_console_enabled = true;
    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-config-http", APP_MIGRATIONS).unwrap();
    // 本地上游验证完整管理员 API 调用链，不使用真实供应商或凭证。
    let upstream = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|| async {
            axum::Json(json!({"data": [{"id": "private/proxy-model"}]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let discovery_base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let external = HashMap::from([
        (
            "MODELS_CONFIG_FILE".to_owned(),
            directory
                .join("config/models.json")
                .to_string_lossy()
                .into_owned(),
        ),
        ("OPENAI_BASE_URLS".to_owned(), discovery_base),
        (
            "OPENAI_API_KEY".to_owned(),
            "must-never-reach-response".to_owned(),
        ),
        (
            "TAVILY_API_KEY".to_owned(),
            "tavily-must-never-reach-response".to_owned(),
        ),
    ]);
    let agent_path = directory.join("config/agent.toml");
    std::fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
    std::fs::write(
        &agent_path,
        include_str!("../../../../../runtime/config/agent.example.toml"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // 该用例会真实保存 agent.toml，夹具必须满足配置中心的安全权限约束，
        // 不能让宿主机 umask 决定测试结果。
        std::fs::set_permissions(
            agent_path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(&agent_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let agent_environment = HashMap::from([(
        crate::config::agent::AGENT_CONFIG_FILE_ENV.to_owned(),
        agent_path.to_string_lossy().into_owned(),
    )]);
    let running_agent = AgentRuntimeConfig::load_from_environment(&agent_environment).unwrap();
    state.registered_tools = Arc::new(vec![ConsoleToolMetadata {
        name: "web_search".to_owned(),
        description: "受控搜索".to_owned(),
    }]);
    state.config_center = Some(
        ConfigCenter::open(
            managed_config_fields(),
            ConfigCenterPaths {
                managed_config_file: directory.join("config/runtime.toml"),
                master_key_file: directory.join("config/secrets/master.key"),
            },
            database.clone(),
        )
        .unwrap()
        .with_external_environment(external)
        .with_running_agent_config(running_agent)
        .unwrap(),
    );
    let (auth, cookie, csrf) = initialize_test_admin(database, &directory);
    state.admin_auth = Some(auth);
    let (unauthenticated_status, _) =
        request_response(state.clone(), "GET", "/api/v1/console/configuration", None).await;
    assert_eq!(unauthenticated_status, StatusCode::UNAUTHORIZED);

    for (method, path, body) in [
        (
            "POST",
            "/api/v1/console/configuration/providers/model-metadata",
            json!({"id":"openai"}),
        ),
        (
            "PATCH",
            "/api/v1/console/configuration/providers/model-override",
            json!({"id":"openai", "expected_revision":"missing", "model":{"provider":"openai", "id":"private/test"}}),
        ),
        (
            "PATCH",
            "/api/v1/console/configuration/providers/credential",
            json!({
                "id": "test", "expected_agent_revision": "missing", "expected_revision": "missing", "value": "test-only-key"
            }),
        ),
        (
            "POST",
            "/api/v1/console/configuration/providers/test",
            json!({
                "id": "test", "expected_revision": "missing", "model": "test-model"
            }),
        ),
        (
            "POST",
            "/api/v1/console/configuration/providers/models",
            json!({"id": "test", "expected_revision": "missing"}),
        ),
    ] {
        let (status, _) = request_response(state.clone(), method, path, Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) =
            request_response_with_cookie(state.clone(), method, path, Some(body), &cookie, None)
                .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    let (status, json) = request_response_with_cookie(
        state.clone(),
        "GET",
        "/api/v1/console/configuration",
        None,
        &cookie,
        None,
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(json["ok"], true);
    let (discovery_status, discovery) = request_response_with_cookie(
        state.clone(),
        "POST",
        "/api/v1/console/configuration/providers/models",
        Some(json!({"id": "openai", "expected_revision": json["configuration"]["revision"]})),
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(discovery_status, StatusCode::OK);
    assert_eq!(discovery["discovery"]["state"], "success");
    assert_eq!(
        discovery["discovery"]["models"][0]["id"],
        "private/proxy-model"
    );
    assert_eq!(
        discovery["discovery"]["models"][0]["source"],
        "connection_discovery"
    );
    assert!(!discovery.to_string().contains("must-never-reach-response"));
    let (stale_status, _) = request_response_with_cookie(
        state.clone(),
        "POST",
        "/api/v1/console/configuration/providers/models",
        Some(json!({"id": "openai", "expected_revision": "stale"})),
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(stale_status, StatusCode::CONFLICT);
    let (status, metadata) = request_response_with_cookie(
        state.clone(),
        "POST",
        "/api/v1/console/configuration/providers/model-metadata",
        Some(json!({"id":"openai"})),
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(metadata["metadata"]["verified_capabilities"], "unknown");
    assert!(!metadata.to_string().contains("must-never-reach-response"));
    let mutation = json!({"id":"openai", "expected_revision":"missing", "model":{"provider":"openai", "id":"private/new", "enabled":false}});
    let (status, _) = request_response_with_cookie(
        state.clone(),
        "PATCH",
        "/api/v1/console/configuration/providers/model-override",
        Some(mutation.clone()),
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request_response_with_cookie(
        state.clone(),
        "PATCH",
        "/api/v1/console/configuration/providers/model-override",
        Some(mutation),
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    server.abort();
    let serialized = json.to_string();
    assert!(!serialized.contains("must-never-reach-response"));
    assert!(!serialized.contains("tavily-must-never-reach-response"));
    let secret = json["configuration"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["key"] == "provider.openai.api_key")
        .unwrap();
    assert_eq!(secret["configured"], true);
    assert_eq!(secret["source"], "environment");
    assert!(secret["effective_value"].is_null());
    let tavily_secret = json["configuration"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["key"] == "tools.web_search.tavily.api_key")
        .unwrap();
    assert_eq!(tavily_secret["configured"], true);
    assert!(tavily_secret["effective_value"].is_null());
    assert_eq!(json["configuration"]["agent"]["source"], "agent_toml");
    assert_eq!(json["configuration"]["agent"]["pending_restart"], false);
    assert_eq!(json["registered_tools"][0]["name"], "web_search");
    assert_eq!(json["registered_tools"][0]["description"], "受控搜索");
    assert_eq!(json["restart"]["available"], false);

    let agent_revision = json["configuration"]["agent"]["revision"].as_str().unwrap();
    let agent_mutation = json!({
        "expected_revision": agent_revision,
        "changes": [{
            "action": "set_web_search",
            "backend": "disabled",
            "max_results": 8,
            "search_depth": "advanced",
            "topic": "news",
            "time_range": "week",
            "connect_timeout_seconds": 5,
            "first_response_timeout_seconds": 15,
            "total_timeout_seconds": 45
        }]
    });
    let (agent_saved, agent_saved_json) = request_response_with_cookie(
        state.clone(),
        "PATCH",
        "/api/v1/console/configuration/agent",
        Some(agent_mutation),
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(agent_saved, StatusCode::OK);
    assert_eq!(agent_saved_json["persisted"], true);
    assert_eq!(
        agent_saved_json["configuration"]["agent"]["saved_value"]["tools"]["web_search"]["backend"],
        "disabled"
    );
    assert!(
        agent_saved_json["configuration"]["agent"]["saved_value"]["tools"]["web_search"]["routes"]
            ["private_search"]
            .is_object()
    );
    assert!(
        !agent_saved_json
            .to_string()
            .contains("tavily-must-never-reach-response")
    );

    let revision = json["configuration"]["revision"].as_str().unwrap();
    let mutation = json!({
        "expected_revision": revision,
        "changes": [{
            "action": "set",
            "key": "features.rss.enabled",
            "value": false
        }]
    });
    let (missing_csrf, _) = request_response_with_cookie(
        state.clone(),
        "PATCH",
        "/api/v1/console/configuration/runtime",
        Some(mutation.clone()),
        &cookie,
        None,
    )
    .await;
    assert_eq!(missing_csrf, StatusCode::FORBIDDEN);
    let (restart_missing_csrf, _) = request_response_with_cookie(
        state.clone(),
        "POST",
        "/api/v1/console/restart",
        Some(json!({})),
        &cookie,
        None,
    )
    .await;
    assert_eq!(restart_missing_csrf, StatusCode::FORBIDDEN);
    let (restart_unavailable, restart_json) = request_response_with_cookie(
        state.clone(),
        "POST",
        "/api/v1/console/restart",
        Some(json!({})),
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(restart_unavailable, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(restart_json["error"]["code"], "restart_unavailable");
    let (saved, saved_json) = request_response_with_cookie(
        state.clone(),
        "PATCH",
        "/api/v1/console/configuration/runtime",
        Some(mutation),
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(saved, StatusCode::OK);
    assert_eq!(saved_json["persisted"], true);

    // 两个标签页顺序刷新会话后都保留同一稳定 CSRF，可继续完成写请求。
    let tab_two_csrf = state
        .admin_auth
        .as_ref()
        .unwrap()
        .refresh_admin_session(&cookie)
        .unwrap()
        .csrf_token;
    let second_revision = saved_json["configuration"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_mutation = json!({
        "expected_revision": second_revision,
        "changes": [{
            "action": "set",
            "key": "features.rss.enabled",
            "value": true
        }]
    });
    let (saved_again, saved_again_json) = request_response_with_cookie(
        state,
        "PATCH",
        "/api/v1/console/configuration/runtime",
        Some(second_mutation),
        &cookie,
        Some(&tab_two_csrf),
    )
    .await;
    assert_eq!(saved_again, StatusCode::OK);
    assert_eq!(saved_again_json["persisted"], true);
    Ok(())
}
