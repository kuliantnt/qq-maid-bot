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
            &MemoryActor {
                user_id: "u1".to_owned(),
            },
            UpdateMemoryRequest {
                content: Some("个人记忆已更新".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.content, "个人记忆已更新");
    assert!(
        store
            .delete_scoped(
                MemoryScopeType::Personal,
                "u1",
                &group.id[..8],
                &MemoryActor {
                    user_id: "u1".to_owned()
                },
            )
            .is_err()
    );
}

#[test]
fn group_memory_creator_can_manage_but_others_cannot() {
    let store = test_store();
    let group = create_scoped_memory(&store, MemoryScopeType::Group, "g1", "u1", "群规则");

    assert_eq!(
        store
            .update_scoped(
                MemoryScopeType::Group,
                "g1",
                &group.id,
                &MemoryActor {
                    user_id: "u2".to_owned()
                },
                UpdateMemoryRequest {
                    content: Some("别人修改".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap_err()
            .code(),
        "forbidden"
    );

    let updated = store
        .update_scoped(
            MemoryScopeType::Group,
            "g1",
            &group.id,
            &MemoryActor {
                user_id: "u1".to_owned(),
            },
            UpdateMemoryRequest {
                content: Some("创建者修改".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.content, "创建者修改");
}

#[test]
fn context_merge_keeps_global_row_order_without_fixed_quota() {
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
        create_scoped_memory(
            &store,
            MemoryScopeType::Personal,
            "u1",
            "u1",
            &format!("较新的个人记忆 {index}"),
        );
    }

    let records = store
        .list_accessible_for_context(Some("u1"), Some("g1"), 12)
        .unwrap();

    assert_eq!(records.len(), 12);
    assert!(
        records
            .iter()
            .all(|record| record.content.contains("个人记忆"))
    );
}

#[test]
fn legacy_v1_database_is_backfilled_conservatively() {
    let path = std::env::temp_dir().join(format!("qq-maid-memory-migration-{}.db", Uuid::new_v4()));
    {
        let database = SqliteDatabase::open(&path, &[MEMORY_SCHEMA_V1]).unwrap();
        let conn = database.connection().unwrap();
        conn.execute(
            "INSERT INTO memories (
                id, created_at, updated_at, memory_type, scope,
                user_id, group_id, content, source_text
             ) VALUES
                ('personal-id', '2026-01-01T00:00:00+08:00', NULL, 'note', 'general', 'u1', NULL, '旧个人', 'seed'),
                ('group-id', '2026-01-01T00:00:01+08:00', NULL, 'note', 'general', NULL, 'g1', '旧群', 'seed'),
                ('unknown-id', '2026-01-01T00:00:02+08:00', NULL, 'note', 'general', NULL, NULL, '未知', 'seed')",
            [],
        )
        .unwrap();
    }

    let store = MemoryStore::new(SqliteDatabase::open(&path, MEMORY_MIGRATIONS).unwrap());
    let personal = store.get("personal-id").unwrap();
    assert_eq!(personal.scope_type, "personal");
    assert_eq!(personal.scope_id.as_deref(), Some("u1"));
    assert_eq!(personal.created_by_user_id.as_deref(), Some("u1"));

    let group = store.get("group-id").unwrap();
    assert_eq!(group.scope_type, "group");
    assert_eq!(group.scope_id.as_deref(), Some("g1"));
    assert_eq!(group.created_by_user_id, None);
    assert_eq!(
        store
            .update_scoped(
                MemoryScopeType::Group,
                "g1",
                "group-id",
                &MemoryActor {
                    user_id: "u1".to_owned()
                },
                UpdateMemoryRequest {
                    content: Some("不能修改旧群".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap_err()
            .code(),
        "forbidden"
    );

    let unknown = store.get("unknown-id").unwrap();
    assert_eq!(unknown.scope_type, "legacy_unassigned");
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
}
