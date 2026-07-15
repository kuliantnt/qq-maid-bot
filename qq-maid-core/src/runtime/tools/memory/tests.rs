use super::*;
use crate::storage::database::SqliteDatabase;

const ACCOUNT: &str = "bot-a";

fn personal(user: &str) -> String {
    format!("platform:qq_official:account:{ACCOUNT}:private:{user}")
}

fn group(group: &str) -> String {
    format!("platform:qq_official:account:{ACCOUNT}:group:{group}")
}

fn actor(user: &str, group_id: Option<&str>, admin: bool) -> MemoryActor {
    MemoryActor {
        user_id: user.to_owned(),
        personal_scope_id: personal(user),
        group_scope_id: group_id.map(group),
        can_manage_group_memory: admin,
    }
}

fn operations() -> MemoryOperations {
    operations_and_store().0
}

fn operations_and_store() -> (MemoryOperations, storage::MemoryStore) {
    let database =
        SqliteDatabase::open_temp("qq-maid-memory-domain", storage::MEMORY_MIGRATIONS).unwrap();
    let store = storage::MemoryStore::new(database);
    (MemoryOperations::new(store.clone()), store)
}

fn save_request(
    actor: MemoryActor,
    target: storage::MemoryTarget,
    content: &str,
    attribute_key: Option<&str>,
) -> SaveMemoryRequest {
    let visibility = match target.memory_kind() {
        storage::MemoryKind::Personal => storage::MemoryVisibility::Private,
        storage::MemoryKind::GroupProfile | storage::MemoryKind::Group => {
            storage::MemoryVisibility::GroupMembers
        }
        storage::MemoryKind::LegacyUnassigned => storage::MemoryVisibility::Private,
    };
    SaveMemoryRequest {
        actor,
        target,
        content: content.to_owned(),
        source_text: format!("明确记忆：{content}"),
        category: storage::MemoryCategory::Note,
        legacy_scope: "general".to_owned(),
        visibility,
        source_type: storage::MemorySourceType::UserConfirmed,
        source_ref: Some("command:memory".to_owned()),
        confirmed_at: None,
        pinned: false,
        attribute_key: attribute_key.map(str::to_owned),
        relation_subject_id: None,
        relation_object_id: None,
    }
}

#[test]
fn exact_targets_keep_personal_profiles_and_groups_isolated() {
    let ops = operations();
    let u1_a = actor("u1", Some("g-a"), false);
    let u1_b = actor("u1", Some("g-b"), false);
    let admin_a = actor("admin", Some("g-a"), true);

    ops.save(save_request(
        u1_a.clone(),
        storage::MemoryTarget::personal(personal("u1")),
        "个人偏好",
        None,
    ))
    .unwrap();
    ops.save(save_request(
        u1_a.clone(),
        storage::MemoryTarget::group_profile(group("g-a"), personal("u1")),
        "A 群昵称",
        Some("nickname"),
    ))
    .unwrap();
    ops.save(save_request(
        u1_b.clone(),
        storage::MemoryTarget::group_profile(group("g-b"), personal("u1")),
        "B 群昵称",
        Some("nickname"),
    ))
    .unwrap();
    ops.save(save_request(
        admin_a.clone(),
        storage::MemoryTarget::group(group("g-a")),
        "A 群群规",
        Some("rules"),
    ))
    .unwrap();

    let personal_rows = ops
        .list(
            &u1_a,
            storage::MemoryQuery::active(storage::MemoryTarget::personal(personal("u1"))),
        )
        .unwrap();
    let profile_a = ops
        .list(
            &u1_a,
            storage::MemoryQuery::active(storage::MemoryTarget::group_profile(
                group("g-a"),
                personal("u1"),
            )),
        )
        .unwrap();
    let profile_b = ops
        .list(
            &u1_b,
            storage::MemoryQuery::active(storage::MemoryTarget::group_profile(
                group("g-b"),
                personal("u1"),
            )),
        )
        .unwrap();

    assert_eq!(personal_rows[0].content, "个人偏好");
    assert_eq!(profile_a[0].content, "A 群昵称");
    assert_eq!(profile_b[0].content, "B 群昵称");
    assert_ne!(profile_a[0].scope_id, profile_b[0].scope_id);

    // Phase B/C 接入前，旧 group list/chat 只看群组公共记忆，不能混入任何成员画像。
    let legacy_group = ops
        .list_scoped(
            &admin_a,
            storage::ScopedMemoryQuery {
                scope_type: storage::MemoryScopeType::Group,
                scope_id: group("g-a"),
                limit: Some(20),
                q: None,
                scope: None,
                memory_type: None,
            },
        )
        .unwrap();
    let chat_rows = ops
        .list_accessible_for_context(Some(&personal("u1")), Some(&group("g-a")), 20)
        .unwrap();
    assert_eq!(legacy_group.len(), 1);
    assert_eq!(legacy_group[0].content, "A 群群规");
    assert!(
        chat_rows
            .iter()
            .all(|row| row.memory_kind != storage::MemoryKind::GroupProfile)
    );
}

#[test]
fn permissions_use_stable_actor_scopes_and_do_not_leak_existence() {
    let ops = operations();
    let u1 = actor("u1", Some("g-a"), false);
    let u2_admin = actor("u2", Some("g-a"), true);
    let personal_target = storage::MemoryTarget::personal(personal("u1"));
    let created = ops
        .save(save_request(
            u1.clone(),
            personal_target.clone(),
            "私密记忆",
            None,
        ))
        .unwrap();

    let existing = ops
        .archive(&u2_admin, &personal_target, &created.memory.id)
        .unwrap_err();
    let missing = ops
        .archive(&u2_admin, &personal_target, "missing")
        .unwrap_err();
    assert_eq!(existing.code(), "forbidden");
    assert_eq!(existing.message(), missing.message());

    let same_raw_user_other_account = MemoryActor {
        user_id: "u1".to_owned(),
        personal_scope_id: "platform:qq_official:account:bot-b:private:u1".to_owned(),
        group_scope_id: Some("platform:qq_official:account:bot-b:group:g-a".to_owned()),
        can_manage_group_memory: true,
    };
    assert_eq!(
        ops.archive(
            &same_raw_user_other_account,
            &personal_target,
            &created.memory.id,
        )
        .unwrap_err()
        .code(),
        "forbidden"
    );

    let other_profile = storage::MemoryTarget::group_profile(group("g-a"), personal("u1"));
    assert_eq!(
        ops.clear(&u2_admin, &other_profile).unwrap_err().code(),
        "forbidden"
    );
    assert_eq!(
        ops.save(save_request(
            u1,
            storage::MemoryTarget::group(group("g-a")),
            "普通成员不能写群记忆",
            None,
        ))
        .unwrap_err()
        .code(),
        "forbidden"
    );
    assert!(
        ops.save(save_request(
            u2_admin,
            storage::MemoryTarget::group(group("g-a")),
            "管理员可写群记忆",
            None,
        ))
        .is_ok()
    );
}

#[test]
fn conflicting_attribute_archives_only_same_target_and_relation_pair() {
    let ops = operations();
    let u1 = actor("u1", Some("g-a"), false);
    let profile = storage::MemoryTarget::group_profile(group("g-a"), personal("u1"));
    let first = ops
        .save(save_request(
            u1.clone(),
            profile.clone(),
            "昵称雪糕",
            Some("Nickname"),
        ))
        .unwrap();
    let second = ops
        .save(save_request(
            u1.clone(),
            profile.clone(),
            "昵称棒冰",
            Some("nickname"),
        ))
        .unwrap();
    assert_eq!(second.archived_ids, vec![first.memory.id.clone()]);

    let active = ops
        .list(&u1, storage::MemoryQuery::active(profile.clone()))
        .unwrap();
    let mut archived_query = storage::MemoryQuery::active(profile.clone());
    archived_query.status = Some(storage::MemoryStatus::Archived);
    let archived = ops.list(&u1, archived_query).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].content, "昵称棒冰");
    assert_eq!(archived[0].id, first.memory.id);

    let mut relation_a = save_request(u1.clone(), profile.clone(), "与 u2 是队友", Some("role"));
    relation_a.category = storage::MemoryCategory::Relation;
    relation_a.relation_subject_id = Some(personal("u1"));
    relation_a.relation_object_id = Some(personal("u2"));
    ops.save(relation_a).unwrap();
    let mut relation_b = save_request(u1.clone(), profile.clone(), "与 u3 是队友", Some("role"));
    relation_b.category = storage::MemoryCategory::Relation;
    relation_b.relation_subject_id = Some(personal("u1"));
    relation_b.relation_object_id = Some(personal("u3"));
    let relation_b = ops.save(relation_b).unwrap();
    assert!(relation_b.archived_ids.is_empty());
    assert_eq!(
        ops.list(&u1, storage::MemoryQuery::active(profile))
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn profile_opt_out_archives_atomically_and_blocks_create_and_replace() {
    let ops = operations();
    let u1 = actor("u1", Some("g-a"), false);
    let target = storage::MemoryTarget::group_profile(group("g-a"), personal("u1"));
    let created = ops
        .save(save_request(
            u1.clone(),
            target.clone(),
            "群昵称",
            Some("nickname"),
        ))
        .unwrap();

    let disabled = ops.set_group_profile_enabled(&u1, &target, false).unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.archived_ids, vec![created.memory.id.clone()]);
    assert_eq!(
        ops.save(save_request(
            u1.clone(),
            target.clone(),
            "新昵称",
            Some("nickname")
        ))
        .unwrap_err()
        .code(),
        "profile_opted_out"
    );
    assert_eq!(
        ops.replace(
            &created.memory.id,
            save_request(u1.clone(), target.clone(), "替换昵称", Some("nickname")),
        )
        .unwrap_err()
        .code(),
        "profile_opted_out"
    );

    ops.set_group_profile_enabled(&u1, &target, true).unwrap();
    assert!(
        ops.save(save_request(
            u1,
            target,
            "重新允许后的昵称",
            Some("nickname")
        ))
        .is_ok()
    );
}

#[test]
fn v3_conflict_and_replace_roll_back_when_insert_fails() {
    let (ops, store) = operations_and_store();
    let u1 = actor("u1", Some("g-a"), false);
    let target = storage::MemoryTarget::group_profile(group("g-a"), personal("u1"));
    let first = ops
        .save(save_request(
            u1.clone(),
            target.clone(),
            "旧昵称",
            Some("nickname"),
        ))
        .unwrap();
    store.abort_memory_insert_for_test().unwrap();

    assert_eq!(
        ops.save(save_request(
            u1.clone(),
            target.clone(),
            "新昵称",
            Some("nickname"),
        ))
        .unwrap_err()
        .code(),
        "io_error"
    );
    assert_eq!(
        ops.replace(
            &first.memory.id,
            save_request(u1.clone(), target.clone(), "替换昵称", Some("nickname"),),
        )
        .unwrap_err()
        .code(),
        "io_error"
    );
    let active = ops.list(&u1, storage::MemoryQuery::active(target)).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, first.memory.id);
    assert_eq!(active[0].content, "旧昵称");
}

#[test]
fn profile_preference_rolls_back_when_archive_step_fails() {
    let (ops, store) = operations_and_store();
    let u1 = actor("u1", Some("g-a"), false);
    let target = storage::MemoryTarget::group_profile(group("g-a"), personal("u1"));
    ops.save(save_request(
        u1.clone(),
        target.clone(),
        "现有画像",
        Some("nickname"),
    ))
    .unwrap();
    store.abort_memory_archive_for_test().unwrap();

    assert_eq!(
        ops.set_group_profile_enabled(&u1, &target, false)
            .unwrap_err()
            .code(),
        "io_error"
    );
    // 使用不同属性不会触发归档；能继续写入证明 preference upsert 已随事务回滚。
    assert!(
        ops.save(save_request(u1, target, "仍允许写入", Some("persona")))
            .is_ok()
    );
}

#[test]
fn group_public_write_matrix_is_admin_only() {
    let ops = operations();
    let member = actor("u1", Some("g-a"), false);
    let admin = actor("admin", Some("g-a"), true);
    let target = storage::MemoryTarget::group(group("g-a"));
    let created = ops
        .save(save_request(
            admin.clone(),
            target.clone(),
            "旧群规",
            Some("rules"),
        ))
        .unwrap();

    assert_eq!(
        ops.replace(
            &created.memory.id,
            save_request(member.clone(), target.clone(), "越权替换", Some("rules")),
        )
        .unwrap_err()
        .code(),
        "forbidden"
    );
    assert_eq!(
        ops.archive(&member, &target, &created.memory.id)
            .unwrap_err()
            .code(),
        "forbidden"
    );
    assert_eq!(
        ops.delete(&member, &target, &created.memory.id)
            .unwrap_err()
            .code(),
        "forbidden"
    );
    assert_eq!(ops.clear(&member, &target).unwrap_err().code(), "forbidden");

    let replaced = ops
        .replace(
            &created.memory.id,
            save_request(admin.clone(), target.clone(), "新群规", Some("rules")),
        )
        .unwrap();
    assert!(replaced.archived_ids.contains(&created.memory.id));
    assert_eq!(ops.clear(&admin, &target).unwrap().count, 1);
}

#[test]
fn source_reference_and_attribute_key_reject_unsafe_values() {
    let ops = operations();
    let u1 = actor("u1", None, false);
    let target = storage::MemoryTarget::personal(personal("u1"));

    for source_ref in [
        "Authorization: Bearer sk-secret".to_owned(),
        "message\nraw-envelope".to_owned(),
        "x".repeat(257),
    ] {
        let mut req = save_request(u1.clone(), target.clone(), "安全内容", None);
        req.source_ref = Some(source_ref);
        assert_eq!(ops.save(req).unwrap_err().code(), "bad_request");
    }
    let mut invalid_attribute = save_request(u1, target, "安全内容", Some("昵称 空格"));
    invalid_attribute.source_ref = Some("command:memory".to_owned());
    assert_eq!(
        ops.save(invalid_attribute).unwrap_err().code(),
        "bad_request"
    );
}

#[test]
fn clear_and_delete_affect_only_authorized_target() {
    let ops = operations();
    let u1_a = actor("u1", Some("g-a"), false);
    let u1_b = actor("u1", Some("g-b"), false);
    let target_a = storage::MemoryTarget::group_profile(group("g-a"), personal("u1"));
    let target_b = storage::MemoryTarget::group_profile(group("g-b"), personal("u1"));
    let a = ops
        .save(save_request(u1_a.clone(), target_a.clone(), "A", None))
        .unwrap();
    ops.save(save_request(u1_b.clone(), target_b.clone(), "B", None))
        .unwrap();

    let cleared = ops.clear(&u1_a, &target_a).unwrap();
    assert_eq!(cleared.count, 1);
    assert_eq!(cleared.affected_ids, vec![a.memory.id]);
    assert!(
        ops.list(&u1_a, storage::MemoryQuery::active(target_a))
            .unwrap()
            .is_empty()
    );
    let b_rows = ops
        .list(&u1_b, storage::MemoryQuery::active(target_b.clone()))
        .unwrap();
    assert_eq!(b_rows.len(), 1);
    let deleted = ops.delete(&u1_b, &target_b, &b_rows[0].id).unwrap();
    assert_eq!(deleted.count, 1);
    assert!(
        ops.list(&u1_b, storage::MemoryQuery::active(target_b))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn draft_rules_strip_markdown_and_reject_sensitive_content() {
    assert_eq!(
        parse_valid_memory_draft_content(r#"{"content":"**记忆草稿：** 回复简短。"}"#).as_deref(),
        Some("回复简短")
    );
    assert!(parse_valid_memory_draft_content(r#"{"content":"token=sk-secret"}"#).is_none());
}
