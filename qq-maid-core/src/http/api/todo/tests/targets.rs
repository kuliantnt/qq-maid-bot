//! Todo 创建目标发现接口测试。

use super::*;

#[tokio::test]
async fn todo_targets_discovers_session_only_private_target_and_creates_first_todo() {
    let api = TestApi::new();
    assert!(
        api.todo_store
            .list_all(&TodoStore::owner(
                Some("user-default"),
                &conversation_scope_key(
                    "qq_official",
                    Some("app-default"),
                    "private",
                    "user-default"
                )
            ))
            .unwrap()
            .is_empty()
    );

    let (status, targets) = api
        .post(
            "/api/v1/console/todo/targets",
            json!({"platform": "qq_official", "user_id": "user-default"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{targets}");
    assert_eq!(targets["data"]["total"], 1);
    let target = &targets["data"]["items"][0];
    assert_eq!(target["platform"], "qq_official");
    assert_eq!(target["account_id"], "app-default");
    assert_eq!(target["scope_type"], "private");
    assert_eq!(target["user_id"], "user-default");
    assert_eq!(target["group_id"], Value::Null);
    assert_eq!(target["reminder_supported"], true);
    assert!(
        target["target_ref"]
            .as_str()
            .unwrap()
            .starts_with("todo_target:v1:")
    );
    assert!(target.get("owner_key").is_none());
    assert!(target.get("scope_key").is_none());

    let (status, created) = api
        .post(
            "/api/v1/console/todo/create",
            json!({
                "target_ref": target["target_ref"],
                "title": "Session 的第一条 Todo"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["data"]["title"], "Session 的第一条 Todo");

    // 同一 owner/scope 同时来自 Session 和 Todo 时仍只返回一个真实目标。
    let (_, deduplicated) = api
        .post(
            "/api/v1/console/todo/targets",
            json!({"platform": "qq_official", "user_id": "user-default"}),
        )
        .await;
    assert_eq!(deduplicated["data"]["total"], 1);
}

#[tokio::test]
async fn todo_targets_restore_group_member_and_report_unsupported_platform() {
    let api = TestApi::new();
    let group_scope = api.seed_session_target(
        "onebot",
        "bot-targets",
        "group",
        "group-targets",
        "member-targets",
    );
    api.seed_session_target(
        "wechat_service",
        "wx-targets",
        "private",
        "wx-target-user",
        "wx-target-user",
    );

    let (status, group_targets) = api
        .post(
            "/api/v1/console/todo/targets",
            json!({
                "platform": "onebot",
                "account_id": "bot-targets",
                "scope_type": "group",
                "group_id": "group-targets"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{group_targets}");
    assert_eq!(group_targets["data"]["total"], 1);
    let group = &group_targets["data"]["items"][0];
    assert_eq!(group["user_id"], "member-targets");
    assert_eq!(group["group_id"], "group-targets");
    assert_eq!(group["account_id"], "bot-targets");
    assert_eq!(group["reminder_supported"], true);

    let (status, created) = api
        .post(
            "/api/v1/console/todo/create",
            json!({"target_ref": group["target_ref"], "title": "群成员第一条 Todo"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let group_owner = TodoStore::owner(Some("member-targets"), &group_scope);
    assert_eq!(api.todo_store.list_all(&group_owner).unwrap().len(), 1);

    let (status, wechat_targets) = api
        .post(
            "/api/v1/console/todo/targets",
            json!({"platform": "wechat_service", "account_id": "wx-targets"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{wechat_targets}");
    assert_eq!(wechat_targets["data"]["total"], 1);
    assert_eq!(
        wechat_targets["data"]["items"][0]["reminder_supported"],
        false
    );
}

#[tokio::test]
async fn todo_targets_support_pagination_filters_authentication_and_csrf() {
    let api = TestApi::new();
    for index in 1..=5 {
        let user_id = format!("page-target-{index}");
        api.seed_session_target("onebot", "bot-page", "private", &user_id, &user_id);
    }

    let (status, first) = api
        .post(
            "/api/v1/console/todo/targets",
            json!({
                "platform": "onebot",
                "account_id": "bot-page",
                "scope_type": "private",
                "page": 1,
                "page_size": 2
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["data"]["total"], 5);
    assert_eq!(first["data"]["total_pages"], 3);
    assert_eq!(first["data"]["items"].as_array().unwrap().len(), 2);
    let (status, second) = api
        .post(
            "/api/v1/console/todo/targets",
            json!({
                "platform": "onebot",
                "account_id": "bot-page",
                "scope_type": "private",
                "page": 2,
                "page_size": 2
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_ne!(first["data"]["items"], second["data"]["items"]);

    let (status, filtered) = api
        .post(
            "/api/v1/console/todo/targets",
            json!({"platform": "onebot", "user_id": "page-target-3"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{filtered}");
    assert_eq!(filtered["data"]["total"], 1);

    for payload in [json!({"page": 0}), json!({"page_size": 101})] {
        let (status, body) = api.post("/api/v1/console/todo/targets", payload).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }

    let (status, unauthenticated) = api
        .request(
            "POST",
            "/api/v1/console/todo/targets",
            Some(json!({})),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{unauthenticated}");
    let (status, _) = api
        .request("GET", "/api/v1/console/todo/targets", None, true)
        .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/console/todo/targets")
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
}
