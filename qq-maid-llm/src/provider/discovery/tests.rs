use super::*;
use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    routing::get,
};

#[test]
fn list_validation_does_not_invent_models_or_capabilities() {
    for body in [
        r#"{}"#,
        r#"{"data":[{}]}"#,
        r#"{"data":[{"id":" "}]}"#,
        r#"{"data":[{"id":"a\nb"}]}"#,
        r#"{"data":[],"has_more":true}"#,
    ] {
        assert!(parse_models(body.as_bytes()).is_none(), "{body}");
    }
    assert!(parse_models(br#"{"data":[]}"#).unwrap().is_empty());
    let models = parse_models(
        br#"{"data":[{"id":"vendor/private-model","tool_calling":true},{"id":"a"},{"id":"a"}]}"#,
    )
    .unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "a");
    assert_eq!(models[1].source, "connection_discovery");
    assert!(
        !serde_json::to_string(&models)
            .unwrap()
            .contains("tool_calling")
    );
}

// 单个异步用例串行验证全局并发限制，避免其他测试竞争静态信号量。
#[tokio::test]
async fn bounded_discovery_uses_connection_auth_and_classifies_failures() {
    let app = Router::new()
        .route(
            "/proxy/models",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["x-test-key"], "test-only-key");
                assert!(!headers.contains_key("authorization"));
                r#"{"data":[{"id":"private/model"}],"ignored":"test-only-sensitive-body"}"#
            }),
        )
        .route("/empty/models", get(|| async { r#"{"data":[]}"# }))
        .route(
            "/invalid/models",
            get(|| async { "test-only-sensitive-body" }),
        )
        .route(
            "/auth/models",
            get(|| async { (StatusCode::UNAUTHORIZED, "test-only-sensitive-body") }),
        )
        .route(
            "/rate/models",
            get(|| async { StatusCode::TOO_MANY_REQUESTS }),
        )
        .route(
            "/upstream/models",
            get(|| async { StatusCode::BAD_GATEWAY }),
        )
        .route("/large/models", get(|| async { "x".repeat(MAX_BYTES + 1) }))
        .route(
            "/chunked/models",
            get(|| async {
                // 无 Content-Length 时仍须逐块限制，不能只信任响应头。
                axum::body::Body::from_stream(futures::stream::iter([
                    Ok::<_, std::convert::Infallible>(vec![b'x'; MAX_BYTES / 2 + 1]),
                    Ok(vec![b'x'; MAX_BYTES / 2 + 1]),
                ]))
            }),
        )
        .route(
            "/redirect/models",
            get(|| async {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", "http://127.0.0.1:1")],
                )
            }),
        )
        .route(
            "/slow/models",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                r#"{"data":[]}"#
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let auth = HttpAuthConfig {
        header: "x-test-key".into(),
        scheme: None,
    };
    for adapter in [
        DiscoveryAdapter::OpenAiCompatible,
        DiscoveryAdapter::OpenAiResponses,
    ] {
        for (path, state, category) in [
            ("proxy/", DiscoveryState::Success, "model_list"),
            ("empty", DiscoveryState::Success, "model_list"),
            (
                "missing",
                DiscoveryState::Unsupported,
                "endpoint_unsupported",
            ),
            ("invalid", DiscoveryState::Unknown, "unrecognized_response"),
            ("auth", DiscoveryState::Failed, "authentication"),
            ("rate", DiscoveryState::Failed, "rate_limit"),
            ("upstream", DiscoveryState::Failed, "upstream"),
            ("large", DiscoveryState::Failed, "response_too_large"),
            ("chunked", DiscoveryState::Failed, "response_too_large"),
            ("redirect", DiscoveryState::Failed, "redirect_rejected"),
            ("slow", DiscoveryState::Failed, "timeout"),
        ] {
            let timeout = if path == "slow" {
                Duration::from_millis(30)
            } else {
                Duration::from_secs(5)
            };
            let result = discover_models(
                adapter,
                &format!("http://{address}/{path}"),
                &auth,
                "test-only-key",
                timeout,
            )
            .await
            .unwrap();
            assert_eq!(result.state, state, "{path}: {result:?}");
            assert_eq!(result.category, category, "{path}");
            assert_eq!(result.models.is_some(), state == DiscoveryState::Success);
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains("test-only")
            );
        }
    }
    let permits = DISCOVERIES.acquire_many(2).await.unwrap();
    let result = discover_models(
        DiscoveryAdapter::OpenAiCompatible,
        "http://127.0.0.1:1",
        &auth,
        "test-only-key",
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(result.category, "busy");
    assert_eq!(result.state, DiscoveryState::Unknown);
    drop(permits);
    let unsupported = discover_models(
        DiscoveryAdapter::Unsupported,
        "invalid",
        &auth,
        "",
        Duration::ZERO,
    )
    .await
    .unwrap();
    assert_eq!(unsupported.state, DiscoveryState::Unsupported);
    assert!(unsupported.http_status.is_none());
    assert!(
        discover_models(
            DiscoveryAdapter::OpenAiCompatible,
            "https://example.invalid/?key=secret",
            &auth,
            "",
            Duration::from_secs(1)
        )
        .await
        .is_err()
    );
    server.abort();
}
