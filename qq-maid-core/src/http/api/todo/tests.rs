//! Todo 管理 API 集成测试。

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
    management::{AdminAuth, SESSION_COOKIE_NAME},
    runtime::tools::todo::{TodoManagementService, TodoStore},
    storage::{APP_MIGRATIONS, database::SqliteDatabase, notification::NotificationOutboxStore},
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
}

impl TestApi {
    fn new() -> Self {
        let (database, directory) =
            SqliteDatabase::open_temp_directory("todo-management-api", APP_MIGRATIONS).unwrap();
        let token_file = directory.join("config/secrets/bootstrap.token");
        let auth = AdminAuth::open(database.clone(), token_file.clone()).unwrap();
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
        let upstream = UpstreamStatus::default();
        let mut state = OpsHttpState::from_parts(
            OpsHttpConfig {
                web_console_enabled: true,
                web_console_allowed_origins: Vec::new(),
                web_console_trusted_proxy_ips: Vec::new(),
                web_console_secure_cookies: false,
            },
            observe_provider(Arc::new(MockProvider), upstream.clone()),
            upstream,
        );
        state.admin_auth = Some(auth);
        state = state.with_todo_management(TodoManagementService::new(
            TodoStore::new(database.clone()),
            NotificationOutboxStore::new(database),
        ));
        Self {
            state,
            cookie: issued.cookie_value,
            csrf: issued.session.csrf_token,
        }
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.request("POST", path, Some(body), true).await
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        authenticated: bool,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("host", "localhost")
            .header("x-request-id", "todo-api-test");
        if authenticated {
            builder = builder
                .header("cookie", format!("{}={}", SESSION_COOKIE_NAME, self.cookie))
                .header("x-csrf-token", &self.csrf);
        }
        let body = body
            .map(|value| Body::from(value.to_string()))
            .unwrap_or_else(Body::empty);
        let response = build_router(self.state.clone())
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        // 路由方法不匹配时由 Axum 在进入 API Handler 前直接生成 405，没有请求上下文。
        if status != StatusCode::METHOD_NOT_ALLOWED {
            assert_eq!(
                response
                    .headers()
                    .get("x-request-id")
                    .and_then(|value| value.to_str().ok()),
                Some("todo-api-test")
            );
        }
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
        (status, json)
    }

    async fn create(&self, title: &str, extra: Value) -> Value {
        let mut body = json!({"title": title});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap_or_default());
        let (status, response) = self.post("/api/v1/console/todo/create", body).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        response["data"].clone()
    }
}

#[tokio::test]
async fn todo_api_requires_authentication_and_only_registers_post() {
    let api = TestApi::new();
    let (status, body) = api
        .request(
            "POST",
            "/api/v1/console/todo/create",
            Some(json!({"title": "未认证"})),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "unauthenticated");

    let (status, _) = api
        .request("GET", "/api/v1/console/todo/list", None, true)
        .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn todo_create_validates_json_and_empty_title() {
    let api = TestApi::new();
    let created = api
        .create(
            "准备周报",
            json!({"detail": "整理本周进度", "due_date": "2099-08-01"}),
        )
        .await;
    assert_eq!(created["title"], "准备周报");
    assert_eq!(created["detail"], "整理本周进度");
    assert_eq!(created["status"], "pending");
    assert!(created.get("user_id").is_none());
    assert!(created.get("scope_key").is_none());

    let (status, body) = api
        .post("/api/v1/console/todo/create", json!({"title": "  "}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");

    let (status, body) = api
        .post("/api/v1/console/todo/create", json!({"detail": "缺标题"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_json");

    let (status, _) = api
        .post(
            "/api/v1/console/todo/create",
            json!({"title": "日期错误", "due_date": "2099-99-99"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn todo_list_paginates_in_repository_with_stable_order_and_total() {
    let api = TestApi::new();
    for index in 1..=25 {
        api.create(&format!("第 {index} 项"), json!({})).await;
    }

    let (status, default_page) = api.post("/api/v1/console/todo/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(default_page["data"]["page"], 1);
    assert_eq!(default_page["data"]["page_size"], 20);
    assert_eq!(default_page["data"]["total"], 25);
    assert_eq!(default_page["data"]["total_pages"], 2);
    assert_eq!(default_page["data"]["items"].as_array().unwrap().len(), 20);

    let (_, first) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"page": 1, "page_size": 10}),
        )
        .await;
    let (_, first_again) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"page": 1, "page_size": 10}),
        )
        .await;
    assert_eq!(first["data"]["items"], first_again["data"]["items"]);
    assert_eq!(first["data"]["total_pages"], 3);

    let (_, last) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"page": 3, "page_size": 10}),
        )
        .await;
    assert_eq!(last["data"]["items"].as_array().unwrap().len(), 5);

    let (_, beyond) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"page": 4, "page_size": 10}),
        )
        .await;
    assert!(beyond["data"]["items"].as_array().unwrap().is_empty());
    assert_eq!(beyond["data"]["total"], 25);

    let (_, management_max) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"page": 1, "page_size": 100}),
        )
        .await;
    assert_eq!(
        management_max["data"]["items"].as_array().unwrap().len(),
        25
    );
}

#[tokio::test]
async fn todo_list_reuses_existing_status_keyword_recurrence_and_time_filters() {
    let api = TestApi::new();
    let completed = api
        .create("项目 Alpha 周报", json!({"due_date": "2099-08-01"}))
        .await;
    api.create("项目 Beta 无日期", json!({})).await;
    api.create(
        "周期巡检",
        json!({"due_date": "2099-08-02", "recurrence_kind": "daily"}),
    )
    .await;
    let (status, _) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": completed["id"], "status": "completed"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, completed_page) = api
        .post("/api/v1/console/todo/list", json!({"status": "completed"}))
        .await;
    assert_eq!(completed_page["data"]["total"], 1);
    assert_eq!(
        completed_page["data"]["items"][0]["title"],
        "项目 Alpha 周报"
    );

    let (_, keyword_page) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"status": "all", "keyword": "项目 Beta"}),
        )
        .await;
    assert_eq!(keyword_page["data"]["total"], 1);

    let (_, recurring_page) = api
        .post("/api/v1/console/todo/list", json!({"recurring": true}))
        .await;
    assert_eq!(recurring_page["data"]["total"], 1);
    assert_eq!(recurring_page["data"]["items"][0]["title"], "周期巡检");

    let (_, no_due_page) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"status": "pending", "time_filter": "no_due_date"}),
        )
        .await;
    assert_eq!(no_due_page["data"]["total"], 1);
    assert_eq!(no_due_page["data"]["items"][0]["title"], "项目 Beta 无日期");

    let (_, date_page) = api
        .post(
            "/api/v1/console/todo/list",
            json!({
                "status": "pending",
                "date_start": "2099-08-02",
                "date_end": "2099-08-02"
            }),
        )
        .await;
    assert_eq!(date_page["data"]["total"], 1);
}

#[tokio::test]
async fn todo_get_update_and_delete_follow_domain_semantics() {
    let api = TestApi::new();
    let created = api
        .create(
            "初始标题",
            json!({"detail": "待清空", "due_date": "2099-08-01"}),
        )
        .await;
    let id = created["id"].clone();

    let (status, fetched) = api
        .post("/api/v1/console/todo/get", json!({"id": id}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["data"]["title"], "初始标题");

    let (status, updated) = api
        .post(
            "/api/v1/console/todo/update",
            json!({
                "id": id,
                "title": "更新标题",
                "detail": null,
                "due_date": null,
                "status": "completed"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["data"]["title"], "更新标题");
    assert_eq!(updated["data"]["detail"], Value::Null);
    assert_eq!(updated["data"]["due_date"], Value::Null);
    assert_eq!(updated["data"]["status"], "completed");

    let (status, conflict) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": id, "title": "不能直接编辑终态"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "conflict");

    let (status, restored) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": id, "status": "pending", "detail": "恢复后编辑"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(restored["data"]["detail"], "恢复后编辑");
    assert_eq!(restored["data"]["status"], "pending");

    let (status, deleted) = api
        .post("/api/v1/console/todo/delete", json!({"id": id}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["data"]["deleted"], true);

    for path in ["get", "update", "delete"] {
        let payload = if path == "update" {
            json!({"id": id, "title": "不存在"})
        } else {
            json!({"id": id})
        };
        let (status, body) = api
            .post(&format!("/api/v1/console/todo/{path}"), payload)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }
}

#[tokio::test]
async fn todo_list_rejects_invalid_pagination_and_filter_combinations() {
    let api = TestApi::new();
    for payload in [
        json!({"page": 0}),
        json!({"page_size": 0}),
        json!({"page_size": 101}),
        json!({"date_start": "2099-01-01"}),
        json!({"due_date": "2099-01-01", "time_filter": "no_due_date"}),
        json!({"status": "all", "time_filter": "overdue"}),
    ] {
        let (status, body) = api.post("/api/v1/console/todo/list", payload).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["code"], "validation_error");
    }
}
