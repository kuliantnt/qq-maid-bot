use super::*;

#[test]
fn codex_requires_independent_enable_switch_and_trusted_inbound_id() {
    let store = test_store();
    let service = OpsService::new(config_with("/fixed/status", true, false), store.clone());
    let reply = service.accept(
        parse_ops_command("/ops codex 修复构建").unwrap(),
        private_context(Some("admin-1")),
    );
    assert!(reply.contains("Codex 运维任务未启用"));

    let mut context = private_context(Some("admin-1"));
    context.inbound_id = None;
    let reply = service.accept(parse_ops_command("/ops status").unwrap(), context);
    assert!(reply.contains("缺少可信消息 ID"));
    assert!(store.list_all_for_test().unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn duplicate_codex_inbound_is_single_execution_and_scope_isolated() {
    let counter = std::env::temp_dir().join(format!("codex-count-{}", Uuid::new_v4()));
    let script = write_script(&format!("printf x >> '{}'\nsleep 30", counter.display()));
    let store = test_store();
    let service = OpsService::new(
        codex_ops_config(&script, &write_working_directory(), 60, 1),
        store.clone(),
    );
    let context = private_context(Some("admin-1"));

    let first = service.accept(
        parse_ops_command("/ops codex 修复 构建").unwrap(),
        context.clone(),
    );
    let task_id = task_id_from_reply(&first);
    let duplicate = service.accept(
        parse_ops_command("/ops codex 修复 构建").unwrap(),
        context.clone(),
    );
    assert!(duplicate.contains("不会重复执行"));
    assert!(duplicate.contains(&task_id));

    let mut another_event = context.clone();
    another_event.inbound_id = Some(Uuid::new_v4().to_string());
    let capacity = service.accept(
        parse_ops_command("/ops codex 另一个任务").unwrap(),
        another_event,
    );
    assert!(capacity.contains("当前已有 Codex 任务"));

    for _ in 0..100 {
        if counter.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fs::read_to_string(&counter).unwrap(), "x");
    let listed = service.accept(parse_ops_command("/ops list").unwrap(), context.clone());
    assert!(listed.contains(&task_id));

    let mut other_actor = context.clone();
    other_actor.user_id = Some("admin-2".to_owned());
    assert!(
        service
            .accept(parse_ops_command("/ops list").unwrap(), other_actor)
            .contains("没有运行中")
    );
    let mut other_account = context.clone();
    other_account.account_id = Some("bot-b".to_owned());
    assert!(
        service
            .accept(parse_ops_command("/ops list").unwrap(), other_account)
            .contains("没有运行中")
    );
    let mut other_platform = context.clone();
    other_platform.platform = "qq_official".to_owned();
    assert!(
        service
            .accept(parse_ops_command("/ops list").unwrap(), other_platform)
            .contains("没有运行中")
    );

    let cancel = service.accept(
        parse_ops_command(&format!("/ops cancel {task_id}")).unwrap(),
        context.clone(),
    );
    assert_eq!(cancel, format!("正在取消任务 {task_id}。"));
    let repeat = service.accept(
        parse_ops_command(&format!("/ops stop {task_id}")).unwrap(),
        context.clone(),
    );
    assert!(repeat.contains("正在取消") || repeat.contains("已结束"));
    let task = wait_for_task(&store).await;
    assert_eq!(task.source_type, "ops");
    assert_eq!(fs::read_to_string(&counter).unwrap(), "x");

    let after = service.accept(
        parse_ops_command(&format!("/ops cancel {task_id}")).unwrap(),
        context,
    );
    assert!(after.contains("已结束"));

    let sink = Arc::new(AlwaysFailSink::default());
    let worker = NotificationWorker::new(
        store,
        sink.clone(),
        NotificationWorkerConfig {
            enabled: true,
            poll_interval: Duration::from_secs(1),
            lock_timeout: Duration::from_secs(60),
            retry_delay: Duration::ZERO,
            batch_limit: 10,
        },
    );
    wait_for_failed_push_attempts(&worker, &sink, 2).await;
    assert_eq!(*sink.attempts.lock().unwrap(), 2);
    assert_eq!(fs::read_to_string(counter).unwrap(), "x");
}

#[cfg(unix)]
#[tokio::test]
async fn group_tasks_are_isolated_by_group_platform_and_account() {
    let script = write_script("sleep 30");
    let store = test_store();
    let service = OpsService::new(
        codex_ops_config(&script, &write_working_directory(), 60, 1),
        store.clone(),
    );
    let group_one = group_context(Some("group-1"), Some("owner"));
    let accepted = service.accept(
        parse_ops_command("/ops codex group task").unwrap(),
        group_one.clone(),
    );
    let task_id = task_id_from_reply(&accepted);

    let group_two = group_context(Some("group-2"), Some("admin"));
    assert!(
        service
            .accept(parse_ops_command("/ops list").unwrap(), group_two.clone())
            .contains("没有运行中")
    );
    assert!(
        service
            .accept(
                parse_ops_command(&format!("/ops cancel {task_id}")).unwrap(),
                group_two,
            )
            .contains("未找到")
    );

    let mut other_account = group_one.clone();
    other_account.account_id = Some("app-b".to_owned());
    assert!(
        service
            .accept(parse_ops_command("/ops list").unwrap(), other_account)
            .contains("没有运行中")
    );
    let mut other_platform = group_one.clone();
    other_platform.platform = "onebot11".to_owned();
    assert!(
        service
            .accept(parse_ops_command("/ops list").unwrap(), other_platform)
            .contains("没有运行中")
    );

    assert!(
        service
            .accept(
                parse_ops_command(&format!("/ops cancel {task_id}")).unwrap(),
                group_one,
            )
            .contains("正在取消")
    );
    let task = wait_for_task(&store).await;
    assert_eq!(task.target.target_type, PushTargetType::Group);
    assert_eq!(task.target.target_id, "group-1");
}

#[cfg(unix)]
pub(super) async fn wait_for_task(
    store: &crate::storage::notification::NotificationOutboxStore,
) -> crate::storage::notification::NotificationTask {
    for _ in 0..100 {
        if let Some(task) = store.list_all_for_test().unwrap().pop() {
            return task;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("ops result notification was not queued");
}

#[cfg(unix)]
#[tokio::test]
async fn accepted_private_and_group_commands_preserve_original_push_target() {
    let script = write_script("printf ok");
    let store = test_store();
    let service = OpsService::new(
        config_with(script.to_str().unwrap(), true, true),
        store.clone(),
    );
    let private_reply = service.accept(
        parse_ops_command("/ops status").unwrap(),
        private_context(Some("admin-1")),
    );
    assert!(private_reply.contains("已受理"));
    let private_task = wait_for_task(&store).await;
    assert_eq!(private_task.source_type, "ops");
    assert_eq!(private_task.target.platform, "onebot11");
    assert_eq!(private_task.target.account_id.as_deref(), Some("bot-a"));
    assert_eq!(private_task.target.target_type, PushTargetType::Private);
    assert_eq!(private_task.target.target_id, "private-target");
    assert!(private_task.dedupe_key.starts_with("ops:"));
    assert!(private_task.dedupe_key.ends_with(":result"));

    let group_store = test_store();
    let group_service = OpsService::new(
        config_with(script.to_str().unwrap(), false, true),
        group_store.clone(),
    );
    for role in ["owner", "admin"] {
        assert!(
            group_service
                .accept(
                    parse_ops_command("/ops status").unwrap(),
                    group_context(Some("group-1"), Some(role)),
                )
                .contains("已受理")
        );
    }
    let group_task = wait_for_task(&group_store).await;
    assert_eq!(group_task.target.target_type, PushTargetType::Group);
    assert_eq!(group_task.target.target_id, "group-1");
}

#[derive(Default)]
pub(super) struct AlwaysFailSink {
    pub(super) attempts: Mutex<usize>,
}

#[async_trait]
impl PushSink for AlwaysFailSink {
    async fn push(&self, _intent: PushIntent) -> Result<PushResult, PushError> {
        *self.attempts.lock().unwrap() += 1;
        Err(PushError::Failed {
            summary: "test failure".to_owned(),
        })
    }
}

pub(super) async fn wait_for_failed_push_attempts(
    worker: &NotificationWorker,
    sink: &AlwaysFailSink,
    expected: usize,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let attempts = *sink.attempts.lock().unwrap();
        assert!(
            attempts <= expected,
            "notification attempts exceeded expected count: {attempts} > {expected}"
        );
        if attempts == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "notification retry did not reach {expected} attempts before timeout; actual={attempts}"
        );
        worker.run_once().await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn notification_retry_never_reexecutes_program() {
    let counter = std::env::temp_dir().join(format!("ops-count-{}", Uuid::new_v4()));
    let script = write_script(&format!("printf x >> '{}'", counter.display()));
    let store = test_store();
    let service = OpsService::new(
        config_with(script.to_str().unwrap(), true, false),
        store.clone(),
    );
    service.accept(
        parse_ops_command("/ops status").unwrap(),
        private_context(Some("admin-1")),
    );
    let _ = wait_for_task(&store).await;
    assert_eq!(fs::read_to_string(&counter).unwrap(), "x");

    let sink = Arc::new(AlwaysFailSink::default());
    let worker = NotificationWorker::new(
        store,
        sink.clone(),
        NotificationWorkerConfig {
            enabled: true,
            poll_interval: Duration::from_secs(1),
            lock_timeout: Duration::from_secs(60),
            retry_delay: Duration::ZERO,
            batch_limit: 10,
        },
    );
    wait_for_failed_push_attempts(&worker, &sink, 2).await;

    assert_eq!(*sink.attempts.lock().unwrap(), 2);
    assert_eq!(fs::read_to_string(counter).unwrap(), "x");
}
