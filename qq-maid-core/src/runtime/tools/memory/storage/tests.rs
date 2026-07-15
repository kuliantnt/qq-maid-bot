use super::*;

fn test_store() -> MemoryStore {
    MemoryStore::new(SqliteDatabase::open_temp("qq-maid-memory-test", MEMORY_MIGRATIONS).unwrap())
}

fn create_memory(store: &MemoryStore, content: &str) -> MemoryRecord {
    store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: content.to_owned(),
            source_text: format!("/memory {content}"),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap()
}

fn create_scoped_memory(
    store: &MemoryStore,
    scope_type: MemoryScopeType,
    scope_id: &str,
    creator: &str,
    content: &str,
) -> MemoryRecord {
    store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type,
            scope_id: scope_id.to_owned(),
            created_by_user_id: creator.to_owned(),
            user_id: Some(creator.to_owned()),
            group_id: (scope_type == MemoryScopeType::Group).then(|| scope_id.to_owned()),
            content: content.to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap()
}

fn create_context_personal_memory(store: &MemoryStore, content: &str) -> MemoryRecord {
    create_context_personal_memory_with_meta(store, content, false, "2026-01-01T00:00:00+08:00")
}

fn create_context_personal_memory_with_meta(
    store: &MemoryStore,
    content: &str,
    pinned: bool,
    confirmed_at: &str,
) -> MemoryRecord {
    store
        .persist_v3(PersistMemoryRequest {
            target: MemoryTarget::personal("u1"),
            created_by_user_id: "u1".to_owned(),
            content: content.to_owned(),
            source_text: "seed".to_owned(),
            category: MemoryCategory::Note,
            legacy_scope: "general".to_owned(),
            visibility: MemoryVisibility::ContextOnly,
            source_type: MemorySourceType::UserConfirmed,
            source_ref: Some("test:context".to_owned()),
            confirmed_at: Some(confirmed_at.to_owned()),
            pinned,
            attribute_key: None,
            relation_subject_id: None,
            relation_object_id: None,
        })
        .unwrap()
        .record
}

#[test]
fn create_get_list_update_and_delete_memory() {
    let store = test_store();
    let created = store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "回复技术方案时先列出结论".to_owned(),
            source_text: "/memory 回复技术方案时先列出结论".to_owned(),
            memory_type: "preference".to_owned(),
            scope: "writing_style".to_owned(),
        })
        .unwrap();

    let listed = store
        .list(ListMemoryQuery {
            q: Some("结论".to_owned()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    let prefix = &created.id[..8];
    assert_eq!(store.get(prefix).unwrap().id, created.id);

    let updated = store
        .update(
            prefix,
            UpdateMemoryRequest {
                content: Some("回复技术方案时先列出结论和风险".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.content, "回复技术方案时先列出结论和风险");
    assert!(updated.updated_at.is_some());

    let deleted_id = store.delete(prefix).unwrap();
    assert_eq!(deleted_id, created.id);
    assert!(store.get(prefix).is_err());
}

#[test]
fn list_uses_stable_newest_first_order() {
    let store = test_store();
    let first = create_memory(&store, "第一条记忆");
    let second = create_memory(&store, "第二条记忆");

    let records = store.list(ListMemoryQuery::default()).unwrap();

    assert_eq!(records[0].id, second.id);
    assert_eq!(records[1].id, first.id);
}

#[test]
fn filters_by_scope_type_user_group_and_query_text() {
    let store = test_store();
    store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "技术方案回复先给结论".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "preference".to_owned(),
            scope: "writing_style".to_owned(),
        })
        .unwrap();
    create_memory(&store, "普通记忆");

    let records = store
        .list(ListMemoryQuery {
            q: Some("结论".to_owned()),
            scope: Some("writing_style".to_owned()),
            memory_type: Some("preference".to_owned()),
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].content, "技术方案回复先给结论");
}

#[test]
fn reports_not_found_and_invalid_update() {
    let store = test_store();

    assert_eq!(store.get("missing-id").unwrap_err().code(), "not_found");
    assert_eq!(store.get("abc").unwrap_err().code(), "bad_request");
    assert_eq!(
        store
            .update("missing-id", UpdateMemoryRequest::default())
            .unwrap_err()
            .code(),
        "bad_request"
    );
    assert_eq!(store.delete("missing-id").unwrap_err().code(), "not_found");
}

#[test]
fn sqlite_reopen_keeps_memory_records() {
    let path = std::env::temp_dir().join(format!("qq-maid-memory-reopen-{}.db", Uuid::new_v4()));
    let first_store = MemoryStore::new(SqliteDatabase::open(&path, MEMORY_MIGRATIONS).unwrap());
    let created = create_memory(&first_store, "重启后仍要保留");
    drop(first_store);

    let reopened = MemoryStore::new(SqliteDatabase::open(&path, MEMORY_MIGRATIONS).unwrap());
    let restored = reopened.get(&created.id).unwrap();

    assert_eq!(restored.content, "重启后仍要保留");
    assert_eq!(restored.ts, restored.created_at);
}

#[test]
fn stores_multiline_chinese_special_and_long_content() {
    let store = test_store();
    let content = format!(
        "第一行：中文、emoji-like 文本 :-) 和 SQL 符号 ' \" % _\n第二行：{}",
        "长文本".repeat(80)
    );

    let created = create_memory(&store, &content);
    let restored = store.get(&created.id).unwrap();

    assert_eq!(restored.content, content);
    assert!(restored.source_text.contains('\n'));
    assert_eq!(
        store
            .list(ListMemoryQuery {
                q: Some("% _".to_owned()),
                ..Default::default()
            })
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn scoped_crud_limits_prefix_resolution_to_current_scope() {
    let store = test_store();
    let personal = create_scoped_memory(&store, MemoryScopeType::Personal, "u1", "u1", "个人记忆");
    let group = create_scoped_memory(&store, MemoryScopeType::Group, "g1", "u1", "群记忆");

    let personal_records = store
        .list_scoped(ScopedMemoryQuery {
            scope_type: MemoryScopeType::Personal,
            scope_id: "u1".to_owned(),
            limit: Some(10),
            q: None,
            scope: None,
            memory_type: None,
        })
        .unwrap();
    assert_eq!(personal_records.len(), 1);
    assert_eq!(personal_records[0].id, personal.id);
    assert!(
        store
            .get_scoped(MemoryScopeType::Personal, "u1", &group.id[..8])
            .is_err()
    );

    let updated = store
        .update_scoped(
            MemoryScopeType::Personal,
            "u1",
            &personal.id[..8],
            UpdateMemoryRequest {
                content: Some("个人记忆已更新".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.content, "个人记忆已更新");
    assert!(
        store
            .delete_scoped(MemoryScopeType::Personal, "u1", &group.id[..8],)
            .is_err()
    );
}

#[test]
fn replace_scoped_creates_new_id_and_deletes_old_record() {
    let store = test_store();
    let old = create_scoped_memory(&store, MemoryScopeType::Personal, "u1", "u1", "旧记忆");

    let replaced = store
        .replace_scoped(ReplaceScopedStorageRequest {
            scope_type: MemoryScopeType::Personal,
            scope_id: "u1".to_owned(),
            id_or_prefix: old.id.clone(),
            created_by_user_id: "u1".to_owned(),
            user_id: Some("u1".to_owned()),
            group_id: None,
            content: "新记忆".to_owned(),
            source_text: "/memory edit 1 新记忆".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    assert_ne!(replaced.id, old.id);
    assert_eq!(replaced.content, "新记忆");
    assert!(store.get(&old.id).is_err());
    assert_eq!(store.get(&replaced.id).unwrap().content, "新记忆");
}

#[test]
fn replace_scoped_keeps_old_record_when_new_insert_fails() {
    let store = test_store();
    let old = create_scoped_memory(&store, MemoryScopeType::Personal, "u1", "u1", "旧记忆");
    store.abort_memory_insert_for_test().unwrap();

    let err = store
        .replace_scoped(ReplaceScopedStorageRequest {
            scope_type: MemoryScopeType::Personal,
            scope_id: "u1".to_owned(),
            id_or_prefix: old.id.clone(),
            created_by_user_id: "u1".to_owned(),
            user_id: Some("u1".to_owned()),
            group_id: None,
            content: "新记忆".to_owned(),
            source_text: "/memory edit 1 新记忆".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap_err();

    assert_eq!(err.code(), "io_error");
    assert_eq!(store.get(&old.id).unwrap().content, "旧记忆");
    let records = store.list(ListMemoryQuery::default()).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, old.id);
}

#[test]
fn replace_group_memory_is_atomic_storage_operation() {
    let store = test_store();
    let group = create_scoped_memory(&store, MemoryScopeType::Group, "g1", "u1", "群规则");

    let replaced = store
        .replace_scoped(ReplaceScopedStorageRequest {
            scope_type: MemoryScopeType::Group,
            scope_id: "g1".to_owned(),
            id_or_prefix: group.id.clone(),
            created_by_user_id: "u2".to_owned(),
            user_id: Some("u2".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "管理员替换".to_owned(),
            source_text: "/memory group edit 1 管理员替换".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();
    assert_eq!(replaced.content, "管理员替换");
    assert!(store.get(&group.id).is_err());
}

#[test]
fn context_merge_keeps_independent_layer_limits_and_visibility() {
    let store = test_store();
    for index in 0..4 {
        create_scoped_memory(
            &store,
            MemoryScopeType::Group,
            "g1",
            "u1",
            &format!("更旧的群记忆 {index}"),
        );
    }
    for index in 0..12 {
        create_context_personal_memory(&store, &format!("较新的个人记忆 {index}"));
    }

    let records = store
        .list_accessible_for_context(Some("u1"), Some("g1"), 12)
        .unwrap();

    assert_eq!(records.len(), 8);
    assert!(
        records
            .iter()
            .any(|record| record.content == "更旧的群记忆 3")
    );
    assert!(
        records
            .iter()
            .any(|record| record.content == "较新的个人记忆 11")
    );
    assert!(
        !records
            .iter()
            .any(|record| record.content == "较新的个人记忆 0")
    );
}

#[test]
fn context_layer_order_prioritizes_pinned_then_recent_confirmation() {
    let store = test_store();
    create_context_personal_memory_with_meta(
        &store,
        "普通确认记忆",
        false,
        "2026-01-01T00:00:00+08:00",
    );
    create_context_personal_memory_with_meta(
        &store,
        "最近确认记忆",
        false,
        "2026-02-01T00:00:00+08:00",
    );
    create_context_personal_memory_with_meta(&store, "固定记忆", true, "2025-01-01T00:00:00+08:00");

    let records = store
        .list_accessible_for_context(Some("u1"), None, 12)
        .unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| record.content.as_str())
            .collect::<Vec<_>>(),
        vec!["固定记忆", "最近确认记忆", "普通确认记忆"]
    );
}

#[test]
fn legacy_v2_database_upgrades_to_v3_and_reopens_conservatively() {
    let path = std::env::temp_dir().join(format!("qq-maid-memory-migration-{}.db", Uuid::new_v4()));
    {
        let database =
            SqliteDatabase::open(&path, &[MEMORY_SCHEMA_V1, MEMORY_SCOPE_SCHEMA_V2]).unwrap();
        let conn = database.connection().unwrap();
        conn.execute(
            "INSERT INTO memories (
                id, created_at, updated_at, memory_type, scope,
                user_id, group_id, content, source_text,
                scope_type, scope_id, created_by_user_id
             ) VALUES
                ('personal-id', '2026-01-01T00:00:00+08:00', NULL, 'note', 'general', 'u1', NULL, '旧个人', 'seed', 'personal', 'u1', 'u1'),
                ('group-id', '2026-01-01T00:00:01+08:00', NULL, 'note', 'general', NULL, 'g1', '旧群', 'seed', 'group', 'g1', NULL),
                ('unknown-id', '2026-01-01T00:00:02+08:00', NULL, 'note', 'general', NULL, NULL, '未知', 'seed', 'legacy_unassigned', NULL, NULL)",
            [],
        )
        .unwrap();
    }

    let store = MemoryStore::new(SqliteDatabase::open(&path, MEMORY_MIGRATIONS).unwrap());
    let personal = store.get("personal-id").unwrap();
    assert_eq!(personal.scope_type, "personal");
    assert_eq!(personal.memory_kind, MemoryKind::Personal);
    assert_eq!(personal.status, MemoryStatus::Active);
    assert_eq!(personal.scope_id.as_deref(), Some("u1"));
    assert_eq!(personal.created_by_user_id.as_deref(), Some("u1"));

    let group = store.get("group-id").unwrap();
    assert_eq!(group.scope_type, "group");
    assert_eq!(group.memory_kind, MemoryKind::Group);
    assert_eq!(group.scope_id.as_deref(), Some("g1"));
    assert_eq!(group.created_by_user_id, None);
    let conn = store.connection().unwrap();
    let unknown = get_by_id_unlocked(&conn, "unknown-id").unwrap().unwrap();
    assert_eq!(unknown.memory_kind, MemoryKind::LegacyUnassigned);
    assert_eq!(unknown.status, MemoryStatus::Archived);
    assert!(
        store
            .list_scoped(ScopedMemoryQuery {
                scope_type: MemoryScopeType::Personal,
                scope_id: "u1".to_owned(),
                limit: Some(10),
                q: None,
                scope: None,
                memory_type: None,
            })
            .unwrap()
            .iter()
            .all(|record| record.id != unknown.id)
    );

    drop(conn);
    drop(store);
    let reopened = MemoryStore::new(SqliteDatabase::open(&path, MEMORY_MIGRATIONS).unwrap());
    assert_eq!(
        reopened.get("personal-id").unwrap().memory_kind,
        MemoryKind::Personal
    );
    assert_eq!(
        reopened.get("group-id").unwrap().memory_kind,
        MemoryKind::Group
    );
}
