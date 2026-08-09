//! Memory 管理 API 集成测试。

use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use qq_maid_llm::provider::{
    ChatOutcome, LlmProvider,
    status::{UpstreamStatus, observe_provider},
    types::{ChatRequest, TokenUsage},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::{
    error::LlmError,
    http::routes::{OpsHttpConfig, OpsHttpState, build_router},
    identity::conversation_scope_key,
    management::{AdminAuth, SESSION_COOKIE_NAME},
    runtime::tools::memory::{
        MemoryCategory, MemoryKind, MemoryManagementService, MemorySourceType, MemoryStore,
        MemoryTarget, MemoryVisibility, storage::PersistMemoryRequest,
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
    util::metrics::LlmMetrics,
};

#[derive(Clone)]
struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        Ok(ChatOutcome {
            reply: String::new(),
            output_parts: Vec::new(),
            metrics: LlmMetrics {
                provider: "mock".to_owned(),
                model: "mock".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 0,
            },
            usage: Some(TokenUsage {
                input_tokens: None,
                cached_input_tokens: None,
                output_tokens: None,
                total_tokens: None,
            }),
            fallback_used: false,
            agent: Default::default(),
        })
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock"
    }

    fn stream_enabled(&self) -> bool {
        false
    }
}

struct TestApi {
    state: OpsHttpState,
    cookie: String,
    csrf: String,
    other_cookie: String,
    other_csrf: String,
    target_ref: String,
    database: SqliteDatabase,
    memory_store: MemoryStore,
}

impl TestApi {
    fn new() -> Self {
        let (database, directory) =
            SqliteDatabase::open_temp_directory("memory-management-api", APP_MIGRATIONS).unwrap();
        let token_file = directory.join("config/secrets/bootstrap.token");
        let auth = AdminAuth::open_silent(database.clone(), token_file.clone()).unwrap();
        let token = std::fs::read_to_string(token_file)
            .unwrap()
            .trim()
            .splitn(3, ':')
            .nth(2)
            .unwrap()
            .to_owned();
        let preauth = auth.issue_preauth().unwrap();
        let issued = auth
            .initialize(
                &preauth.cookie_value,
                &preauth.session.csrf_token,
                &token,
                "admin",
                "correct horse battery staple",
            )
            .unwrap();
        let database_connection = database.connection().unwrap();
        database_connection
            .execute(
                "INSERT INTO console_admins (username, password_hash, disabled, created_at)
                 SELECT 'admin2', password_hash, 0, created_at
                 FROM console_admins WHERE username = 'admin'",
                [],
            )
            .unwrap();
        drop(database_connection);
        let other_preauth = auth.issue_preauth().unwrap();
        let other_issued = auth
            .login(
                &other_preauth.cookie_value,
                &other_preauth.session.csrf_token,
                "admin2",
                "correct horse battery staple",
            )
            .unwrap();

        let database_for_test = database.clone();
        let memory_store = MemoryStore::new(database);
        let target = MemoryTarget::personal(personal_scope("user-default"));
        seed(
            &memory_store,
            target,
            "初始 % 内容",
            "raw-source-must-not-leak",
        );
        let memory_service = MemoryManagementService::new(memory_store.clone());
        let target_ref = memory_service
            .targets(Default::default(), 20, 0)
            .unwrap()
            .items
            .into_iter()
            .next()
            .unwrap()
            .target_ref;

        let upstream = UpstreamStatus::default();
        let mut state = OpsHttpState::from_parts(
            OpsHttpConfig {
                web_console_enabled: true,
                web_console_allowed_origins: Vec::new(),
                web_console_trusted_proxy_ips: Vec::new(),
                web_console_secure_cookies: false,
                knowledge_max_file_bytes: crate::config::DEFAULT_KNOWLEDGE_MAX_FILE_BYTES,
            },
            observe_provider(Arc::new(MockProvider), upstream.clone()),
            upstream,
        );
        state.admin_auth = Some(auth);
        state = state.with_memory_management(memory_service);
        Self {
            state,
            cookie: issued.cookie_value,
            csrf: issued.session.csrf_token,
            other_cookie: other_issued.cookie_value,
            other_csrf: other_issued.session.csrf_token,
            target_ref,
            database: database_for_test,
            memory_store,
        }
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.request_with_credentials(path, Some(body), Some((&self.cookie, &self.csrf)), None)
            .await
    }

    async fn post_as_other(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.request_with_credentials(
            path,
            Some(body),
            Some((&self.other_cookie, &self.other_csrf)),
            None,
        )
        .await
    }

    async fn request_with_credentials(
        &self,
        path: &str,
        body: Option<Value>,
        credentials: Option<(&str, &str)>,
        origin: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("host", "localhost")
            .header("x-request-id", "memory-api-test");
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        if let Some((cookie, csrf)) = credentials {
            builder = builder
                .header("cookie", format!("{SESSION_COOKIE_NAME}={cookie}"))
                .header("x-csrf-token", csrf);
        }
        let body = body
            .map(|value| Body::from(value.to_string()))
            .unwrap_or_else(Body::empty);
        let response = build_router(self.state.clone())
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        if status != StatusCode::NOT_FOUND {
            assert_eq!(
                response
                    .headers()
                    .get("x-request-id")
                    .and_then(|value| value.to_str().ok()),
                Some("memory-api-test")
            );
        }
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
        (status, json)
    }

    fn seed_target(&self, target: MemoryTarget, content: &str) -> String {
        seed(&self.memory_store, target, content, "seed-source");
        self.state
            .memory_management
            .as_ref()
            .unwrap()
            .targets(Default::default(), 100, 0)
            .unwrap()
            .items
            .into_iter()
            .find(|item| item.scope == "group_profile")
            .unwrap()
            .target_ref
    }

    fn set_audit_failure(&self, enabled: bool) {
        self.state
            .admin_auth
            .as_ref()
            .unwrap()
            .set_management_audit_failure_for_tests(enabled);
    }
}

fn personal_scope(user: &str) -> String {
    conversation_scope_key("qq_official", Some("app-default"), "private", user)
}

fn group_scope(group: &str) -> String {
    conversation_scope_key("qq_official", Some("app-default"), "group", group)
}

fn seed(store: &MemoryStore, target: MemoryTarget, content: &str, source_text: &str) {
    let visibility = match target.memory_kind() {
        MemoryKind::Personal => MemoryVisibility::Private,
        MemoryKind::GroupProfile | MemoryKind::Group => MemoryVisibility::GroupMembers,
        MemoryKind::LegacyUnassigned => MemoryVisibility::Private,
    };
    store
        .persist_v3(PersistMemoryRequest {
            target,
            created_by_user_id: None,
            content: content.to_owned(),
            source_text: source_text.to_owned(),
            category: MemoryCategory::Note,
            legacy_scope: "general".to_owned(),
            visibility,
            source_type: MemorySourceType::ManualImport,
            source_ref: None,
            confirmed_at: None,
            pinned: false,
            attribute_key: None,
            relation_subject_id: None,
            relation_object_id: None,
        })
        .unwrap();
}

#[tokio::test]
async fn memory_api_reuses_session_origin_and_csrf_boundaries() {
    let api = TestApi::new();
    let (status, body) = api
        .request_with_credentials(
            "/api/v1/console/memories/targets",
            Some(json!({})),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthenticated");

    let (status, body) = api
        .request_with_credentials(
            "/api/v1/console/memories/targets",
            Some(json!({})),
            Some((&api.cookie, "")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "csrf_failed");

    let (status, body) = api
        .request_with_credentials(
            "/api/v1/console/memories/targets",
            Some(json!({})),
            Some((&api.cookie, "wrong-csrf")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "csrf_failed");

    let (status, body) = api
        .request_with_credentials(
            "/api/v1/console/memories/targets",
            Some(json!({})),
            Some((&api.cookie, &api.csrf)),
            Some("https://evil.example"),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "origin_denied");
}

#[tokio::test]
async fn memory_api_targets_and_like_search_are_opaque_and_scoped() {
    let api = TestApi::new();
    let (status, targets) = api
        .post("/api/v1/console/memories/targets", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{targets}");
    assert_eq!(targets["data"]["total"], 1);
    let serialized = targets.to_string();
    assert!(serialized.contains("memory_target:v1:"));
    assert!(!serialized.contains("user-default"));
    assert!(!serialized.contains("app-default"));
    assert!(!serialized.contains("raw-source"));

    let (status, literal_percent) = api
        .post(
            "/api/v1/console/memories/list",
            json!({"target_ref": api.target_ref, "keyword": "%", "page_size": 100}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{literal_percent}");
    assert_eq!(literal_percent["data"]["total"], 1);

    let (status, source_search) = api
        .post(
            "/api/v1/console/memories/list",
            json!({"target_ref": api.target_ref, "keyword": "raw-source"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{source_search}");
    assert_eq!(source_search["data"]["total"], 0);

    let zero_digest = "0000000000000000000000000000000000000000000000000000000000000000";
    let (status, conflicting_filters) = api
        .post(
            "/api/v1/console/memories/list",
            json!({
                "target_ref": api.target_ref,
                "platform": "onebot",
                "account_ref": format!("memory_account:v1:{zero_digest}"),
                "group_ref": format!("memory_group:v1:{zero_digest}"),
                "subject_ref": format!("memory_subject:v1:{zero_digest}")
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{conflicting_filters}");
    assert_eq!(conflicting_filters["data"]["total"], 0);
    assert!(
        conflicting_filters["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let (status, beyond) = api
        .post(
            "/api/v1/console/memories/list",
            json!({"target_ref": api.target_ref, "page": 99, "page_size": 20}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{beyond}");
    assert!(beyond["data"]["items"].as_array().unwrap().is_empty());
    assert_eq!(beyond["data"]["total"], 1);

    let (status, body) = api
        .post(
            "/api/v1/console/memories/list",
            json!({"target_ref": "memory_target:v1:0000000000000000000000000000000000000000000000000000000000000000"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    let (status, body) = api
        .post(
            "/api/v1/console/memories/targets",
            json!({"scope_key": "private:user-default"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_json");
}

#[tokio::test]
async fn memory_api_crud_returns_real_versions_and_keeps_history() {
    let api = TestApi::new();
    let (status, created) = api
        .post(
            "/api/v1/console/memories/create",
            json!({
                "target_ref": api.target_ref,
                "content": "管理员创建的内容",
                "category": "note",
                "visibility": "private"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["data"]["memory"]["version"], 1);
    assert_eq!(created["data"]["memory"]["content"], "管理员创建的内容");
    assert_eq!(created["data"]["memory"]["source_type"], "manual_import");
    assert!(
        created["data"]["memory"]
            .get("created_by_user_id")
            .is_none()
    );
    assert!(created["data"]["memory"].get("source_text").is_none());
    assert!(created["data"]["memory"].get("source_ref").is_none());

    let old_memory_ref = created["data"]["memory"]["memory_ref"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, updated) = api
        .post(
            "/api/v1/console/memories/update",
            json!({
                "target_ref": api.target_ref,
                "memory_ref": old_memory_ref,
                "expected_version": 1,
                "patch": {"content": "管理员编辑后的内容"}
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["data"]["memory"]["version"], 2);
    assert_eq!(updated["data"]["archived_count"], 1);
    let updated_memory_ref = updated["data"]["memory"]["memory_ref"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, stale) = api
        .post(
            "/api/v1/console/memories/update",
            json!({
                "target_ref": api.target_ref,
                "memory_ref": old_memory_ref,
                "expected_version": 1,
                "patch": {"content": "stale write"}
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], "conflict");
    assert!(!stale.to_string().contains("stale write"));

    let (status, archived) = api
        .post(
            "/api/v1/console/memories/archive",
            json!({
                "target_ref": api.target_ref,
                "memory_ref": updated_memory_ref,
                "expected_version": 2
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert_eq!(archived["data"]["memory"]["status"], "archived");
    assert_eq!(archived["data"]["memory"]["version"], 3);

    let (status, restored) = api
        .post(
            "/api/v1/console/memories/restore",
            json!({
                "target_ref": api.target_ref,
                "memory_ref": updated_memory_ref,
                "expected_version": 3
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["data"]["memory"]["status"], "active");
    assert_eq!(restored["data"]["memory"]["version"], 4);

    let (status, empty_patch) = api
        .post(
            "/api/v1/console/memories/update",
            json!({
                "target_ref": api.target_ref,
                "memory_ref": updated_memory_ref,
                "expected_version": 4,
                "patch": {}
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(empty_patch["error"]["code"], "validation_error");
}

#[tokio::test]
async fn memory_api_audit_failure_rolls_back_mutations_and_consumes_commit_token() {
    let api = TestApi::new();
    let (status, initial) = api
        .post(
            "/api/v1/console/memories/list",
            json!({"target_ref": api.target_ref, "status": "active"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    let initial_memory_ref = initial["data"]["items"][0]["memory_ref"]
        .as_str()
        .unwrap()
        .to_owned();

    api.set_audit_failure(true);
    let (status, create_failed) = api
        .post(
            "/api/v1/console/memories/create",
            json!({
                "target_ref": api.target_ref,
                "content": "审计失败创建不应落库",
                "category": "note",
                "visibility": "private"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{create_failed}");
    api.set_audit_failure(false);
    let (status, after_create) = api
        .post(
            "/api/v1/console/memories/list",
            json!({"target_ref": api.target_ref, "keyword": "审计失败创建不应落库"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{after_create}");
    assert_eq!(after_create["data"]["total"], 0);

    api.set_audit_failure(true);
    let (status, update_failed) = api
        .post(
            "/api/v1/console/memories/update",
            json!({
                "target_ref": api.target_ref,
                "memory_ref": initial_memory_ref,
                "expected_version": 1,
                "patch": {"content": "审计失败更新不应落库"}
            }),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{update_failed}");
    api.set_audit_failure(false);
    let (status, after_update) = api
        .post(
            "/api/v1/console/memories/list",
            json!({"target_ref": api.target_ref, "keyword": "审计失败更新不应落库"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{after_update}");
    assert_eq!(after_update["data"]["total"], 0);
    let (status, unchanged) = api
        .post(
            "/api/v1/console/memories/get",
            json!({"target_ref": api.target_ref, "memory_ref": initial_memory_ref}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{unchanged}");
    assert_eq!(unchanged["data"]["version"], 1);
    assert_eq!(unchanged["data"]["content"], "初始 % 内容");

    let (status, prepared) = api
        .post(
            "/api/v1/console/memories/operations/prepare",
            json!({"operation": "clear_target", "target_ref": api.target_ref}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    let token = prepared["data"]["confirmation_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let commit_body = json!({
        "operation": "clear_target",
        "target_ref": api.target_ref,
        "confirmation_token": token
    });
    api.set_audit_failure(true);
    let (status, commit_failed) = api
        .post(
            "/api/v1/console/memories/operations/commit",
            commit_body.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{commit_failed}");
    api.set_audit_failure(false);
    let (status, after_commit) = api
        .post(
            "/api/v1/console/memories/list",
            json!({"target_ref": api.target_ref, "status": "active"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{after_commit}");
    assert_eq!(after_commit["data"]["total"], 1);

    let (status, replay) = api
        .post("/api/v1/console/memories/operations/commit", commit_body)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{replay}");
}

#[tokio::test]
async fn memory_api_prepare_commit_is_actor_bound_one_shot_and_snapshot_safe() {
    let api = TestApi::new();
    let (status, prepared) = api
        .post(
            "/api/v1/console/memories/operations/prepare",
            json!({"operation": "clear_target", "target_ref": api.target_ref}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    assert_eq!(prepared["data"]["affected_count"], 1);
    let token = prepared["data"]["confirmation_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(token.starts_with("memory_confirmation:v1:"));

    let commit_body = json!({
        "operation": "clear_target",
        "target_ref": api.target_ref,
        "confirmation_token": token.clone()
    });
    let (status, wrong_actor) = api
        .post_as_other(
            "/api/v1/console/memories/operations/commit",
            commit_body.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{wrong_actor}");
    assert_eq!(wrong_actor["error"]["code"], "permission_denied");

    let (status, committed) = api
        .post(
            "/api/v1/console/memories/operations/commit",
            commit_body.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_eq!(committed["data"]["affected_count"], 1);
    assert_eq!(committed["data"]["operation"], "clear_target");

    let audit_dump = api
        .database
        .connection()
        .unwrap()
        .query_row(
            "SELECT group_concat(
                coalesce(event_type, '') || '|' || coalesce(outcome, '') || '|' ||
                coalesce(request_id, '') || '|' || coalesce(target_digest, '') || '|' ||
                coalesce(safe_error_code, ''), '\n'
             ) FROM console_audit_events",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap()
        .unwrap_or_default();
    assert!(!audit_dump.contains(&token));
    assert!(!audit_dump.contains("raw-source"));
    assert!(!audit_dump.contains("初始 % 内容"));

    let (status, replay) = api
        .post("/api/v1/console/memories/operations/commit", commit_body)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{replay}");
    assert_eq!(replay["error"]["code"], "not_found");

    let profile_ref = api.seed_target(
        MemoryTarget::group_profile(group_scope("group-default"), personal_scope("user-default")),
        "群画像内容",
    );
    let (status, prepared) = api
        .post(
            "/api/v1/console/memories/operations/prepare",
            json!({"operation": "disable_group_profile", "target_ref": profile_ref}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    let token = prepared["data"]["confirmation_token"].clone();
    let (status, disabled) = api
        .post(
            "/api/v1/console/memories/operations/commit",
            json!({
                "operation": "disable_group_profile",
                "target_ref": profile_ref,
                "confirmation_token": token
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(disabled["data"]["affected_count"], 1);
    assert_eq!(
        disabled["data"]["capabilities"]["can_disable_group_profile"],
        false
    );
}

#[tokio::test]
async fn memory_api_is_not_registered_when_console_is_disabled_and_reports_unavailable() {
    let api = TestApi::new();
    let mut disabled_state = api.state.clone();
    disabled_state.config.web_console_enabled = false;
    for path in [
        "/api/v1/console/memories/targets",
        "/api/v1/console/memories/list",
        "/memory",
        "/query",
        "/v1/chat",
    ] {
        let response = build_router(disabled_state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }

    let mut unavailable_state = api.state.clone();
    unavailable_state.memory_management = None;
    let response = build_router(unavailable_state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/console/memories/targets")
                .header("content-type", "application/json")
                .header("host", "localhost")
                .header("cookie", format!("{SESSION_COOKIE_NAME}={}", api.cookie))
                .header("x-csrf-token", &api.csrf)
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "memory_unavailable");
}
