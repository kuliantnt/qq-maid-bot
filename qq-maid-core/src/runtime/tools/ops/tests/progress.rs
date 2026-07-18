use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

use crate::{
    runtime::push::{PushTarget, PushTargetType},
    runtime::tools::ops::{
        CodexProgressSchedule, OPS_PROGRESS_SOURCE_TYPE, enqueue_progress, enqueue_result,
        execute_codex_with_progress,
    },
    storage::notification::NotificationStatus,
};

use super::*;

fn completed_result(status: OpsExecutionStatus) -> OpsExecutionResult {
    OpsExecutionResult {
        command: "codex".to_owned(),
        status,
        exit_code: (status == OpsExecutionStatus::Succeeded).then_some(0),
        elapsed: Duration::from_millis(20),
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        error_type: None,
        tree_termination_limited: false,
    }
}

fn progress_target() -> PushTarget {
    PushTarget::new(
        "onebot",
        Some("bot-progress".to_owned()),
        PushTargetType::Private,
        "progress-user",
    )
}

async fn wait_for_progress_count(
    store: &crate::storage::notification::NotificationOutboxStore,
    expected: usize,
) {
    for _ in 0..200 {
        let count = store
            .list_all_for_test()
            .unwrap()
            .into_iter()
            .filter(|task| task.source_type == OPS_PROGRESS_SOURCE_TYPE)
            .count();
        if count >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("expected {expected} progress notifications");
}

#[tokio::test]
async fn short_task_and_timer_boundary_do_not_queue_progress() {
    for initial_delay in [Duration::from_millis(50), Duration::ZERO] {
        let store = test_store();
        let result = execute_codex_with_progress(
            async { completed_result(OpsExecutionStatus::Succeeded) },
            &store,
            "ops-short",
            progress_target(),
            CodexProgressSchedule {
                initial_delay,
                interval: Duration::from_millis(50),
            },
        )
        .await;

        assert_eq!(result.status, OpsExecutionStatus::Succeeded);
        assert!(store.list_all_for_test().unwrap().is_empty());
    }
}

#[tokio::test]
async fn long_task_queues_rate_limited_unique_progress_snapshots() {
    let store = test_store();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn({
        let store = store.clone();
        async move {
            execute_codex_with_progress(
                async { finished.await.unwrap() },
                &store,
                "ops-long",
                progress_target(),
                CodexProgressSchedule {
                    initial_delay: Duration::from_millis(10),
                    interval: Duration::from_millis(40),
                },
            )
            .await
        }
    });

    wait_for_progress_count(&store, 3).await;
    let progress = store
        .list_all_for_test()
        .unwrap()
        .into_iter()
        .filter(|task| task.source_type == OPS_PROGRESS_SOURCE_TYPE)
        .collect::<Vec<_>>();
    assert_eq!(progress.len(), 3);
    assert_eq!(
        progress
            .iter()
            .map(|task| task.dedupe_key.as_str())
            .collect::<Vec<_>>(),
        vec![
            "ops:ops-long:progress:1",
            "ops:ops-long:progress:2",
            "ops:ops-long:progress:3",
        ]
    );
    let payload = progress[0].payload.to_string();
    assert_eq!(progress[0].source_id, "ops-long");
    assert!(payload.contains("仍在运行"));
    assert!(payload.contains("/ops cancel"));

    finish
        .send(completed_result(OpsExecutionStatus::Succeeded))
        .unwrap();
    handle.await.unwrap();
    assert!(store.list_all_for_test().unwrap().iter().all(|task| {
        task.source_type != OPS_PROGRESS_SOURCE_TYPE || task.status == NotificationStatus::Cancelled
    }));
}

#[tokio::test]
async fn first_progress_snapshot_is_delivered_through_notification_worker() {
    let store = test_store();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn({
        let store = store.clone();
        async move {
            execute_codex_with_progress(
                async { finished.await.unwrap() },
                &store,
                "ops-deliver-progress",
                progress_target(),
                CodexProgressSchedule {
                    initial_delay: Duration::from_millis(5),
                    interval: Duration::from_secs(1),
                },
            )
            .await
        }
    });
    wait_for_progress_count(&store, 1).await;

    let sink = Arc::new(RecordingSink::default());
    let worker = NotificationWorker::new(
        store.clone(),
        sink.clone(),
        NotificationWorkerConfig::default(),
    );
    worker.run_once().await.unwrap();
    {
        let pushed = sink.intents.lock().unwrap();
        assert_eq!(pushed.len(), 1);
        assert!(pushed[0].text.contains("仍在运行"));
        assert!(pushed[0].text.contains("/ops cancel"));
    }

    finish
        .send(completed_result(OpsExecutionStatus::Succeeded))
        .unwrap();
    handle.await.unwrap();
    let progress = store
        .get_by_dedupe_key("ops:ops-deliver-progress:progress:1")
        .unwrap()
        .unwrap();
    assert_eq!(progress.status, NotificationStatus::Sent);
}

#[tokio::test]
async fn terminal_statuses_stop_progress_and_cancel_pending_snapshots() {
    for status in [
        OpsExecutionStatus::Failed,
        OpsExecutionStatus::TimedOut,
        OpsExecutionStatus::Cancelled,
        OpsExecutionStatus::SpawnFailed,
    ] {
        let store = test_store();
        let result = execute_codex_with_progress(
            async move {
                tokio::time::sleep(Duration::from_millis(14)).await;
                completed_result(status)
            },
            &store,
            "ops-terminal",
            progress_target(),
            CodexProgressSchedule {
                initial_delay: Duration::from_millis(5),
                interval: Duration::from_millis(50),
            },
        )
        .await;
        assert_eq!(result.status, status);
        let before = store.list_all_for_test().unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].status, NotificationStatus::Cancelled);

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(store.list_all_for_test().unwrap().len(), before.len());
    }
}

#[tokio::test]
async fn completion_cancels_stale_progress_before_final_result_is_deliverable() {
    let store = test_store();
    let task_id = "ops-final-order";
    let result = execute_codex_with_progress(
        async {
            tokio::time::sleep(Duration::from_millis(18)).await;
            completed_result(OpsExecutionStatus::Succeeded)
        },
        &store,
        task_id,
        progress_target(),
        CodexProgressSchedule {
            initial_delay: Duration::from_millis(5),
            interval: Duration::from_secs(1),
        },
    )
    .await;
    enqueue_result(
        &store,
        task_id,
        progress_target(),
        &result,
        Some(task_id),
        Instant::now(),
    );

    let tasks = store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].status, NotificationStatus::Cancelled);
    assert_eq!(tasks[1].dedupe_key, "ops:ops-final-order:result");
    assert_eq!(tasks[1].status, NotificationStatus::Pending);

    let sink = Arc::new(RecordingSink::default());
    let worker = NotificationWorker::new(store, sink.clone(), NotificationWorkerConfig::default());
    worker.run_once().await.unwrap();
    let pushed = sink.intents.lock().unwrap();
    assert_eq!(pushed.len(), 1);
    assert!(!pushed[0].text.contains("仍在运行"));
}

#[tokio::test]
async fn progress_retry_does_not_reexecute_codex_future() {
    let store = test_store();
    let starts = Arc::new(AtomicUsize::new(0));
    let (finish, finished) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn({
        let store = store.clone();
        let starts = starts.clone();
        async move {
            execute_codex_with_progress(
                async move {
                    starts.fetch_add(1, Ordering::SeqCst);
                    finished.await.unwrap()
                },
                &store,
                "ops-retry-progress",
                progress_target(),
                CodexProgressSchedule {
                    initial_delay: Duration::from_millis(5),
                    interval: Duration::from_secs(10),
                },
            )
            .await
        }
    });
    wait_for_progress_count(&store, 1).await;

    let sink = Arc::new(AlwaysFailSink::default());
    let worker = NotificationWorker::new(
        store.clone(),
        sink.clone(),
        NotificationWorkerConfig {
            retry_delay: Duration::ZERO,
            ..NotificationWorkerConfig::default()
        },
    );
    wait_for_failed_push_attempts(&worker, &sink, 2).await;
    assert_eq!(*sink.attempts.lock().unwrap(), 2);
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    finish
        .send(completed_result(OpsExecutionStatus::Failed))
        .unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn different_tasks_use_non_conflicting_progress_keys() {
    let store = test_store();
    let (finish_a, finished_a) = tokio::sync::oneshot::channel();
    let (finish_b, finished_b) = tokio::sync::oneshot::channel();
    let task_a = tokio::spawn({
        let store = store.clone();
        async move {
            execute_codex_with_progress(
                async { finished_a.await.unwrap() },
                &store,
                "ops-a",
                progress_target(),
                CodexProgressSchedule {
                    initial_delay: Duration::from_millis(5),
                    interval: Duration::from_secs(1),
                },
            )
            .await
        }
    });
    let task_b = tokio::spawn({
        let store = store.clone();
        async move {
            execute_codex_with_progress(
                async { finished_b.await.unwrap() },
                &store,
                "ops-b",
                progress_target(),
                CodexProgressSchedule {
                    initial_delay: Duration::from_millis(5),
                    interval: Duration::from_secs(1),
                },
            )
            .await
        }
    });
    wait_for_progress_count(&store, 2).await;
    let keys = store
        .list_all_for_test()
        .unwrap()
        .into_iter()
        .map(|task| task.dedupe_key)
        .collect::<std::collections::HashSet<_>>();
    assert!(keys.contains("ops:ops-a:progress:1"));
    assert!(keys.contains("ops:ops-b:progress:1"));

    finish_a
        .send(completed_result(OpsExecutionStatus::Succeeded))
        .unwrap();
    finish_b
        .send(completed_result(OpsExecutionStatus::Succeeded))
        .unwrap();
    task_a.await.unwrap();
    task_b.await.unwrap();
}

#[test]
fn ops_service_startup_cancels_abandoned_progress_snapshots() {
    let store = test_store();
    enqueue_progress(
        &store,
        "ops-before-restart",
        progress_target(),
        1,
        Duration::from_secs(45),
    );
    let before = store
        .get_by_dedupe_key("ops:ops-before-restart:progress:1")
        .unwrap()
        .unwrap();
    assert_eq!(before.status, NotificationStatus::Pending);

    let _service = OpsService::new(OpsConfig::default(), store.clone());
    let after = store
        .get_by_dedupe_key("ops:ops-before-restart:progress:1")
        .unwrap()
        .unwrap();
    assert_eq!(after.status, NotificationStatus::Cancelled);
}

#[derive(Default)]
struct RecordingSink {
    intents: Mutex<Vec<PushIntent>>,
}

#[async_trait]
impl PushSink for RecordingSink {
    async fn push(&self, intent: PushIntent) -> Result<PushResult, PushError> {
        self.intents.lock().unwrap().push(intent);
        Ok(PushResult { message_id: None })
    }
}
