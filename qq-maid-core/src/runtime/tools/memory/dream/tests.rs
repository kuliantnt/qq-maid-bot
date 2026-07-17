use std::{sync::Arc, time::Duration};

use crate::{
    error::LlmError,
    runtime::{
        respond::tests::support::MockProvider,
        session::{SessionMeta, SessionStore, SessionTurnActor},
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
};

use super::super::{
    MemoryKind, MemoryQuery, MemorySourceType, MemoryStatus, MemoryVisibility, SaveMemoryRequest,
};
use super::*;

fn test_config() -> MemoryDreamConfig {
    MemoryDreamConfig {
        enabled: true,
        min_interval_seconds: 0,
        min_new_sessions: 1,
        max_sessions: 20,
        max_input_chars: 32_000,
        max_output_memories: 8,
    }
}

fn test_stores() -> (MemoryStore, SessionStore) {
    let database =
        SqliteDatabase::open_temp("memory-dream", APP_MIGRATIONS).expect("open database");
    (
        MemoryStore::new(database.clone()),
        SessionStore::new(database),
    )
}

fn private_actor(user: &str) -> MemoryActor {
    MemoryActor {
        user_id: user.to_owned(),
        personal_scope_id: user.to_owned(),
        group_scope_id: None,
        can_manage_group_memory: false,
    }
}

fn private_context(user: &str) -> MemoryDreamContext {
    MemoryDreamContext {
        actor: private_actor(user),
        target: MemoryTarget::personal(user),
        conversation_scope_key: format!("private:{user}"),
        actor_ref: None,
        model: Some("mock-dream".to_owned()),
    }
}

fn add_private_session(store: &SessionStore, user: &str, text: &str) {
    let _ = add_private_session_with_id(store, user, text);
}

fn add_private_session_with_id(store: &SessionStore, user: &str, text: &str) -> String {
    let meta = SessionMeta::new(
        format!("private:{user}"),
        Some(user.to_owned()),
        None,
        None,
        None,
        "test",
    );
    let mut session = store.create(&meta, "", false).expect("create session");
    store
        .append_exchange(&mut session, text, "assistant reply not used by Dream")
        .expect("append exchange");
    session.session_id
}

fn group_context(group: &str, user: &str) -> MemoryDreamContext {
    let scope_key = format!("group:{group}");
    let actor_ref = SessionTurnActor::actor_ref_for_user(&scope_key, user).unwrap();
    MemoryDreamContext {
        actor: MemoryActor {
            user_id: user.to_owned(),
            personal_scope_id: user.to_owned(),
            group_scope_id: Some(group.to_owned()),
            can_manage_group_memory: false,
        },
        target: MemoryTarget::group_profile(group, user),
        conversation_scope_key: scope_key,
        actor_ref: Some(actor_ref),
        model: Some("mock-dream".to_owned()),
    }
}

fn add_group_session(store: &SessionStore, group: &str, turns: &[(&str, &str)]) {
    let scope_key = format!("group:{group}");
    let meta = SessionMeta::new(
        scope_key.clone(),
        Some("request-user".to_owned()),
        Some(group.to_owned()),
        None,
        None,
        "test",
    );
    let mut session = store.create(&meta, "", false).expect("create session");
    for (user, text) in turns {
        session.set_turn_actor(Some(SessionTurnActor {
            actor_ref: SessionTurnActor::actor_ref_for_user(&scope_key, user),
            display_name: None,
            display_name_source: None,
            group_member_role: None,
            identity_source: None,
        }));
        store
            .append_exchange(&mut session, text, "assistant reply not used by Dream")
            .expect("append group exchange");
    }
}

fn active_memories(
    store: &MemoryStore,
    actor: &MemoryActor,
    target: MemoryTarget,
) -> Vec<super::super::MemoryRecord> {
    MemoryOperations::new(store.clone())
        .list(actor, MemoryQuery::active(target))
        .expect("list memories")
}

fn worker(
    store: &MemoryStore,
    provider: MockProvider,
    config: MemoryDreamConfig,
) -> MemoryDreamWorker {
    MemoryDreamWorker::new(Arc::new(provider), store.clone(), config)
}

#[test]
fn no_reply_validation_is_strict_but_format_tolerant() {
    assert!(is_no_reply("NO_REPLY"));
    assert!(is_no_reply(" no-reply \n"));
    assert!(!is_no_reply("NO_REPLY because nothing was found"));
}

#[test]
fn model_output_rejects_permission_fields() {
    let raw = r#"{"memories":[],"scope_id":"forged"}"#;
    assert_eq!(
        parse_model_output(raw, 2).unwrap_err(),
        "dream_output_invalid_json"
    );
}

#[tokio::test]
async fn private_session_dream_writes_system_derived_personal_memory() {
    let (store, sessions) = test_stores();
    add_private_session(&sessions, "u1", "我长期偏好简洁的中文回答");
    let provider = MockProvider::with_dream_replies(vec![Ok(
        r#"{"memories":[{"content":"用户长期偏好简洁的中文回答","category":"preference","attribute_key":null,"worth_saving":true}]}"#,
    )]);
    let stats = worker(&store, provider, test_config())
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(stats.inserted_count, 1);
    let records = active_memories(&store, &private_actor("u1"), MemoryTarget::personal("u1"));
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].source_type, MemorySourceType::SystemDerived);
    assert_eq!(records[0].memory_kind, MemoryKind::Personal);
    assert_eq!(records[0].visibility, MemoryVisibility::Private);
}

#[tokio::test]
async fn group_dream_only_writes_current_members_profile_and_keeps_groups_isolated() {
    let (store, sessions) = test_stores();
    add_group_session(
        &sessions,
        "g1",
        &[("u1", "我一直喜欢红茶"), ("u2", "我一直喜欢咖啡")],
    );
    add_group_session(&sessions, "g2", &[("u1", "我在这个群喜欢绿茶")]);
    let provider = MockProvider::with_dream_replies(vec![
        Ok(
            r#"{"memories":[{"content":"当前成员长期喜欢红茶","category":"preference","attribute_key":"drink","worth_saving":true}]}"#,
        ),
        Ok(
            r#"{"memories":[{"content":"当前成员长期喜欢绿茶","category":"preference","attribute_key":"drink","worth_saving":true}]}"#,
        ),
    ]);
    let observable = provider.clone();
    let worker = worker(&store, provider, test_config());
    let g1 = group_context("g1", "u1");
    let g2 = group_context("g2", "u1");
    worker.run_once(g1.clone()).await.unwrap().unwrap();
    worker.run_once(g2.clone()).await.unwrap().unwrap();

    let g1_records = active_memories(&store, &g1.actor, g1.target.clone());
    let g2_records = active_memories(&store, &g2.actor, g2.target.clone());
    assert_eq!(g1_records[0].content, "当前成员长期喜欢红茶");
    assert_eq!(g2_records[0].content, "当前成员长期喜欢绿茶");
    assert_eq!(g1_records[0].memory_kind, MemoryKind::GroupProfile);
    assert_eq!(g1_records[0].subject_id.as_deref(), Some("u1"));
    assert_eq!(g1_records[0].visibility, MemoryVisibility::GroupMembers);
    let requests = observable.requests();
    let first_input = &requests[0].messages.last().unwrap().content;
    assert!(first_input.contains("我一直喜欢红茶"));
    assert!(!first_input.contains("我一直喜欢咖啡"));
    let u2 = group_context("g1", "u2");
    assert!(active_memories(&store, &u2.actor, u2.target).is_empty());
    assert!(
        MemoryOperations::new(store.clone())
            .authorize_dream_target(&g1.actor, &MemoryTarget::group("g1"))
            .is_err()
    );
}

#[tokio::test]
async fn group_profile_opt_out_skips_model_and_write() {
    let (store, sessions) = test_stores();
    add_group_session(&sessions, "g1", &[("u1", "我长期喜欢红茶")]);
    let context = group_context("g1", "u1");
    MemoryOperations::new(store.clone())
        .set_group_profile_enabled(&context.actor, &context.target, false)
        .unwrap();
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
    let observable = provider.clone();

    assert!(
        worker(&store, provider, test_config())
            .run_once(context)
            .await
            .unwrap()
            .is_none()
    );
    assert!(observable.requests().is_empty());
}

#[tokio::test]
async fn duplicate_and_user_confirmed_conflict_do_not_replace_confirmed_memory() {
    let (store, sessions) = test_stores();
    add_private_session(&sessions, "u1", "我的长期称呼偏好没有变化");
    let actor = private_actor("u1");
    let target = MemoryTarget::personal("u1");
    MemoryOperations::new(store.clone())
        .save(SaveMemoryRequest {
            actor: actor.clone(),
            target: target.clone(),
            content: "请称呼用户为小一".to_owned(),
            source_text: String::new(),
            category: MemoryCategory::Identity,
            legacy_scope: "private".to_owned(),
            visibility: MemoryVisibility::Private,
            source_type: MemorySourceType::UserConfirmed,
            source_ref: None,
            confirmed_at: None,
            pinned: false,
            attribute_key: Some("nickname".to_owned()),
            relation_subject_id: None,
            relation_object_id: None,
        })
        .unwrap();
    let provider = MockProvider::with_dream_replies(vec![Ok(
        r#"{"memories":[{"content":"请称呼用户为小一","category":"identity","attribute_key":"nickname","worth_saving":true},{"content":"请称呼用户为小二","category":"identity","attribute_key":"nickname","worth_saving":true}]}"#,
    )]);
    let stats = worker(&store, provider, test_config())
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(stats.duplicate_count, 1);
    assert_eq!(stats.conflict_count, 1);
    let records = active_memories(&store, &actor, target);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].content, "请称呼用户为小一");
    assert_eq!(records[0].status, MemoryStatus::Active);
    assert_eq!(records[0].source_type, MemorySourceType::UserConfirmed);
}

#[tokio::test]
async fn invalid_output_and_model_failure_leave_batch_retryable() {
    let (store, sessions) = test_stores();
    add_private_session(&sessions, "u1", "我长期喜欢结构化回答");
    let invalid = MockProvider::with_dream_replies(vec![Ok("not-json")]);
    assert_eq!(
        worker(&store, invalid, test_config())
            .run_once(private_context("u1"))
            .await
            .unwrap_err(),
        "dream_output_invalid_json"
    );
    let failed = MockProvider::with_dream_replies(vec![Err(LlmError::provider(
        "dream unavailable",
        "test",
    ))]);
    assert_eq!(
        worker(&store, failed, test_config())
            .run_once(private_context("u1"))
            .await
            .unwrap_err(),
        "dream_model_failed"
    );
    let retry = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
    assert!(
        worker(&store, retry, test_config())
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn no_reply_and_all_safety_filtered_candidates_advance_checkpoint() {
    let (store, sessions) = test_stores();
    add_private_session(&sessions, "u1", "我长期喜欢结构化回答");
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
    let no_reply_worker = worker(&store, provider, test_config());
    let stats = no_reply_worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();
    assert!(stats.no_reply);
    assert!(
        no_reply_worker
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .is_none()
    );

    add_private_session(&sessions, "u2", "我长期使用某个凭据");
    let filtered = MockProvider::with_dream_replies(vec![Ok(
        r#"{"memories":[{"content":"用户的 access token 是 secret-value","category":"note","attribute_key":null,"worth_saving":true}]}"#,
    )]);
    let filtered_worker = worker(&store, filtered, test_config());
    let stats = filtered_worker
        .run_once(private_context("u2"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stats.filtered_count, 1);
    assert_eq!(stats.inserted_count, 0);
    assert!(
        filtered_worker
            .run_once(private_context("u2"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn database_failure_rolls_back_memory_and_checkpoint() {
    let (store, sessions) = test_stores();
    add_private_session(&sessions, "u1", "我长期喜欢结构化回答");
    store.abort_memory_insert_for_test().unwrap();
    let output = r#"{"memories":[{"content":"用户长期喜欢结构化回答","category":"preference","attribute_key":null,"worth_saving":true}]}"#;
    let provider = MockProvider::with_dream_replies(vec![Ok(output)]);
    assert_eq!(
        worker(&store, provider, test_config())
            .run_once(private_context("u1"))
            .await
            .unwrap_err(),
        "dream_commit_failed"
    );
    assert!(active_memories(&store, &private_actor("u1"), MemoryTarget::personal("u1")).is_empty());
    let storage_context = DreamContext {
        actor_scope_id: "u1".to_owned(),
        target: MemoryTarget::personal("u1"),
        conversation_scope_key: "private:u1".to_owned(),
        actor_ref: None,
    };
    assert!(
        store
            .claim_dream(
                &storage_context,
                DreamLimits {
                    min_interval_seconds: 0,
                    min_new_sessions: 1,
                    max_sessions: 20,
                },
                unix_epoch(),
            )
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn truncated_input_processes_tail_in_later_batch() {
    let (store, sessions) = test_stores();
    add_private_session(&sessions, "u1", &"长期偏好甲".repeat(500));
    add_private_session(&sessions, "u1", "长期偏好乙");
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let observable = provider.clone();
    let mut config = test_config();
    config.max_input_chars = 1000;
    config.min_new_sessions = 2;
    let worker = worker(&store, provider, config);
    let first = worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();
    let second = worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    assert!(first.truncated);
    assert_eq!(first.input_sessions, 1);
    assert_eq!(second.input_sessions, 1);
    assert_eq!(observable.requests().len(), 2);
}

#[tokio::test]
async fn max_session_limit_never_checkpoints_across_interleaved_session_gap() {
    let (store, sessions) = test_stores();
    let first_session = add_private_session_with_id(&sessions, "u1", "会话甲第一条");
    add_private_session(&sessions, "u1", "会话乙第一条");
    let mut session = sessions.get(&first_session).unwrap().unwrap();
    sessions
        .append_exchange(&mut session, "会话甲第二条", "assistant reply")
        .unwrap();
    let provider =
        MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let observable = provider.clone();
    let mut config = test_config();
    config.max_sessions = 1;
    let worker = worker(&store, provider, config);

    for _ in 0..3 {
        worker
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .unwrap();
    }

    let requests = observable.requests();
    let inputs = requests
        .iter()
        .map(|request| request.messages.last().unwrap().content.as_str())
        .collect::<Vec<_>>();
    assert!(inputs[0].contains("会话甲第一条"));
    assert!(!inputs[0].contains("会话乙第一条"));
    assert!(inputs[1].contains("会话乙第一条"));
    assert!(!inputs[1].contains("会话甲第二条"));
    assert!(inputs[2].contains("会话甲第二条"));
}

#[tokio::test]
async fn same_session_second_dream_only_reads_appended_messages() {
    let (store, sessions) = test_stores();
    let session_id = add_private_session_with_id(&sessions, "u1", "旧消息只应处理一次");
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let observable = provider.clone();
    let worker = worker(&store, provider, test_config());
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    let mut session = sessions.get(&session_id).unwrap().unwrap();
    sessions
        .append_exchange(&mut session, "这是追加的新消息", "assistant reply")
        .unwrap();
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    let requests = observable.requests();
    let second_input = &requests[1].messages.last().unwrap().content;
    assert!(second_input.contains("这是追加的新消息"));
    assert!(!second_input.contains("旧消息只应处理一次"));
}

#[tokio::test]
async fn dream_input_does_not_replay_processed_content_from_session_summary() {
    let (store, sessions) = test_stores();
    let session_id = add_private_session_with_id(&sessions, "u1", "旧消息唯一标记-OLD-MESSAGE");
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let observable = provider.clone();
    let worker = worker(&store, provider, test_config());
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    let mut session = sessions.get(&session_id).unwrap().unwrap();
    session.summary = "摘要中的旧消息唯一标记-SUMMARY-REPLAY".to_owned();
    sessions.save(&mut session).unwrap();
    sessions
        .append_exchange(
            &mut session,
            "第二轮新消息唯一标记-NEW-MESSAGE",
            "assistant reply",
        )
        .unwrap();
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    let requests = observable.requests();
    let second_input = &requests[1].messages.last().unwrap().content;
    assert!(second_input.contains("第二轮新消息唯一标记-NEW-MESSAGE"));
    assert!(!second_input.contains("旧消息唯一标记-OLD-MESSAGE"));
    assert!(!second_input.contains("摘要中的旧消息唯一标记-SUMMARY-REPLAY"));
}

#[tokio::test]
async fn compacted_session_keeps_processed_boundary_and_unprocessed_archive_tail() {
    let (store, sessions) = test_stores();
    let session_id =
        add_private_session_with_id(&sessions, "u1", &format!("已处理消息-{}", "甲".repeat(800)));
    let mut session = sessions.get(&session_id).unwrap().unwrap();
    for text in ["未处理归档消息", "未处理活跃消息一", "未处理活跃消息二"] {
        sessions
            .append_exchange(&mut session, text, "assistant reply")
            .unwrap();
    }
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let observable = provider.clone();
    let mut config = test_config();
    config.max_input_chars = 600;
    let worker = worker(&store, provider, config);
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    let mut session = sessions.get(&session_id).unwrap().unwrap();
    sessions
        .compact_history(&mut session, "压缩摘要", 4)
        .unwrap();
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    let requests = observable.requests();
    let second_input = &requests[1].messages.last().unwrap().content;
    assert!(!second_input.contains("已处理消息"));
    assert!(second_input.contains("未处理归档消息"));
}

#[tokio::test]
async fn legacy_archive_without_ids_is_backfilled_and_processed_once() {
    let (store, sessions) = test_stores();
    let meta = SessionMeta::new(
        "private:u1".to_owned(),
        Some("u1".to_owned()),
        None,
        None,
        None,
        "test",
    );
    let mut session = sessions.create(&meta, "", false).unwrap();
    session.extra.insert(
        "archived_history".to_owned(),
        serde_json::json!([{
            "archived_at": "2026-01-01T00:00:00+08:00",
            "summary_before": "",
            "history": [{
                "role": "user",
                "content": "旧归档里的长期偏好",
                "ts": "2026-01-01T00:00:00+08:00"
            }]
        }]),
    );
    sessions.save(&mut session).unwrap();
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
    let observable = provider.clone();
    let worker = worker(&store, provider, test_config());

    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();
    assert!(
        worker
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .is_none()
    );

    assert!(
        observable.requests()[0]
            .messages
            .last()
            .unwrap()
            .content
            .contains("旧归档里的长期偏好")
    );
    let reloaded = sessions.get(&session.session_id).unwrap().unwrap();
    let archived_id = reloaded.extra["archived_history"][0]["history"][0]["message_id"]
        .as_i64()
        .unwrap();
    assert!(archived_id < 0);
}

#[tokio::test]
async fn character_limit_resumes_remaining_messages_in_same_session() {
    let (store, sessions) = test_stores();
    let session_id =
        add_private_session_with_id(&sessions, "u1", &format!("第一条-{}", "甲".repeat(240)));
    let mut session = sessions.get(&session_id).unwrap().unwrap();
    for (label, fill) in [("第二条", "乙"), ("第三条", "丙")] {
        sessions
            .append_exchange(
                &mut session,
                &format!("{label}-{}", fill.repeat(240)),
                "assistant reply",
            )
            .unwrap();
    }
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let observable = provider.clone();
    let mut config = test_config();
    config.max_input_chars = 560;
    let worker = worker(&store, provider, config);
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    let requests = observable.requests();
    let first_input = &requests[0].messages.last().unwrap().content;
    let second_input = &requests[1].messages.last().unwrap().content;
    assert!(first_input.contains("第一条"));
    assert!(first_input.contains("第二条"));
    assert!(!first_input.contains("第三条"));
    assert!(second_input.contains("第三条"));
    assert!(!second_input.contains("第一条"));
}

#[tokio::test]
async fn oversized_first_message_is_complete_and_following_message_resumes() {
    let (store, sessions) = test_stores();
    let tail_marker = "超长消息末尾唯一标记-OVERSIZED-TAIL";
    let session_id = add_private_session_with_id(
        &sessions,
        "u1",
        &format!("超长第一条-{}-{tail_marker}", "甲".repeat(1000)),
    );
    let mut session = sessions.get(&session_id).unwrap().unwrap();
    sessions
        .append_exchange(&mut session, "后续短消息必须保留", "assistant reply")
        .unwrap();
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let observable = provider.clone();
    let mut config = test_config();
    config.max_input_chars = 100;
    let worker = worker(&store, provider, config);
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();
    worker
        .run_once(private_context("u1"))
        .await
        .unwrap()
        .unwrap();

    let requests = observable.requests();
    let first_input = &requests[0].messages.last().unwrap().content;
    let second_input = &requests[1].messages.last().unwrap().content;
    assert!(first_input.chars().count() > 100);
    assert!(first_input.contains(tail_marker));
    assert!(
        !first_input.contains("后续短消息必须保留"),
        "首批只允许完整纳入单条超限消息"
    );
    assert!(
        second_input.contains("后续短消息必须保留"),
        "下一批必须继续处理后续消息"
    );
    assert!(!second_input.contains(tail_marker));
}

#[tokio::test]
async fn disabled_dream_does_not_call_model() {
    let (store, sessions) = test_stores();
    add_private_session(
        &sessions,
        "u1",
        "即使其他 Memory 后台能力开启也不应触发 Dream",
    );
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
    let observable = provider.clone();
    let mut config = test_config();
    config.enabled = false;

    assert!(
        worker(&store, provider, config)
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(observable.requests().is_empty());
}

#[tokio::test]
async fn enabled_dream_runs_without_consolidation_worker() {
    let (store, sessions) = test_stores();
    add_private_session(&sessions, "u1", "Dream 独立开启时仍可运行");
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
    let observable = provider.clone();

    assert!(
        worker(&store, provider, test_config())
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(observable.requests().len(), 1);
}

#[tokio::test]
async fn concurrent_dream_claims_execute_model_only_once() {
    let (store, sessions) = test_stores();
    add_private_session(&sessions, "u1", "我长期喜欢结构化回答");
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")])
        .with_dream_delay(Duration::from_millis(100));
    let observable = provider.clone();
    let worker = worker(&store, provider, test_config());
    let first = tokio::spawn({
        let worker = worker.clone();
        async move { worker.run_once(private_context("u1")).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = worker.run_once(private_context("u1")).await.unwrap();
    let first = first.await.unwrap().unwrap();

    assert!(first.is_some());
    assert!(second.is_none());
    assert_eq!(observable.requests().len(), 1);
}

#[tokio::test]
async fn ordinary_session_save_and_title_update_do_not_reprocess_old_messages() {
    let (store, sessions) = test_stores();
    let session_id = add_private_session_with_id(&sessions, "u1", "我长期喜欢结构化回答");
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
    let worker = worker(&store, provider, test_config());
    assert!(
        worker
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .is_some()
    );
    let mut session = sessions.get(&session_id).unwrap().unwrap();
    session.summary = "只更新摘要".to_owned();
    session
        .state
        .insert("dream-test".to_owned(), serde_json::Value::Bool(true));
    sessions.save(&mut session).unwrap();
    assert!(
        sessions
            .update_title_if_current(
                &session_id,
                crate::runtime::session::DEFAULT_SESSION_TITLE,
                "后台标题",
            )
            .unwrap()
    );

    assert!(
        worker
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .is_none()
    );
}
