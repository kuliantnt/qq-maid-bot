//! 用户偏好与通用文件 API 集成测试。

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, Request, StatusCode},
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
    management::{
        AdminAuth, ConsoleUserDataService, PreferenceValuePatch, SESSION_COOKIE_NAME,
        UserFileModule, UserPreferencesPatch,
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

struct TestResponse {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Bytes,
}

impl TestResponse {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or_else(|_| json!({}))
    }
}

struct TestApi {
    state: OpsHttpState,
    cookie: String,
    csrf: String,
    other_cookie: String,
    other_csrf: String,
    admin_id: i64,
    database: SqliteDatabase,
    directory: PathBuf,
}

impl TestApi {
    fn new() -> Self {
        let (database, directory) =
            SqliteDatabase::open_temp_directory("console-user-data-api", APP_MIGRATIONS).unwrap();
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
        let (admin_id, _) = auth
            .authorize_admin(&issued.cookie_value, Some(&issued.session.csrf_token))
            .unwrap();

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
        )
        .with_console_user_data(ConsoleUserDataService::new(database.clone()));
        state.admin_auth = Some(auth);
        Self {
            state,
            cookie: issued.cookie_value,
            csrf: issued.session.csrf_token,
            other_cookie: other_issued.cookie_value,
            other_csrf: other_issued.session.csrf_token,
            admin_id,
            database,
            directory,
        }
    }

    async fn post(&self, path: &str, body: Value) -> TestResponse {
        self.post_with_credentials(path, body, Some((&self.cookie, Some(&self.csrf))))
            .await
    }

    async fn post_as_other(&self, path: &str, body: Value) -> TestResponse {
        self.post_with_credentials(
            path,
            body,
            Some((&self.other_cookie, Some(&self.other_csrf))),
        )
        .await
    }

    async fn post_with_credentials(
        &self,
        path: &str,
        body: Value,
        credentials: Option<(&str, Option<&str>)>,
    ) -> TestResponse {
        let request = request_builder(path, credentials)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.request(request).await
    }

    async fn upload(&self, filename: &str, content_type: &str, bytes: &[u8]) -> TestResponse {
        self.upload_with_credentials(
            filename,
            content_type,
            bytes,
            Some((&self.cookie, Some(&self.csrf))),
        )
        .await
    }

    async fn upload_as_other(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> TestResponse {
        self.upload_with_credentials(
            filename,
            content_type,
            bytes,
            Some((&self.other_cookie, Some(&self.other_csrf))),
        )
        .await
    }

    async fn upload_with_credentials(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
        credentials: Option<(&str, Option<&str>)>,
    ) -> TestResponse {
        let boundary = "qq-maid-test-boundary";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let request = request_builder("/api/v1/console/files/upload", credentials)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        self.request(request).await
    }

    async fn get_file(&self, file_id: &str) -> TestResponse {
        self.get_file_with_credentials(file_id, (&self.cookie, &self.csrf))
            .await
    }

    async fn get_file_as_other(&self, file_id: &str) -> TestResponse {
        self.get_file_with_credentials(file_id, (&self.other_cookie, &self.other_csrf))
            .await
    }

    async fn get_file_with_credentials(
        &self,
        file_id: &str,
        credentials: (&str, &str),
    ) -> TestResponse {
        let path = format!("/api/v1/console/files/get/{file_id}");
        let request = request_builder(&path, Some((credentials.0, Some(credentials.1))))
            .body(Body::empty())
            .unwrap();
        self.request(request).await
    }

    async fn request(&self, request: Request<Body>) -> TestResponse {
        let response = build_router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        if status != StatusCode::METHOD_NOT_ALLOWED && status != StatusCode::NOT_FOUND {
            assert_eq!(
                headers
                    .get("x-request-id")
                    .and_then(|value| value.to_str().ok()),
                Some("console-user-data-test")
            );
        }
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        TestResponse {
            status,
            headers,
            bytes,
        }
    }
}

fn request_builder(
    path: &str,
    credentials: Option<(&str, Option<&str>)>,
) -> axum::http::request::Builder {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "localhost")
        .header("x-request-id", "console-user-data-test");
    if let Some((cookie, csrf)) = credentials {
        builder = builder.header("cookie", format!("{SESSION_COOKIE_NAME}={cookie}"));
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
    }
    builder
}

fn data(response: &TestResponse) -> Value {
    assert_eq!(response.status, StatusCode::OK, "{}", response.json());
    response.json()["data"].clone()
}

async fn upload_id(api: &TestApi, filename: &str, bytes: &[u8]) -> String {
    data(&api.upload(filename, "image/webp", bytes).await)["file_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn preferences_return_defaults_and_support_ordered_partial_updates() {
    let api = TestApi::new();
    let defaults = data(
        &api.post("/api/v1/console/user-preferences/get", json!({}))
            .await,
    );
    assert_eq!(
        defaults,
        json!({
            "custom_colors": [],
            "background_file_ids": [],
            "active_background_file_id": null,
            "background_mode": "default",
            "kuliantnt": false,
        })
    );

    let updated = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"kuliantnt": true}),
        )
        .await,
    );
    assert_eq!(updated["kuliantnt"], true);
    assert_eq!(updated["custom_colors"], json!([]));

    let colors = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"custom_colors": ["#FF6699", "#8B5CF6", "theme-accent"]}),
        )
        .await,
    );
    assert_eq!(
        colors["custom_colors"],
        json!(["#FF6699", "#8B5CF6", "theme-accent"])
    );
    assert_eq!(colors["kuliantnt"], true);

    let other = data(
        &api.post_as_other("/api/v1/console/user-preferences/get", json!({}))
            .await,
    );
    assert_eq!(other["custom_colors"], json!([]));
    assert_eq!(other["kuliantnt"], false);
}

#[tokio::test]
async fn background_mode_persists_independently_and_keeps_invariants() {
    let api = TestApi::new();
    let file = upload_id(&api, "mode.webp", b"mode").await;

    // 选择特殊九宫格：模式持久化，且不残留活动背景文件。
    let special = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"background_mode": "special"}),
        )
        .await,
    );
    assert_eq!(special["background_mode"], "special");
    assert_eq!(special["active_background_file_id"], Value::Null);
    assert_eq!(special["kuliantnt"], false);

    // 选择无背景：模式回到 default，活动背景清空。
    let default_background = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"background_mode": "default", "active_background_file_id": null}),
        )
        .await,
    );
    assert_eq!(default_background["background_mode"], "default");
    assert_eq!(default_background["active_background_file_id"], Value::Null);

    // 激活自定义背景：active_background_file_id 表达自定义背景，模式字段只能是 default。
    let custom = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({
                "background_file_ids": [&file],
                "active_background_file_id": &file,
            }),
        )
        .await,
    );
    assert_eq!(custom["background_mode"], "default");
    assert_eq!(custom["active_background_file_id"], file);

    // 切回特殊背景时服务端主动清空活动背景，避免模式和文件同时存在。
    let back_to_special = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"background_mode": "special"}),
        )
        .await,
    );
    assert_eq!(back_to_special["background_mode"], "special");
    assert_eq!(back_to_special["active_background_file_id"], Value::Null);

    // 新模式字段通过读取接口原样返回，刷新后仍然一致。
    let reread = data(
        &api.post("/api/v1/console/user-preferences/get", json!({}))
            .await,
    );
    assert_eq!(reread["background_mode"], "special");
    assert_eq!(reread["active_background_file_id"], Value::Null);
    assert_eq!(reread["background_file_ids"], json!([file]));

    // 非法模式值返回 422/400 级校验错误，不写入。
    let invalid = api
        .post(
            "/api/v1/console/user-preferences/update",
            json!({"background_mode": "unknown"}),
        )
        .await;
    assert_ne!(invalid.status, StatusCode::OK);
    let still_special = data(
        &api.post("/api/v1/console/user-preferences/get", json!({}))
            .await,
    );
    assert_eq!(still_special["background_mode"], "special");
}

#[tokio::test]
async fn files_upload_list_read_delete_and_isolate_users() {
    let api = TestApi::new();
    let file_bytes = b"test-webp-content";
    let uploaded = data(
        &api.upload("background.webp", "image/webp", file_bytes)
            .await,
    );
    let file_id = uploaded["file_id"].as_str().unwrap();
    assert!(uuid::Uuid::parse_str(file_id).is_ok());
    assert_eq!(uploaded["filename"], "background.webp");
    assert_eq!(uploaded["content_type"], "image/webp");
    assert_eq!(uploaded["module"], "background");
    assert_eq!(uploaded["size"], file_bytes.len());
    assert_eq!(
        uploaded["url"],
        format!("/api/v1/console/files/get/{file_id}")
    );

    let listed = data(
        &api.post(
            "/api/v1/console/files/list",
            json!({"page": 1, "page_size": 20}),
        )
        .await,
    );
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["items"][0]["file_id"], file_id);

    // 知识库文件由知识领域入口创建；通用背景 API 必须按 module 隔离，而不是靠前端过滤。
    let knowledge = api
        .state
        .console_user_data
        .as_ref()
        .unwrap()
        .create_file_with_limit(
            api.admin_id,
            "knowledge.md".to_owned(),
            "text/markdown".to_owned(),
            b"knowledge-source".to_vec(),
            1024 * 1024,
            UserFileModule::Knowledge,
        )
        .unwrap();
    let knowledge_id = knowledge.file_id.clone();
    let listed_without_knowledge = data(
        &api.post(
            "/api/v1/console/files/list",
            json!({"page": 1, "page_size": 20}),
        )
        .await,
    );
    assert_eq!(listed_without_knowledge["total"], 1);
    assert_eq!(
        api.get_file(&knowledge_id).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        api.post(
            "/api/v1/console/files/delete",
            json!({"file_id": &knowledge_id}),
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );

    let read = api.get_file(file_id).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.bytes.as_ref(), file_bytes);
    assert_eq!(read.headers["content-type"], "image/webp");
    assert_eq!(read.headers["content-length"], file_bytes.len().to_string());
    assert_eq!(read.headers["x-content-type-options"], "nosniff");
    assert_eq!(read.headers["x-frame-options"], "DENY");

    let other_list = data(
        &api.post_as_other("/api/v1/console/files/list", json!({}))
            .await,
    );
    assert_eq!(other_list["total"], 0);
    assert_eq!(
        api.get_file_as_other(file_id).await.status,
        StatusCode::NOT_FOUND
    );
    let other_delete = api
        .post_as_other("/api/v1/console/files/delete", json!({"file_id": file_id}))
        .await;
    assert_eq!(other_delete.status, StatusCode::NOT_FOUND);

    let stored_filename: String = api
        .database
        .connection()
        .unwrap()
        .query_row(
            "SELECT storage_filename FROM console_user_files WHERE file_id = ?1",
            [file_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(stored_filename, "background.webp");
    assert!(
        api.directory
            .join("console-files")
            .join(stored_filename)
            .is_file()
    );

    let deleted = data(
        &api.post("/api/v1/console/files/delete", json!({"file_id": file_id}))
            .await,
    );
    assert_eq!(deleted, json!({"file_id": file_id, "deleted": true}));
    assert_eq!(api.get_file(file_id).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn backgrounds_validate_ownership_switch_active_and_clean_up_on_delete() {
    let api = TestApi::new();
    let first = upload_id(&api, "first.webp", b"first").await;
    let second = upload_id(&api, "second.webp", b"second").await;
    let foreign = data(
        &api.upload_as_other("foreign.webp", "image/webp", b"foreign")
            .await,
    )["file_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let non_image = data(
        &api.upload("not-an-image.md", "text/markdown", b"not an image")
            .await,
    )["file_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let gallery = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({
                "background_file_ids": [&first, &second],
                "active_background_file_id": &first,
            }),
        )
        .await,
    );
    assert_eq!(gallery["background_file_ids"], json!([first, second]));
    assert_eq!(gallery["active_background_file_id"], first);

    let switched = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"active_background_file_id": &second}),
        )
        .await,
    );
    assert_eq!(switched["active_background_file_id"], second);

    let default_background = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"active_background_file_id": null}),
        )
        .await,
    );
    assert_eq!(default_background["active_background_file_id"], Value::Null);
    data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"active_background_file_id": &second}),
        )
        .await,
    );

    let nonexistent_id = uuid::Uuid::new_v4().to_string();
    let unknown_active = api
        .post(
            "/api/v1/console/user-preferences/update",
            json!({"active_background_file_id": &nonexistent_id}),
        )
        .await;
    assert_eq!(unknown_active.status, StatusCode::UNPROCESSABLE_ENTITY);
    let nonexistent_gallery = api
        .post(
            "/api/v1/console/user-preferences/update",
            json!({"background_file_ids": [&first, nonexistent_id]}),
        )
        .await;
    assert_eq!(nonexistent_gallery.status, StatusCode::UNPROCESSABLE_ENTITY);
    let foreign_gallery = api
        .post(
            "/api/v1/console/user-preferences/update",
            json!({"background_file_ids": [&first, foreign]}),
        )
        .await;
    assert_eq!(foreign_gallery.status, StatusCode::UNPROCESSABLE_ENTITY);
    let non_image_gallery = api
        .post(
            "/api/v1/console/user-preferences/update",
            json!({"background_file_ids": [&non_image]}),
        )
        .await;
    assert_eq!(non_image_gallery.status, StatusCode::UNPROCESSABLE_ENTITY);

    let removed_active = data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"background_file_ids": [&first]}),
        )
        .await,
    );
    assert_eq!(removed_active["background_file_ids"], json!([first]));
    assert_eq!(removed_active["active_background_file_id"], Value::Null);
    data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({"active_background_file_id": &first}),
        )
        .await,
    );
    data(
        &api.post("/api/v1/console/files/delete", json!({"file_id": &first}))
            .await,
    );
    let cleaned = data(
        &api.post("/api/v1/console/user-preferences/get", json!({}))
            .await,
    );
    assert_eq!(cleaned["background_file_ids"], json!([]));
    assert_eq!(cleaned["active_background_file_id"], Value::Null);
}

#[tokio::test]
async fn resources_reject_missing_auth_csrf_and_unsafe_file_ids() {
    let api = TestApi::new();
    let unauthenticated = api
        .post_with_credentials("/api/v1/console/user-preferences/get", json!({}), None)
        .await;
    assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);

    let missing_csrf = api
        .post_with_credentials(
            "/api/v1/console/user-preferences/update",
            json!({"kuliantnt": true}),
            Some((&api.cookie, None)),
        )
        .await;
    assert_eq!(missing_csrf.status, StatusCode::FORBIDDEN);
    assert_eq!(missing_csrf.json()["error"]["code"], "csrf_failed");
    let upload_missing_csrf = api
        .upload_with_credentials(
            "unsafe.webp",
            "image/webp",
            b"unsafe",
            Some((&api.cookie, None)),
        )
        .await;
    assert_eq!(upload_missing_csrf.status, StatusCode::FORBIDDEN);

    let file_id = upload_id(&api, "protected.webp", b"protected").await;
    let read_missing_csrf = request_builder(
        &format!("/api/v1/console/files/get/{file_id}"),
        Some((&api.cookie, None)),
    )
    .body(Body::empty())
    .unwrap();
    assert_eq!(
        api.request(read_missing_csrf).await.status,
        StatusCode::FORBIDDEN
    );

    let invalid = api.get_file("not-a-uuid").await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    let traversal = api.get_file("%2E%2E").await;
    assert_ne!(traversal.status, StatusCode::OK);
    let delete_traversal = api
        .post(
            "/api/v1/console/files/delete",
            json!({"file_id": "../../app.db"}),
        )
        .await;
    assert_eq!(delete_traversal.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn preferences_and_files_survive_service_reconstruction() {
    let mut api = TestApi::new();
    let file_id = upload_id(&api, "persistent.webp", b"persistent-file").await;
    data(
        &api.post(
            "/api/v1/console/user-preferences/update",
            json!({
                "custom_colors": ["first", "second"],
                "background_file_ids": [&file_id],
                "active_background_file_id": &file_id,
                "kuliantnt": true,
            }),
        )
        .await,
    );

    let reopened = SqliteDatabase::open(api.database.path(), APP_MIGRATIONS).unwrap();
    api.state.console_user_data = Some(ConsoleUserDataService::new(reopened));
    let preferences = data(
        &api.post("/api/v1/console/user-preferences/get", json!({}))
            .await,
    );
    assert_eq!(preferences["custom_colors"], json!(["first", "second"]));
    assert_eq!(preferences["active_background_file_id"], file_id);
    assert_eq!(preferences["background_mode"], "default");
    assert_eq!(preferences["kuliantnt"], true);
    let listed = data(&api.post("/api/v1/console/files/list", json!({})).await);
    assert_eq!(listed["items"][0]["file_id"], file_id);
    assert_eq!(api.get_file(&file_id).await.bytes, b"persistent-file"[..]);
}

#[tokio::test]
async fn reading_all_allowed_backgrounds_does_not_consume_management_quota() {
    const MAX_BACKGROUNDS: usize = 64;

    let api = TestApi::new();
    let service = ConsoleUserDataService::new(api.database.clone());
    let mut file_ids = Vec::with_capacity(MAX_BACKGROUNDS);
    for index in 0..MAX_BACKGROUNDS {
        let file = service
            .create_file(
                api.admin_id,
                format!("background-{index}.webp"),
                "image/webp".to_owned(),
                vec![u8::try_from(index).unwrap()],
            )
            .unwrap();
        file_ids.push(file.file_id);
    }
    service
        .update_preferences(
            api.admin_id,
            UserPreferencesPatch {
                background_file_ids: Some(file_ids.clone()),
                active_background_file_id: PreferenceValuePatch::Set(file_ids[0].clone()),
                ..UserPreferencesPatch::default()
            },
        )
        .unwrap();

    for (index, file_id) in file_ids.iter().enumerate() {
        let response = api.get_file(file_id).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.bytes.as_ref(), &[u8::try_from(index).unwrap()]);
    }

    // 只读文件认证使用独立路径，64 次读取后完整的 60 次管理动作额度仍然可用。
    for _ in 0..60 {
        assert_eq!(
            api.post("/api/v1/console/user-preferences/get", json!({}))
                .await
                .status,
            StatusCode::OK
        );
    }
    assert_eq!(
        api.post("/api/v1/console/user-preferences/get", json!({}))
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn multipart_body_limit_is_scoped_to_upload_route() {
    let api = TestApi::new();
    let upload = api
        .upload(
            "larger-than-json-limit.webp",
            "image/webp",
            &vec![7_u8; 70 * 1024],
        )
        .await;
    assert_eq!(upload.status, StatusCode::OK, "{}", upload.json());

    let oversized_json = format!(r#"{{"padding":"{}"}}"#, "x".repeat(70 * 1024));
    for path in [
        "/api/v1/console/user-preferences/get",
        "/api/v1/console/user-preferences/update",
        "/api/v1/console/files/list",
        "/api/v1/console/files/delete",
    ] {
        let request = request_builder(path, Some((&api.cookie, Some(&api.csrf))))
            .header("content-type", "application/json")
            .body(Body::from(oversized_json.clone()))
            .unwrap();
        assert_eq!(
            api.request(request).await.status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{path} must retain a small JSON body limit"
        );
    }
}
