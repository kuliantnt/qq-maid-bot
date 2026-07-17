use super::*;
use crate::runtime::{
    pending::{
        PreparedAction, PreparedActionExecutionContext, PreparedActionMetadata,
        PreparedActionState, PreparedActionValidationError,
    },
    tools::todo::{TodoPendingPayload, TodoStatus, valid_last_visible_todo_query},
};
use uuid::Uuid;

fn test_store() -> SessionStore {
    SessionStore::new(
        SqliteDatabase::open_temp("qq-maid-session-test", SESSION_MIGRATIONS).unwrap(),
    )
}

fn test_meta() -> SessionMeta {
    SessionMeta::new(
        "group:g1",
        Some("u1".to_owned()),
        Some("g1".to_owned()),
        None,
        None,
        "qq_official",
    )
}

fn write_pending_json_for_test(store: &SessionStore, session_id: &str, pending_json: &str) {
    let conn = store.connection().unwrap();
    conn.execute(
        "UPDATE sessions SET pending_operation_json = ?1 WHERE session_id = ?2",
        params![pending_json, session_id],
    )
    .unwrap();
}

fn pending_todo_delete(summary: &str) -> PreparedAction {
    TodoPendingPayload::TodoBulkDelete {
        initiator_user_id: Some("u1".to_owned()),
        owner_key: "u1".to_owned(),
        item_ids: vec!["todo-1".to_owned()],
        matched_count: 1,
        status: TodoStatus::Pending,
        summary: summary.to_owned(),
        source_condition: "测试范围".to_owned(),
        created_at: now_iso_cn(),
    }
    .into_prepared_action("group:g1")
}

fn prepared_action(scope_key: &str) -> PreparedAction {
    PreparedAction::new(
        PreparedActionMetadata {
            domain: "todo".to_owned(),
            action_kind: "todo_add".to_owned(),
            initiator_user_id: Some("u1".to_owned()),
            owner_key: Some("u1".to_owned()),
            scope_key: scope_key.to_owned(),
            created_at: "2026-07-15T10:00:00+08:00".to_owned(),
            expires_at: "2026-07-15T10:10:00+08:00".to_owned(),
        },
        serde_json::json!({"title": "测试"}),
        serde_json::json!({
            "kind": "todo_add",
            "initiator_user_id": "u1",
            "owner_key": "u1",
            "draft": {"title": "测试"},
            "created_at": "2026-07-15T10:00:00+08:00"
        }),
    )
}

#[test]
fn pending_execution_claim_is_atomic_and_revision_guarded() {
    let store = test_store();
    let meta = test_meta();
    let mut session = store.create(&meta, "原子领取", true).unwrap();
    session.pending_operation = Some(prepared_action(&meta.scope_key));
    store.save(&mut session).unwrap();
    let context = PreparedActionExecutionContext {
        initiator_user_id: Some("u1"),
        owner_key: Some("u1"),
        scope_key: &meta.scope_key,
        expected_revision: 1,
        now: "2026-07-15T10:05:00+08:00",
    };

    let claimed = store
        .claim_pending_execution(&session.session_id, &context)
        .unwrap();
    let PendingExecutionClaim::Claimed(claimed_session) = claimed else {
        panic!("first claim should succeed");
    };
    assert_eq!(
        claimed_session.pending_operation.as_ref().unwrap().state(),
        PreparedActionState::Executing
    );

    let repeated = store
        .claim_pending_execution(&session.session_id, &context)
        .unwrap();
    assert!(matches!(
        repeated,
        PendingExecutionClaim::Rejected {
            error: PreparedActionValidationError::InvalidState,
            ..
        }
    ));

    let failed = store
        .mark_pending_execution_failed(&session.session_id, 1)
        .unwrap();
    assert_eq!(
        failed.pending_operation.as_ref().unwrap().state(),
        PreparedActionState::Failed
    );
    assert_eq!(
        failed.pending_operation.as_ref().unwrap().revision(),
        1,
        "失败状态保留原 revision，不能被当成一次修订后重试"
    );
}

#[test]
fn create_active_and_list_sessions_for_scope() {
    let store = test_store();
    let meta = test_meta();

    let mut first = store.create(&meta, "旧话题", true).unwrap();
    first.updated_at = "2026-06-01T10:00:00+08:00".to_owned();
    first.append_message("user", "hello");
    store.save(&mut first).unwrap();
    let second = store.create(&meta, "新话题", true).unwrap();

    let active = store.get_or_create_active(&meta).unwrap();
    assert_eq!(active.session_id, second.session_id);

    let sessions = store
        .list_for_scope("group:g1", Some(&second.session_id))
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title, "旧话题");
}

#[test]
fn session_exchange_persists_same_turn_actor_for_user_and_assistant() {
    let store = test_store();
    let meta = test_meta();
    let mut session = store.create(&meta, "actor history", true).unwrap();
    let actor = SessionTurnActor {
        actor_ref: Some("actor_1234abcd".to_owned()),
        display_name: Some("初墨".to_owned()),
        display_name_source: Some("manual".to_owned()),
        group_member_role: Some("member".to_owned()),
        identity_source: Some("event".to_owned()),
    };
    session.set_turn_actor(Some(actor.clone()));

    store
        .append_exchange(&mut session, "/set 昵称 初墨", "当前展示名：初墨")
        .unwrap();
    let restored = store.get(&session.session_id).unwrap().unwrap();

    assert_eq!(restored.history.len(), 2);
    assert_eq!(restored.history[0].turn_actor.as_ref(), Some(&actor));
    assert_eq!(restored.history[1].turn_actor.as_ref(), Some(&actor));
    assert_eq!(restored.history[0].content, "/set 昵称 初墨");
    assert_eq!(restored.history[1].content, "当前展示名：初墨");
}

#[test]
fn reset_keeps_session_but_clears_context() {
    let store = test_store();
    let meta = test_meta();
    let mut session = store.create(&meta, "话题", true).unwrap();
    session.summary = "摘要".to_owned();
    session.append_message("user", "hi");
    session.pending_operation = Some(pending_todo_delete("新待办"));

    session.reset();
    store.save(&mut session).unwrap();
    let reloaded = store.get_or_create_active(&meta).unwrap();

    assert!(reloaded.summary.is_empty());
    assert!(reloaded.history.is_empty());
    assert!(reloaded.pending_operation.is_none());
}

#[test]
fn sqlite_reopen_restores_active_title_and_message_order() {
    let path = std::env::temp_dir().join(format!("qq-maid-session-reopen-{}.db", Uuid::new_v4()));
    let meta = test_meta();
    let first_db = SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap();
    let store = SessionStore::new(first_db);
    let mut session = store.create(&meta, "重启测试", true).unwrap();
    session.append_message("user", "第一条");
    session.append_message("assistant", "第二条");
    session.append_message("user", "第三条");
    store.save(&mut session).unwrap();
    let expected_id = session.session_id.clone();
    drop(store);

    let reopened = SessionStore::new(SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap());
    let restored = reopened.get_or_create_active(&meta).unwrap();

    assert_eq!(restored.session_id, expected_id);
    assert_eq!(restored.title, "重启测试");
    assert_eq!(
        restored
            .history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["第一条", "第二条", "第三条"]
    );
}

#[test]
fn compact_history_persists_summary_and_archive() {
    let store = test_store();
    let meta = test_meta();
    let mut session = store.create(&meta, "压缩测试", true).unwrap();
    for index in 0..6 {
        session.append_message("user", &format!("消息 {index}"));
    }

    store.compact_history(&mut session, "摘要", 2).unwrap();
    let reloaded = store.get_or_create_active(&meta).unwrap();

    assert_eq!(reloaded.summary, "摘要");
    assert_eq!(reloaded.history.len(), 2);
    assert!(
        reloaded
            .extra
            .get("archived_history")
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() == 1)
    );
    let archived_ids = reloaded
        .extra
        .get("archived_history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|archive| archive.get("history").and_then(Value::as_array))
        .flatten()
        .filter_map(|message| message.get("message_id").and_then(Value::as_i64))
        .collect::<Vec<_>>();
    assert_eq!(archived_ids.len(), 4);
    assert!(archived_ids.iter().all(|id| *id > 0));
    assert!(
        reloaded
            .history
            .iter()
            .all(|message| message.message_id.is_some())
    );
}

#[test]
fn ordinary_save_preserves_stable_message_ids() {
    let store = test_store();
    let meta = test_meta();
    let mut session = store.create(&meta, "稳定 ID", true).unwrap();
    store
        .append_exchange(&mut session, "第一轮问题", "第一轮回复")
        .unwrap();
    let original_ids = session
        .history
        .iter()
        .map(|message| message.message_id.unwrap())
        .collect::<Vec<_>>();

    session.summary = "只更新状态".to_owned();
    session.state.insert("custom".to_owned(), Value::Bool(true));
    store.save(&mut session).unwrap();
    let reloaded = store.get(&session.session_id).unwrap().unwrap();
    let reloaded_ids = reloaded
        .history
        .iter()
        .map(|message| message.message_id.unwrap())
        .collect::<Vec<_>>();

    assert_eq!(reloaded_ids, original_ids);
}

#[test]
fn update_title_if_current_preserves_newer_session_data() {
    let store = test_store();
    let meta = test_meta();
    let mut snapshot = store.create(&meta, "", true).unwrap();
    snapshot.append_message("user", "第二轮问题");
    snapshot.append_message("assistant", "第二轮回复");
    store.save(&mut snapshot).unwrap();

    let mut current = store.get_or_create_active(&meta).unwrap();
    current.append_message("user", "第三轮问题");
    current.summary = "后续摘要".to_owned();
    store.save(&mut current).unwrap();

    let updated = store
        .update_title_if_current(&snapshot.session_id, DEFAULT_SESSION_TITLE, "后台标题")
        .unwrap();
    let reloaded = store.get_or_create_active(&meta).unwrap();

    assert!(updated);
    assert_eq!(reloaded.title, "后台标题");
    assert_eq!(reloaded.summary, "后续摘要");
    assert_eq!(
        reloaded
            .history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["第二轮问题", "第二轮回复", "第三轮问题"]
    );
}

#[test]
fn update_title_if_current_skips_after_manual_rename() {
    let store = test_store();
    let meta = test_meta();
    let session = store.create(&meta, "", true).unwrap();

    let mut renamed = store.get_or_create_active(&meta).unwrap();
    renamed.title = "手工标题".to_owned();
    store.save(&mut renamed).unwrap();

    let updated = store
        .update_title_if_current(&session.session_id, DEFAULT_SESSION_TITLE, "后台标题")
        .unwrap();
    let reloaded = store.get_or_create_active(&meta).unwrap();

    assert!(!updated);
    assert_eq!(reloaded.title, "手工标题");
}

#[test]
fn sqlite_reopen_restores_pending_and_last_queries() {
    let path =
        std::env::temp_dir().join(format!("qq-maid-session-json-fields-{}.db", Uuid::new_v4()));
    let meta = test_meta();
    let store = SessionStore::new(SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap());
    let mut session = store.create(&meta, "跨进程状态", true).unwrap();
    session.pending_operation = Some(pending_todo_delete("需要确认的待办"));
    session.last_todo_query = Some(LastTodoQuery {
        owner_key: "u1".to_owned(),
        query_type: "pending".to_owned(),
        condition: "全部".to_owned(),
        result_ids: vec!["1".to_owned(), "2".to_owned()],
        created_at: now_iso_cn(),
    });
    session.last_todo_action = Some(LastTodoAction {
        owner_key: "u1".to_owned(),
        item_id: "2".to_owned(),
        title: "恢复的待办".to_owned(),
        action: "restored".to_owned(),
        resulting_status: TodoStatus::Pending,
        created_at: now_iso_cn(),
    });
    session.last_memory_query = Some(LastMemoryQuery {
        actor_id: Some("u1".to_owned()),
        query_type: "list".to_owned(),
        condition: "全部".to_owned(),
        scope_type: Some("personal".to_owned()),
        scope_id: Some("u1".to_owned()),
        memory_kind: Some("personal".to_owned()),
        subject_id: None,
        result_ids: vec!["m1".to_owned()],
        created_at: now_iso_cn(),
    });
    store.save(&mut session).unwrap();
    drop(store);

    let reopened = SessionStore::new(SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap());
    let restored = reopened.get_or_create_active(&meta).unwrap();

    assert_eq!(restored.pending_operation, session.pending_operation);
    assert_eq!(restored.last_todo_query, session.last_todo_query);
    assert_eq!(restored.last_todo_action, session.last_todo_action);
    assert_eq!(restored.last_memory_query, session.last_memory_query);
}

#[test]
fn flat_pending_json_is_cleared_on_read() {
    for kind in ["todo_add", "todo_delete", "memory_create"] {
        let store = test_store();
        let meta = test_meta();
        let session = store.create(&meta, "旧 memory pending", true).unwrap();
        write_pending_json_for_test(
            &store,
            &session.session_id,
            &serde_json::json!({
                "kind": kind,
                "created_at": now_iso_cn(),
                "payload": {"ignored": true}
            })
            .to_string(),
        );

        let reloaded = store.get_or_create_active(&meta).unwrap();

        assert_eq!(reloaded.session_id, session.session_id, "kind={kind}");
        assert!(reloaded.pending_operation.is_none(), "kind={kind}");
        let conn = store.connection().unwrap();
        let stored: Option<String> = conn
            .query_row(
                "SELECT pending_operation_json FROM sessions WHERE session_id = ?1",
                params![session.session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored.is_none(), "kind={kind}");
    }
}

#[test]
fn unknown_pending_json_is_cleared_instead_of_blocking_session() {
    let store = test_store();
    let meta = test_meta();
    let session = store.create(&meta, "未知 pending", true).unwrap();
    write_pending_json_for_test(
        &store,
        &session.session_id,
        r#"{"kind":"unknown_pending","created_at":"2026-07-01T00:00:00+08:00"}"#,
    );

    let reloaded = store.get_or_create_active(&meta).unwrap();
    assert!(reloaded.pending_operation.is_none());

    write_pending_json_for_test(&store, &session.session_id, "{");
    let reloaded = store.get_or_create_active(&meta).unwrap();
    assert!(reloaded.pending_operation.is_none());
}

#[test]
fn unsupported_prepared_action_schema_is_cleared_on_read() {
    let store = test_store();
    let meta = test_meta();
    let session = store.create(&meta, "旧版本 envelope", true).unwrap();
    let mut value = serde_json::to_value(prepared_action("group:g1")).unwrap();
    value["schema_version"] = serde_json::json!(0);
    write_pending_json_for_test(&store, &session.session_id, &value.to_string());

    let reloaded = store.get_or_create_active(&meta).unwrap();
    assert!(reloaded.pending_operation.is_none());
    let conn = store.connection().unwrap();
    let stored: Option<String> = conn
        .query_row(
            "SELECT pending_operation_json FROM sessions WHERE session_id = ?1",
            params![session.session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.is_none());
}

#[test]
fn append_exchange_with_latest_merges_query_snapshot_without_overwriting_newer_fields() {
    let store = test_store();
    let meta = test_meta();
    let mut stale = store.create(&meta, "合并测试", true).unwrap();
    stale.append_message("user", "旧问题");
    store.save(&mut stale).unwrap();

    let mut latest = store.get_or_create_active(&meta).unwrap();
    latest.pending_operation = Some(pending_todo_delete("较新的 pending"));
    latest.last_todo_action = Some(LastTodoAction {
        owner_key: "group:g1".to_owned(),
        item_id: "todo-new".to_owned(),
        title: "较新的最近对象".to_owned(),
        action: "completed".to_owned(),
        resulting_status: TodoStatus::Completed,
        created_at: now_iso_cn(),
    });
    latest.append_message("assistant", "较新的回复");
    store.save(&mut latest).unwrap();

    stale.remember_last_todo_query(
        "group:g1",
        "list",
        "",
        vec!["todo-a".to_owned(), "todo-b".to_owned()],
    );
    stale.last_memory_query = Some(LastMemoryQuery {
        actor_id: Some("u1".to_owned()),
        query_type: "list".to_owned(),
        condition: String::new(),
        scope_type: Some("personal".to_owned()),
        scope_id: Some("u1".to_owned()),
        memory_kind: Some("personal".to_owned()),
        subject_id: None,
        result_ids: vec!["memory-a".to_owned()],
        created_at: now_iso_cn(),
    });
    store
        .append_exchange_with_latest(
            &mut stale,
            "看一下待办",
            "1. A\n2. B",
            |current, stale| {
                current.state = stale.state.clone();
                current.last_todo_query = stale.last_todo_query.clone();
                current.last_memory_query = stale.last_memory_query.clone();
            },
        )
        .unwrap();

    let merged = store.get_or_create_active(&meta).unwrap();
    assert!(merged.pending_operation.is_some());
    assert_eq!(
        merged
            .last_todo_action
            .as_ref()
            .map(|item| item.item_id.as_str()),
        Some("todo-new")
    );
    assert_eq!(
        merged
            .last_todo_query
            .as_ref()
            .map(|query| query.result_ids.clone()),
        Some(vec!["todo-a".to_owned(), "todo-b".to_owned()])
    );
    assert_eq!(
        merged
            .last_memory_query
            .as_ref()
            .map(|query| query.result_ids.clone()),
        Some(vec!["memory-a".to_owned()])
    );
    assert_eq!(
        merged
            .history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["旧问题", "较新的回复", "看一下待办", "1. A\n2. B"]
    );
}

#[test]
fn due_date_todo_query_is_valid_visible_snapshot() {
    let store = test_store();
    let meta = test_meta();
    let mut session = store.create(&meta, "日期待办", true).unwrap();
    session.remember_last_todo_query("u1", "due-date", "2026-07-03", vec!["todo-a".to_owned()]);

    let query = valid_last_visible_todo_query(&mut session, "u1").unwrap();
    assert_eq!(query.query_type, "due-date");
    assert_eq!(query.result_ids, vec!["todo-a"]);
}

#[test]
fn session_schema_migrations_keep_legacy_rows_compatible() {
    let path =
        std::env::temp_dir().join(format!("qq-maid-session-v2-compat-{}.db", Uuid::new_v4()));
    let meta = test_meta();
    let legacy_database = SqliteDatabase::open(&path, &[SESSION_SCHEMA_V1]).unwrap();
    let legacy_query = LastTodoQuery {
        owner_key: "u1".to_owned(),
        query_type: "list".to_owned(),
        condition: String::new(),
        result_ids: vec!["1".to_owned()],
        created_at: now_iso_cn(),
    };
    let conn = legacy_database.connection().unwrap();
    conn.execute(
        "INSERT INTO sessions (
            session_id, scope, scope_key, user_id, group_id, guild_id, channel_id, platform,
            created_at, updated_at, title, state_json, summary, pending_operation_json,
            last_todo_query_json, last_memory_query_json, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            "legacy-session",
            meta.scope.as_str(),
            meta.scope_key.as_str(),
            meta.user_id.as_deref(),
            meta.group_id.as_deref(),
            meta.guild_id.as_deref(),
            meta.channel_id.as_deref(),
            meta.platform.as_str(),
            "2026-06-30T00:00:00+08:00",
            "2026-06-30T00:00:00+08:00",
            "旧 schema",
            "{}",
            "",
            Option::<String>::None,
            serde_json::to_string(&legacy_query).unwrap(),
            Option::<String>::None,
            "{}",
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session_active (scope_key, session_id, updated_at)
         VALUES (?1, ?2, ?3)",
        params![
            meta.scope_key.as_str(),
            "legacy-session",
            "2026-06-30T00:00:00+08:00"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session_messages (session_id, message_index, role, content, ts)
         VALUES (?1, 0, 'user', '旧消息', ?2)",
        params!["legacy-session", "2026-06-30T00:00:00+08:00"],
    )
    .unwrap();
    drop(conn);
    drop(legacy_database);

    let reopened = SessionStore::new(SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap());
    let restored = reopened.get_or_create_active(&meta).unwrap();

    assert_eq!(restored.title, "旧 schema");
    assert_eq!(restored.last_todo_query, Some(legacy_query));
    assert!(restored.last_todo_action.is_none());
    assert_eq!(restored.history[0].content, "旧消息");
    assert!(restored.history[0].turn_actor.is_none());
}

#[test]
fn session_state_cleanup_migration_removes_only_removed_chat_state_keys() {
    let path = std::env::temp_dir().join(format!(
        "qq-maid-session-state-cleanup-{}.db",
        Uuid::new_v4()
    ));
    let meta = test_meta();
    let legacy_database =
        SqliteDatabase::open(&path, &[SESSION_SCHEMA_V1, SESSION_SCHEMA_V2]).unwrap();
    let legacy_state = serde_json::json!({
        "current_speaker_hint": "旧身份",
        "recent_session_focus": "旧焦点",
        "recent_innerworld_focus": "旧里世界焦点",
        "active_scene": "旧场景",
        "expected_mode": "旧模式",
        "last_user_correction": "旧修正",
        "known_correction": "旧已知修正",
        "current_topic": "保留话题",
        "custom_extension_state": "保留扩展",
    });
    let conn = legacy_database.connection().unwrap();
    conn.execute(
        "INSERT INTO sessions (
            session_id, scope, scope_key, user_id, group_id, guild_id, channel_id, platform,
            created_at, updated_at, title, state_json, summary, pending_operation_json,
            last_todo_query_json, last_memory_query_json, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            "legacy-state-session",
            meta.scope.as_str(),
            meta.scope_key.as_str(),
            meta.user_id.as_deref(),
            meta.group_id.as_deref(),
            meta.guild_id.as_deref(),
            meta.channel_id.as_deref(),
            meta.platform.as_str(),
            "2026-06-30T00:00:00+08:00",
            "2026-06-30T00:00:00+08:00",
            "旧状态",
            legacy_state.to_string(),
            "",
            Option::<String>::None,
            Option::<String>::None,
            Option::<String>::None,
            "{}",
        ],
    )
    .unwrap();
    drop(conn);
    drop(legacy_database);

    let reopened = SessionStore::new(SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap());
    let restored = reopened.get("legacy-state-session").unwrap().unwrap();

    for removed_key in [
        "current_speaker_hint",
        "recent_session_focus",
        "recent_innerworld_focus",
        "active_scene",
        "expected_mode",
        "last_user_correction",
        "known_correction",
    ] {
        assert!(
            !restored.state.contains_key(removed_key),
            "{removed_key} should be removed"
        );
    }
    assert_eq!(
        restored.state.get("current_topic").and_then(Value::as_str),
        Some("保留话题")
    );
    assert_eq!(
        restored
            .state
            .get("custom_extension_state")
            .and_then(Value::as_str),
        Some("保留扩展")
    );
}

#[test]
fn set_active_rejects_missing_session_without_changing_current() {
    let store = test_store();
    let meta = test_meta();
    let current = store.create(&meta, "当前", true).unwrap();

    let err = store
        .set_active_session_id(&meta.scope_key, "missing-session")
        .unwrap_err();

    assert_eq!(err.code(), "database_error");
    assert_eq!(
        store.get_or_create_active(&meta).unwrap().session_id,
        current.session_id
    );
}

#[test]
fn broken_active_pointer_reports_data_error() {
    let database =
        SqliteDatabase::open_temp("qq-maid-session-broken-active", SESSION_MIGRATIONS).unwrap();
    let conn = database.connection().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO session_active (scope_key, session_id, updated_at)
         VALUES ('group:g1', 'missing-session', '2026-06-01T10:00:00+08:00');
         PRAGMA foreign_keys = ON;",
    )
    .unwrap();
    drop(conn);
    let store = SessionStore::new(database);

    let err = store.get_active(&test_meta()).unwrap_err();

    assert_eq!(err.code(), "data_error");
    assert!(err.message().contains("active session"));
}

#[test]
fn session_record_defaults_still_deserialize_for_tests() {
    let mut session = serde_json::from_str::<SessionRecord>(
        r#"{
            "session_id": "legacy-session",
            "scope": "group",
            "scope_key": "group:g1",
            "created_at": "2026-06-01T10:00:00+08:00",
            "updated_at": "2026-06-01T10:00:00+08:00"
        }"#,
    )
    .unwrap();

    normalize_session(&mut session);

    assert_eq!(session.session_id, "legacy-session");
    assert!(session.last_todo_query.is_none());
    assert!(session.last_todo_action.is_none());
}
