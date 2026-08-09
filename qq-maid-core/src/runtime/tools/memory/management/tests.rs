use super::{
    refs::{memory_ref_for, resolved_target, validate_ref},
    types::{MEMORY_REF_PREFIX, TARGET_REF_PREFIX},
    *,
};

use crate::{
    identity::conversation_scope_key,
    runtime::tools::memory::{
        MemoryKind, MemoryTarget, MemoryVisibility, storage::CreateMemoryRequest,
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
};

fn test_service() -> (MemoryManagementService, MemoryStore) {
    let database = SqliteDatabase::open_temp("memory-management-service", APP_MIGRATIONS).unwrap();
    let store = MemoryStore::new(database);
    (MemoryManagementService::new(store.clone()), store)
}

fn personal_scope(user: &str) -> String {
    conversation_scope_key("qq_official", Some("bot-a"), "private", user)
}

fn group_scope(group: &str) -> String {
    conversation_scope_key("qq_official", Some("bot-a"), "group", group)
}

fn seed(store: &MemoryStore, target: MemoryTarget, content: &str) -> MemoryRecord {
    let visibility = match target.memory_kind() {
        MemoryKind::Personal => MemoryVisibility::Private,
        MemoryKind::Group | MemoryKind::GroupProfile => MemoryVisibility::GroupMembers,
        MemoryKind::LegacyUnassigned => MemoryVisibility::Private,
    };
    store
        .persist_v3(PersistMemoryRequest {
            target,
            created_by_user_id: None,
            content: content.to_owned(),
            source_text: "raw source must not be searched".to_owned(),
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
        .unwrap()
        .record
}

#[test]
fn opaque_refs_hide_identity_and_reject_raw_ids() {
    let target = MemoryTarget::personal(personal_scope("user-a"));
    let resolved = resolved_target(target).unwrap();
    assert!(resolved.summary.target_ref.starts_with(TARGET_REF_PREFIX));
    assert!(!resolved.summary.target_ref.contains("user-a"));
    assert!(validate_ref(&resolved.summary.target_ref, TARGET_REF_PREFIX).is_ok());
    assert!(validate_ref("u1", MEMORY_REF_PREFIX).is_err());
}

#[test]
fn create_update_archive_restore_use_monotonic_revision_and_cas() {
    let (service, store) = test_service();
    let target = MemoryTarget::personal(personal_scope("user-a"));
    let seeded = seed(&store, target, "第一条");
    let target_ref = service
        .targets(MemoryTargetFilter::default(), 20, 0)
        .unwrap()
        .items[0]
        .target_ref
        .clone();
    let created = service
        .create(MemoryCreateInput {
            target_ref: target_ref.clone(),
            content: "第二条".to_owned(),
            category: MemoryCategory::Note,
            visibility: MemoryVisibility::Private,
            pinned: false,
            attribute_key: None,
        })
        .unwrap();
    let updated = service
        .update(
            &target_ref,
            &created.memory.memory_ref,
            created.memory.version,
            MemoryUpdatePatch {
                content: Some("编辑后的第二条".to_owned()),
                ..MemoryUpdatePatch::default()
            },
        )
        .unwrap();
    assert!(updated.memory.version > created.memory.version);
    assert!(matches!(
        service.update(
            &target_ref,
            &created.memory.memory_ref,
            created.memory.version,
            MemoryUpdatePatch::default(),
        ),
        Err(MemoryManagementError::Validation(_))
    ));
    let archived = service
        .archive(
            &target_ref,
            &updated.memory.memory_ref,
            updated.memory.version,
        )
        .unwrap();
    let restored = service
        .restore(
            &target_ref,
            &archived.memory.memory_ref,
            archived.memory.version,
        )
        .unwrap();
    assert_eq!(restored.memory.status, "active");
    assert!(restored.memory.version > archived.memory.version);
    assert_eq!(seeded.revision, 1);
}

#[test]
fn clear_and_profile_disable_are_snapshot_bound_and_one_shot() {
    let (service, store) = test_service();
    let target = MemoryTarget::personal(personal_scope("user-a"));
    seed(&store, target, "清空前");
    let target_ref = service
        .targets(MemoryTargetFilter::default(), 20, 0)
        .unwrap()
        .items[0]
        .target_ref
        .clone();
    let actor = ManagementActor {
        admin_id: 1,
        session_digest: [1; 32],
    };
    let prepared = service.prepare(actor, "clear_target", &target_ref).unwrap();
    service
        .create(MemoryCreateInput {
            target_ref: target_ref.clone(),
            content: "确认后新增".to_owned(),
            category: MemoryCategory::Note,
            visibility: MemoryVisibility::Private,
            pinned: false,
            attribute_key: None,
        })
        .unwrap();
    assert!(matches!(
        service.commit(
            actor,
            "clear_target",
            &target_ref,
            &prepared.confirmation_token
        ),
        Err(MemoryManagementError::Conflict(_))
    ));
    let prepared = service.prepare(actor, "clear_target", &target_ref).unwrap();
    let committed = service
        .commit(
            actor,
            "clear_target",
            &target_ref,
            &prepared.confirmation_token,
        )
        .unwrap();
    assert_eq!(committed.affected_count, 2);
    assert!(matches!(
        service.commit(
            actor,
            "clear_target",
            &target_ref,
            &prepared.confirmation_token
        ),
        Err(MemoryManagementError::NotFound)
    ));

    let profile = MemoryTarget::group_profile(group_scope("group-a"), personal_scope("user-a"));
    seed(&store, profile, "群画像");
    let profile_ref = service
        .targets(MemoryTargetFilter::default(), 20, 0)
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.scope == "group_profile")
        .unwrap()
        .target_ref;
    let profile_actor = ManagementActor {
        admin_id: 1,
        session_digest: [2; 32],
    };
    let prepared = service
        .prepare(profile_actor, "disable_group_profile", &profile_ref)
        .unwrap();
    assert!(matches!(
        service.commit(
            ManagementActor {
                admin_id: 2,
                session_digest: [2; 32],
            },
            "disable_group_profile",
            &profile_ref,
            &prepared.confirmation_token,
        ),
        Err(MemoryManagementError::PermissionDenied)
    ));
    let committed = service
        .commit(
            profile_actor,
            "disable_group_profile",
            &profile_ref,
            &prepared.confirmation_token,
        )
        .unwrap();
    assert_eq!(committed.affected_count, 1);
}

#[test]
fn like_search_is_literal_and_ignores_source_text() {
    let (service, store) = test_service();
    seed(
        &store,
        MemoryTarget::personal(personal_scope("user-a")),
        "中文 % 字面 _ 反斜杠 \\",
    );
    let target_ref = service
        .targets(MemoryTargetFilter::default(), 20, 0)
        .unwrap()
        .items[0]
        .target_ref
        .clone();
    let page = service
        .list(
            MemoryListFilter {
                target_ref: Some(target_ref.clone()),
                keyword: Some("%".to_owned()),
                ..MemoryListFilter::default()
            },
            20,
            0,
        )
        .unwrap();
    assert_eq!(page.total_count, 1);
    for keyword in ["_", "\\"] {
        let page = service
            .list(
                MemoryListFilter {
                    target_ref: Some(target_ref.clone()),
                    keyword: Some(keyword.to_owned()),
                    ..MemoryListFilter::default()
                },
                20,
                0,
            )
            .unwrap();
        assert_eq!(page.total_count, 1, "keyword {keyword:?}");
    }
    let page = service
        .list(
            MemoryListFilter {
                target_ref: Some(target_ref.clone()),
                keyword: Some("   ".to_owned()),
                ..MemoryListFilter::default()
            },
            20,
            0,
        )
        .unwrap();
    assert_eq!(page.total_count, 1);
    let page = service
        .list(
            MemoryListFilter {
                target_ref: Some(target_ref),
                keyword: Some("raw source".to_owned()),
                ..MemoryListFilter::default()
            },
            20,
            0,
        )
        .unwrap();
    assert_eq!(page.total_count, 0);
}

#[test]
fn target_discovery_isolates_platform_account_group_and_subject() {
    let (service, store) = test_service();
    let targets = [
        MemoryTarget::personal(personal_scope("user-a")),
        MemoryTarget::personal(conversation_scope_key(
            "onebot",
            Some("bot-b"),
            "private",
            "user-a",
        )),
        MemoryTarget::group(group_scope("group-a")),
        MemoryTarget::group(conversation_scope_key(
            "onebot",
            Some("bot-a"),
            "group",
            "group-a",
        )),
        MemoryTarget::group_profile(group_scope("group-a"), personal_scope("user-a")),
        MemoryTarget::group_profile(group_scope("group-a"), personal_scope("user-b")),
    ];
    for (index, target) in targets.into_iter().enumerate() {
        seed(&store, target, &format!("target-{index}"));
    }
    store
        .create(CreateMemoryRequest {
            user_id: None,
            group_id: None,
            content: "legacy must stay hidden".to_owned(),
            source_text: "legacy source".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let page = service
        .targets(MemoryTargetFilter::default(), 100, 0)
        .unwrap();
    assert_eq!(page.total_count, 6);
    assert!(
        page.items
            .iter()
            .all(|item| item.scope != "legacy_unassigned")
    );
    assert!(
        page.items
            .iter()
            .all(|item| !item.target_ref.contains("user-a"))
    );

    let qq_account = page
        .items
        .iter()
        .find(|item| item.platform == "qq_official" && item.scope == "personal")
        .unwrap()
        .account_ref
        .clone();
    let qq_page = service
        .list(
            MemoryListFilter {
                account_ref: Some(qq_account),
                ..MemoryListFilter::default()
            },
            100,
            0,
        )
        .unwrap();
    assert_eq!(qq_page.total_count, 4);
    assert!(
        qq_page
            .items
            .iter()
            .all(|item| item.target.platform == "qq_official")
    );

    let profile_targets = page
        .items
        .iter()
        .filter(|item| item.scope == "group_profile")
        .collect::<Vec<_>>();
    assert_eq!(profile_targets.len(), 2);
    assert_ne!(
        profile_targets[0].subject_ref,
        profile_targets[1].subject_ref
    );
    let subject_page = service
        .list(
            MemoryListFilter {
                subject_ref: profile_targets[0].subject_ref.clone(),
                ..MemoryListFilter::default()
            },
            100,
            0,
        )
        .unwrap();
    assert_eq!(subject_page.total_count, 1);
}

#[test]
fn same_version_concurrent_updates_have_one_winner() {
    let (service, store) = test_service();
    let record = seed(
        &store,
        MemoryTarget::personal(personal_scope("concurrent-user")),
        "concurrent source",
    );
    let target_ref = service
        .targets(MemoryTargetFilter::default(), 20, 0)
        .unwrap()
        .items[0]
        .target_ref
        .clone();
    let memory_ref = memory_ref_for(&target_ref, &record.id);
    let first = service.clone();
    let second = service;
    let first_target = target_ref.clone();
    let second_target = target_ref;
    let first_memory = memory_ref.clone();
    let second_memory = memory_ref;
    let first = std::thread::spawn(move || {
        first.update(
            &first_target,
            &first_memory,
            1,
            MemoryUpdatePatch {
                content: Some("并发写入 A".to_owned()),
                ..MemoryUpdatePatch::default()
            },
        )
    });
    let second = std::thread::spawn(move || {
        second.update(
            &second_target,
            &second_memory,
            1,
            MemoryUpdatePatch {
                content: Some("并发写入 B".to_owned()),
                ..MemoryUpdatePatch::default()
            },
        )
    });
    let results = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(MemoryManagementError::Conflict(_))))
            .count(),
        1
    );
}

#[test]
fn expired_confirmation_is_removed_without_exposing_snapshot() {
    let (service, store) = test_service();
    seed(
        &store,
        MemoryTarget::personal(personal_scope("expired-user")),
        "不可进入错误响应的正文",
    );
    let target_ref = service
        .targets(MemoryTargetFilter::default(), 20, 0)
        .unwrap()
        .items[0]
        .target_ref
        .clone();
    let actor = ManagementActor {
        admin_id: 9,
        session_digest: [9; 32],
    };
    let prepared = service.prepare(actor, "clear_target", &target_ref).unwrap();
    let digest = super::refs::token_digest(&prepared.confirmation_token);
    service
        .confirmations
        .lock()
        .unwrap()
        .get_mut(&digest)
        .unwrap()
        .expires_at = 0;
    let error = service
        .commit(
            actor,
            "clear_target",
            &target_ref,
            &prepared.confirmation_token,
        )
        .unwrap_err();
    assert!(matches!(error, MemoryManagementError::NotFound));
    assert!(!error.message().contains("不可进入错误响应的正文"));
}

#[allow(dead_code)]
fn _memory_ref_is_target_bound(target_ref: &str, id: &str) -> String {
    memory_ref_for(target_ref, id)
}
