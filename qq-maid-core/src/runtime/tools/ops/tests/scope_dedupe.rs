use std::{fs, sync::Arc, time::Duration};

use uuid::Uuid;

use super::*;

#[cfg(unix)]
#[tokio::test]
async fn concurrent_same_scope_codex_replay_runs_once_and_returns_same_task_id() {
    let counter = std::env::temp_dir().join(format!("codex-concurrent-{}", Uuid::new_v4()));
    let script = write_script(&format!("printf x >> '{}'\nsleep 30", counter.display()));
    let store = test_store();
    let service = OpsService::new(
        codex_ops_config(&script, &std::env::temp_dir(), 60, 1),
        store.clone(),
    );
    let mut context = private_context(Some("admin-1"));
    context.inbound_id = Some("same-trusted-message".to_owned());
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let runtime = tokio::runtime::Handle::current();
    let handles = [(), ()].map(|_| {
        let service = service.clone();
        let context = context.clone();
        let barrier = barrier.clone();
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let _guard = runtime.enter();
            barrier.wait();
            service.accept(
                parse_ops_command("/ops codex 并发修复任务").unwrap(),
                context,
            )
        })
    });
    let replies = handles.map(|handle| handle.join().unwrap());
    let task_ids = replies
        .iter()
        .map(|reply| task_id_from_reply(reply))
        .collect::<Vec<_>>();

    assert_eq!(task_ids[0], task_ids[1]);
    assert_eq!(
        replies
            .iter()
            .filter(|reply| reply.starts_with("Codex 任务已受理"))
            .count(),
        1
    );
    for _ in 0..100 {
        if counter.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fs::read_to_string(&counter).unwrap(), "x");
    assert!(
        service
            .accept(
                parse_ops_command(&format!("/ops cancel {}", task_ids[0])).unwrap(),
                context,
            )
            .contains("正在取消")
    );
    let _ = wait_for_task(&store).await;
}

#[cfg(unix)]
#[tokio::test]
async fn same_message_id_is_isolated_by_target_kind_target_id_and_account() {
    let script = write_script("sleep 30");
    let store = test_store();
    let service = OpsService::new(
        codex_ops_config(&script, &std::env::temp_dir(), 60, 4),
        store.clone(),
    );
    let inbound_id = Some("platform-message-collision".to_owned());

    let mut group_one = group_context(Some("group-1"), Some("owner"));
    group_one.inbound_id = inbound_id.clone();
    let mut group_two = group_context(Some("group-2"), Some("admin"));
    group_two.inbound_id = inbound_id.clone();
    let mut private_account_a = private_context(Some("admin-1"));
    private_account_a.platform = group_one.platform.clone();
    private_account_a.account_id = group_one.account_id.clone();
    private_account_a.inbound_id = inbound_id.clone();
    let mut private_account_b = private_account_a.clone();
    private_account_b.account_id = Some("app-b".to_owned());

    let contexts = [
        group_one.clone(),
        group_two.clone(),
        private_account_a.clone(),
        private_account_b.clone(),
    ];
    let task_ids = contexts
        .iter()
        .enumerate()
        .map(|(index, context)| {
            let reply = service.accept(
                parse_ops_command(&format!("/ops codex scope task {index}")).unwrap(),
                context.clone(),
            );
            assert!(reply.starts_with("Codex 任务已受理"), "{reply}");
            task_id_from_reply(&reply)
        })
        .collect::<Vec<_>>();

    assert_eq!(
        task_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    assert!(
        service
            .accept(
                parse_ops_command(&format!("/ops cancel {}", task_ids[1])).unwrap(),
                group_one.clone(),
            )
            .contains("未找到")
    );
    assert!(
        service
            .accept(
                parse_ops_command(&format!("/ops cancel {}", task_ids[2])).unwrap(),
                group_one,
            )
            .contains("未找到")
    );
    assert!(
        service
            .accept(
                parse_ops_command(&format!("/ops cancel {}", task_ids[3])).unwrap(),
                private_account_a.clone(),
            )
            .contains("未找到")
    );

    for (task_id, context) in task_ids.iter().zip(contexts) {
        assert!(
            service
                .accept(
                    parse_ops_command(&format!("/ops cancel {task_id}")).unwrap(),
                    context,
                )
                .contains("正在取消")
        );
    }
    for _ in 0..200 {
        if store.list_all_for_test().unwrap().len() == 4 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("all scoped Codex tasks should finish and enqueue notifications");
}
