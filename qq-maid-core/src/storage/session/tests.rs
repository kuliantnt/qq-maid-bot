use super::*;
use crate::runtime::{
    pending::PendingOperation,
    tools::todo::{TodoItemDraft, TodoPendingOperation, TodoStatus, valid_last_visible_todo_query},
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

fn pending_todo_add(title: &str) -> PendingOperation {
    TodoPendingOperation::TodoAdd {
        initiator_user_id: Some("u1".to_owned()),
        owner_key: "u1".to_owned(),
        draft: TodoItemDraft {
            title: title.to_owned(),
            detail: None,
            raw_text: Some(title.to_owned()),
            due_date: None,
            due_at: None,
            reminder_at: None,
            time_precision: Default::default(),
            recurrence_kind: Default::default(),
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: Default::default(),
        },
        allow_revision: false,
        created_at: now_iso_cn(),
    }
    .into()
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
fn reset_keeps_session_but_clears_context() {
    let store = test_store();
    let meta = test_meta();
    let mut session = store.create(&meta, "话题", true).unwrap();
    session.summary = "摘要".to_owned();
    session.append_message("user", "hi");
    session.pending_operation = Some(pending_todo_add("新待办"));

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
    session.pending_operation = Some(pending_todo_add("需要确认的待办"));
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
        query_type: "list".to_owned(),
        condition: "全部".to_owned(),
        scope_type: Some("personal".to_owned()),
        scope_id: Some("u1".to_owned()),
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
fn legacy_memory_pending_json_loads_as_no_pending() {
    for kind in ["memory_create", "memory_update", "memory_delete"] {
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
    }
}

#[test]
fn unknown_or_broken_pending_json_still_reports_decode_error() {
    let store = test_store();
    let meta = test_meta();
    let session = store.create(&meta, "未知 pending", true).unwrap();
    write_pending_json_for_test(
        &store,
        &session.session_id,
        r#"{"kind":"unknown_pending","created_at":"2026-07-01T00:00:00+08:00"}"#,
    );

    let err = store.get_or_create_active(&meta).unwrap_err();
    assert_eq!(err.code(), "decode_error");
    assert!(err.message().contains("pending operation"));

    write_pending_json_for_test(&store, &session.session_id, "{");
    let err = store.get_or_create_active(&meta).unwrap_err();
    assert_eq!(err.code(), "decode_error");
    assert!(err.message().contains("pending operation"));
}

#[test]
fn append_exchange_with_latest_merges_query_snapshot_without_overwriting_newer_fields() {
    let store = test_store();
    let meta = test_meta();
    let mut stale = store.create(&meta, "合并测试", true).unwrap();
    stale.append_message("user", "旧问题");
    store.save(&mut stale).unwrap();

    let mut latest = store.get_or_create_active(&meta).unwrap();
    latest.pending_operation = Some(pending_todo_add("较新的 pending"));
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
        query_type: "list".to_owned(),
        condition: String::new(),
        scope_type: Some("personal".to_owned()),
        scope_id: Some("u1".to_owned()),
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
fn session_schema_v2_keeps_legacy_rows_compatible() {
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
    drop(conn);
    drop(legacy_database);

    let reopened = SessionStore::new(SqliteDatabase::open(&path, SESSION_MIGRATIONS).unwrap());
    let restored = reopened.get_or_create_active(&meta).unwrap();

    assert_eq!(restored.title, "旧 schema");
    assert_eq!(restored.last_todo_query, Some(legacy_query));
    assert!(restored.last_todo_action.is_none());
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
