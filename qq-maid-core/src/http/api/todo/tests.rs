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
    identity::conversation_scope_key,
    management::{AdminAuth, SESSION_COOKIE_NAME},
    runtime::{
        session::{SessionMeta, SessionStore},
        tools::todo::{
            TodoItemDraft, TodoManagementService, TodoRecurrenceKind, TodoRecurrenceUnit,
            TodoStore, TodoTimePrecision,
        },
    },
    storage::{
        APP_MIGRATIONS,
        database::SqliteDatabase,
        notification::{NotificationOutboxStore, NotificationStatus},
    },
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
    todo_store: TodoStore,
    notification_store: NotificationOutboxStore,
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
        state.admin_auth = Some(auth.clone());
        let todo_store = TodoStore::new(database.clone());
        let notification_store = NotificationOutboxStore::new(database.clone());
        let default_scope = conversation_scope_key(
            "qq_official",
            Some("app-default"),
            "private",
            "user-default",
        );
        SessionStore::new(database.clone())
            .get_or_create_active(&SessionMeta::new_with_account(
                default_scope.clone(),
                Some("user-default".to_owned()),
                None,
                None,
                None,
                "qq_official",
                Some("app-default".to_owned()),
            ))
            .unwrap();
        let default_owner = TodoStore::owner(Some("user-default"), &default_scope);
        let anchor = todo_store
            .create(&default_owner, todo_draft("目标锚点"))
            .unwrap();
        let service = TodoManagementService::new(todo_store.clone(), notification_store.clone());
        let target_ref = service.get(&anchor.id).unwrap().target.target_ref.unwrap();
        todo_store
            .delete_by_ids(&default_owner, &[anchor.id])
            .unwrap();
        database
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO console_admins (username, password_hash, disabled, created_at)
                 SELECT 'admin2', password_hash, 0, created_at
                 FROM console_admins WHERE username = 'admin'",
                [],
            )
            .unwrap();
        let other_preauth = auth.issue_preauth().unwrap();
        let other_issued = auth
            .login(
                &other_preauth.cookie_value,
                &other_preauth.session.csrf_token,
                "admin2",
                "correct horse battery staple",
            )
            .unwrap();
        state = state.with_todo_management(service);
        Self {
            state,
            cookie: issued.cookie_value,
            csrf: issued.session.csrf_token,
            other_cookie: other_issued.cookie_value,
            other_csrf: other_issued.session.csrf_token,
            target_ref,
            todo_store,
            notification_store,
        }
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.request("POST", path, Some(body), true).await
    }

    async fn post_as_other(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.request_with_credentials(
            "POST",
            path,
            Some(body),
            Some((&self.other_cookie, &self.other_csrf)),
        )
        .await
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        authenticated: bool,
    ) -> (StatusCode, Value) {
        let credentials = authenticated.then_some((self.cookie.as_str(), self.csrf.as_str()));
        self.request_with_credentials(method, path, body, credentials)
            .await
    }

    async fn request_with_credentials(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        credentials: Option<(&str, &str)>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("host", "localhost")
            .header("x-request-id", "todo-api-test");
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
        let mut body = json!({"target_ref": self.target_ref, "title": title});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap_or_default());
        let (status, response) = self.post("/api/v1/console/todo/create", body).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        response["data"].clone()
    }

    fn seed_todo(
        &self,
        platform: &str,
        account_id: &str,
        scope_type: &str,
        target_id: &str,
        user_id: &str,
        title: &str,
    ) -> (
        crate::runtime::tools::todo::TodoOwner,
        crate::runtime::tools::todo::TodoItem,
    ) {
        let scope = conversation_scope_key(platform, Some(account_id), scope_type, target_id);
        let owner = TodoStore::owner(Some(user_id), &scope);
        let item = self.todo_store.create(&owner, todo_draft(title)).unwrap();
        (owner, item)
    }

    fn target_ref_for_item(&self, id: &str) -> String {
        self.state
            .todo_management
            .as_ref()
            .unwrap()
            .get(id)
            .unwrap()
            .target
            .target_ref
            .unwrap()
    }
}

fn todo_draft(title: &str) -> TodoItemDraft {
    TodoItemDraft {
        title: title.to_owned(),
        detail: None,
        raw_text: None,
        due_date: None,
        due_at: None,
        reminder_at: None,
        time_precision: TodoTimePrecision::None,
        recurrence_kind: TodoRecurrenceKind::None,
        recurrence_interval_days: 0,
        recurrence_interval: 0,
        recurrence_unit: TodoRecurrenceUnit::Day,
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
    assert_eq!(created["target"]["platform"], "qq_official");
    assert_eq!(created["target"]["scope_type"], "private");
    assert_eq!(created["target"]["user_id"], "user-default");

    let (status, body) = api
        .post(
            "/api/v1/console/todo/create",
            json!({"target_ref": api.target_ref, "title": "  "}),
        )
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
            json!({"target_ref": api.target_ref, "title": "日期错误", "due_date": "2099-99-99"}),
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

#[tokio::test]
async fn global_management_is_shared_by_admins_and_preserves_chat_owner_scope() {
    let api = TestApi::new();
    let (qq_owner, qq) = api.seed_todo(
        "qq_official",
        "app-1",
        "private",
        "qq-user",
        "qq-user",
        "QQ 待办",
    );
    let (onebot_owner, onebot) = api.seed_todo(
        "onebot",
        "bot-1",
        "group",
        "group-1",
        "member-1",
        "OneBot 待办",
    );
    let (wechat_owner, wechat) = api.seed_todo(
        "wechat_service",
        "wx-1",
        "private",
        "wx-user",
        "wx-user",
        "微信待办",
    );

    let (status, first_admin) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"status": "all", "page_size": 100}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_admin["data"]["total"], 3);
    let (status, second_admin) = api
        .post_as_other(
            "/api/v1/console/todo/list",
            json!({"status": "all", "page_size": 100}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second_admin["data"]["items"], first_admin["data"]["items"]);

    let (status, updated) = api
        .post_as_other(
            "/api/v1/console/todo/update",
            json!({"id": qq.id, "title": "管理员已修改 QQ 待办"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        api.todo_store
            .get_by_id(&qq_owner, &qq.id)
            .unwrap()
            .unwrap()
            .title,
        "管理员已修改 QQ 待办"
    );

    let (status, updated) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": onebot.id, "title": "管理员已修改 OneBot 待办"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        api.todo_store
            .get_by_id(&onebot_owner, &onebot.id)
            .unwrap()
            .unwrap()
            .title,
        "管理员已修改 OneBot 待办"
    );

    let (status, updated) = api
        .post_as_other(
            "/api/v1/console/todo/update",
            json!({"id": wechat.id, "title": "管理员已修改微信待办"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        api.todo_store
            .get_by_id(&wechat_owner, &wechat.id)
            .unwrap()
            .unwrap()
            .title,
        "管理员已修改微信待办"
    );

    // 普通聊天使用的 owner-scoped Store 入口仍只能看到当前真实 owner 的记录。
    assert_eq!(api.todo_store.list_all(&qq_owner).unwrap().len(), 1);
    assert_eq!(api.todo_store.list_all(&onebot_owner).unwrap().len(), 1);
    assert_eq!(api.todo_store.list_all(&wechat_owner).unwrap().len(), 1);

    let (status, filtered) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"platform": "onebot", "scope_type": "group"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filtered["data"]["total"], 1);
    assert_eq!(filtered["data"]["items"][0]["id"], onebot.id);

    let (status, _) = api
        .post_as_other("/api/v1/console/todo/delete", json!({"id": onebot.id}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        api.todo_store
            .get_by_id(&onebot_owner, &onebot.id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn create_uses_verified_private_or_group_target_and_rejects_unsupported_reminder() {
    let api = TestApi::new();
    let (private_owner, private_anchor) = api.seed_todo(
        "qq_official",
        "app-2",
        "private",
        "private-2",
        "private-2",
        "私聊锚点",
    );
    let private_ref = api.target_ref_for_item(&private_anchor.id);
    let (status, private_created) = api
        .post(
            "/api/v1/console/todo/create",
            json!({"target_ref": private_ref, "title": "API 私聊待办"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{private_created}");
    assert_eq!(private_created["data"]["target"]["scope_type"], "private");
    assert!(
        api.todo_store
            .list_all(&private_owner)
            .unwrap()
            .iter()
            .any(|item| item.title == "API 私聊待办")
    );

    let (group_owner, group_anchor) = api.seed_todo(
        "onebot",
        "bot-2",
        "group",
        "group-2",
        "member-2",
        "群聊锚点",
    );
    let group_ref = api.target_ref_for_item(&group_anchor.id);
    let (status, group_created) = api
        .post(
            "/api/v1/console/todo/create",
            json!({
                "target_ref": group_ref,
                "title": "API 群聊提醒",
                "reminder_at": "2099-08-01T09:00:00+08:00"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{group_created}");
    assert_eq!(group_created["data"]["target"]["platform"], "onebot");
    assert!(
        api.todo_store
            .list_all(&group_owner)
            .unwrap()
            .iter()
            .any(|item| item.title == "API 群聊提醒")
    );
    let tasks = api.notification_store.list_all_for_test().unwrap();
    let group_task = tasks
        .iter()
        .find(|task| task.source_id == group_created["data"]["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(group_task.target.platform, "onebot11");
    assert_eq!(group_task.target.target_id, "group-2");

    let (_, wechat_anchor) = api.seed_todo(
        "wechat_service",
        "wx-2",
        "private",
        "wx-user-2",
        "wx-user-2",
        "微信锚点",
    );
    let wechat_ref = api.target_ref_for_item(&wechat_anchor.id);
    let (status, rejected) = api
        .post(
            "/api/v1/console/todo/create",
            json!({
                "target_ref": wechat_ref,
                "title": "不可投递提醒",
                "reminder_at": "2099-08-01T10:00:00+08:00"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(rejected["error"]["code"], "validation_error");
    let (_, absent) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"keyword": "不可投递提醒"}),
        )
        .await;
    assert_eq!(absent["data"]["total"], 0);
}

#[tokio::test]
async fn reminder_update_replaces_outbox_and_delete_cancels_it() {
    let api = TestApi::new();
    let (_, item) = api.seed_todo(
        "qq_official",
        "app-3",
        "private",
        "user-3",
        "user-3",
        "提醒待办",
    );
    let (status, first) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "reminder_at": "2099-08-01T09:00:00+08:00"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "reminder_at": "2099-08-01T10:00:00+08:00"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let tasks = api.notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].status, NotificationStatus::Cancelled);
    assert_eq!(tasks[1].status, NotificationStatus::Pending);
    assert_eq!(tasks[1].scheduled_at, "2099-08-01T10:00:00+08:00");

    let (status, _) = api
        .post("/api/v1/console/todo/delete", json!({"id": item.id}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let tasks = api.notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks[1].status, NotificationStatus::Cancelled);
}

#[tokio::test]
async fn recurring_completion_advances_atomically_and_reschedules_reminder() {
    let api = TestApi::new();
    let scope = conversation_scope_key(
        "qq_official",
        Some("app-recurring"),
        "private",
        "recurring-user",
    );
    let owner = TodoStore::owner(Some("recurring-user"), &scope);
    let mut draft = todo_draft("周期提醒");
    draft.due_date = Some("2099-08-01".to_owned());
    draft.reminder_at = Some("2099-08-01T09:00:00+08:00".to_owned());
    draft.recurrence_kind = TodoRecurrenceKind::Daily;
    draft.recurrence_interval_days = 1;
    let item = api.todo_store.create(&owner, draft).unwrap();

    let (status, completed) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "title": "周期提醒已更新", "status": "completed"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["data"]["status"], "pending");
    assert_eq!(completed["data"]["title"], "周期提醒已更新");
    assert_ne!(completed["data"]["reminder_at"], item.reminder_at.unwrap());
    let stored = api.todo_store.get_by_id(&owner, &item.id).unwrap().unwrap();
    assert_eq!(
        stored.status,
        crate::runtime::tools::todo::TodoStatus::Pending
    );
    assert_eq!(stored.title, "周期提醒已更新");
    assert_eq!(api.notification_store.list_all_for_test().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_target_atomic_restore_patch_and_invalid_ids_leave_data_unchanged() {
    let api = TestApi::new();
    let created = api.create("原子恢复", json!({})).await;
    api.post(
        "/api/v1/console/todo/update",
        json!({"id": created["id"], "status": "completed"}),
    )
    .await;
    let (status, invalid_patch) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": created["id"], "status": "pending", "title": "   "}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(invalid_patch["error"]["code"], "validation_error");
    let (_, unchanged) = api
        .post("/api/v1/console/todo/get", json!({"id": created["id"]}))
        .await;
    assert_eq!(unchanged["data"]["status"], "completed");
    assert_eq!(unchanged["data"]["title"], "原子恢复");

    let (status, invalid_target) = api
        .post(
            "/api/v1/console/todo/create",
            json!({"target_ref": "todo_target:v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "title": "幽灵 Todo"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(invalid_target["error"]["code"], "validation_error");

    let legacy_owner = TodoStore::owner(Some("legacy-user"), "mystery:legacy-scope");
    api.todo_store
        .create(&legacy_owner, todo_draft("异常旧作用域"))
        .unwrap();
    let (status, degraded) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"keyword": "异常旧作用域"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        degraded["data"]["items"][0]["target"]["scope_type"],
        "unknown"
    );
    assert_eq!(
        degraded["data"]["items"][0]["target"]["target_ref"],
        Value::Null
    );
    assert_eq!(
        degraded["data"]["items"][0]["target"]["diagnostic"],
        "unrecognized_scope"
    );

    for id in [
        json!("0"),
        json!("000"),
        json!("abc"),
        json!(-1),
        json!(9223372036854775808_u64),
        json!("9223372036854775808"),
    ] {
        let (status, body) = api
            .post("/api/v1/console/todo/get", json!({"id": id}))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["code"], "validation_error");
    }
}

#[tokio::test]
async fn todo_api_still_rejects_missing_csrf_and_cross_origin_requests() {
    let api = TestApi::new();
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/console/todo/list")
        .header("content-type", "application/json")
        .header("host", "localhost")
        .header("cookie", format!("{}={}", SESSION_COOKIE_NAME, api.cookie))
        .body(Body::from("{}"))
        .unwrap();
    let response = build_router(api.state.clone())
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/console/todo/list")
        .header("content-type", "application/json")
        .header("host", "localhost")
        .header("origin", "https://evil.example")
        .header("cookie", format!("{}={}", SESSION_COOKIE_NAME, api.cookie))
        .header("x-csrf-token", api.csrf)
        .body(Body::from("{}"))
        .unwrap();
    let response = build_router(api.state.clone())
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
