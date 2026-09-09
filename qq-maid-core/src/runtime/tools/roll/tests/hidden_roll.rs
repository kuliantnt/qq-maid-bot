//! 群聊暗骰完整通知链路测试：覆盖 Worker 实际投递、入站幂等与目标隔离。

use std::{
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use qq_maid_common::identity_context::ConversationKind;

use crate::{
    runtime::{
        notification::{NotificationWorker, NotificationWorkerConfig},
        push::{PushError, PushIntent, PushResult, PushSink, PushTarget, PushTargetType},
        tools::roll::{dice::Roller, parse_extended_command},
    },
    storage::{
        database::SqliteDatabase,
        notification::{NOTIFICATION_MIGRATIONS, NotificationOutboxStore, NotificationStatus},
    },
};

use super::super::extensions::execute_extended_command_with_roller;

fn hidden_store() -> NotificationOutboxStore {
    let database =
        SqliteDatabase::open_temp("qq-maid-hidden-roll", NOTIFICATION_MIGRATIONS).unwrap();
    NotificationOutboxStore::new(database)
}

fn onebot_private_target() -> PushTarget {
    PushTarget::onebot11("test-bot", PushTargetType::Private, "test-user")
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

#[derive(Clone)]
struct CountingRoller {
    value: u8,
    counter: Arc<AtomicUsize>,
}

impl Roller for CountingRoller {
    fn roll(&mut self, sides: u8) -> u8 {
        self.counter.fetch_add(1, Ordering::SeqCst);
        self.value.min(sides).max(1)
    }
}

fn run_hidden_roll(
    store: &NotificationOutboxStore,
    roller: &mut CountingRoller,
    message_id: Option<&str>,
) -> String {
    run_hidden_roll_command(store, roller, message_id, "/rh 1d1 私有原因")
}

fn run_hidden_roll_command(
    store: &NotificationOutboxStore,
    roller: &mut CountingRoller,
    message_id: Option<&str>,
    command: &str,
) -> String {
    execute_extended_command_with_roller(
        &parse_extended_command(command).unwrap(),
        20,
        None,
        ConversationKind::Group,
        Some(&onebot_private_target()),
        store,
        message_id,
        "onebot11",
        Some("test-bot"),
        Some("g1"),
        roller,
    )
}

fn make_worker(store: &NotificationOutboxStore) -> (Arc<RecordingSink>, NotificationWorker) {
    let sink = Arc::new(RecordingSink::default());
    let worker = NotificationWorker::new(
        store.clone(),
        sink.clone(),
        NotificationWorkerConfig {
            enabled: true,
            poll_interval: Duration::from_secs(1),
            lock_timeout: Duration::from_secs(60),
            retry_delay: Duration::ZERO,
            batch_limit: 10,
        },
    );
    (sink, worker)
}

fn assert_no_leak(text: &str) {
    assert!(!text.contains("私有原因"), "{text}");
    assert!(!text.contains("1d1"), "{text}");
    assert!(!text.contains("= 1"), "{text}");
}

#[tokio::test]
async fn concurrent_replays_claim_one_rng_result_and_deliver_once() {
    let store = hidden_store();
    let counter = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();

    for value in [1_u8, 2] {
        let store = store.clone();
        let counter = counter.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let mut roller = CountingRoller { value, counter };
            let reply = run_hidden_roll_command(
                &store,
                &mut roller,
                Some("msg-concurrent-replay"),
                "/rh 1d2 私有原因",
            );
            (reply, value)
        }));
    }

    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("并发暗骰线程不应 panic"))
        .collect::<Vec<_>>();

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "并发重放时 RNG 必须严格只执行一次"
    );
    assert_eq!(
        results
            .iter()
            .filter(|(reply, _)| reply.contains("不会重复投骰"))
            .count(),
        1,
        "失败领取者必须只返回重复提示"
    );
    let (_, winner_value) = results
        .iter()
        .find(|(reply, _)| reply.contains("尚未确认送达"))
        .expect("必须有一个首次执行者");

    let tasks = store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 1, "并发重放只能形成一个 Outbox 事件");
    let text = tasks[0].payload["text"].as_str().unwrap();
    assert!(
        text.contains(&format!("= {winner_value}")),
        "payload 必须保留首次执行者的唯一 RNG 结果：{text}"
    );

    let (sink, worker) = make_worker(&store);
    let stats = worker.run_once().await.unwrap();
    assert_eq!(stats.sent_count, 1);
    assert_eq!(sink.intents.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn group_hidden_roll_payload_passes_worker_delivery_to_private_sink() {
    let store = hidden_store();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut roller = CountingRoller {
        value: 1,
        counter: counter.clone(),
    };

    let reply = run_hidden_roll(&store, &mut roller, Some("msg-hr-1"));
    assert!(reply.contains("私发队列"), "{reply}");
    assert_no_leak(&reply);
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    let tasks = store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].target.platform, "onebot11");
    assert_eq!(tasks[0].target.target_type, PushTargetType::Private);
    assert_eq!(tasks[0].target.target_id, "test-user");
    let payload = &tasks[0].payload;
    assert_eq!(payload["message_type"].as_str(), Some("text"));
    assert!(payload["text"].as_str().unwrap().contains("= 1"));

    let (sink, worker) = make_worker(&store);
    let stats = worker.run_once().await.unwrap();
    assert_eq!(stats.sent_count, 1, "worker 应成功投递而非 invalid payload");
    assert_eq!(stats.invalid_payload_count, 0);

    let task = store
        .get_by_dedupe_key(&tasks[0].dedupe_key)
        .unwrap()
        .unwrap();
    assert_eq!(task.status, NotificationStatus::Sent);

    let intents = sink.intents.lock().unwrap();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].target.platform, "onebot11");
    assert_eq!(intents[0].target.target_type, PushTargetType::Private);
    assert_eq!(intents[0].target.target_id, "test-user");
    assert_eq!(intents[0].message_type, "text");
    assert!(intents[0].text.contains("= 1"));
}

#[tokio::test]
async fn replayed_inbound_message_rolls_once_and_delivers_once() {
    let store = hidden_store();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut roller = CountingRoller {
        value: 1,
        counter: counter.clone(),
    };

    let first = run_hidden_roll(&store, &mut roller, Some("msg-replay-1"));
    assert!(first.contains("私发队列"), "{first}");
    assert_eq!(counter.load(Ordering::SeqCst), 1, "首次应恰好投骰一次");

    let second = run_hidden_roll(&store, &mut roller, Some("msg-replay-1"));
    assert!(second.contains("不会重复投骰"), "{second}");
    assert_eq!(counter.load(Ordering::SeqCst), 1, "重放不得触发第二次 RNG");

    let tasks = store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 1, "重放不得生成第二个 Outbox 任务");
    assert!(
        tasks[0].payload["text"].as_str().unwrap().contains("= 1"),
        "payload 必须保持第一次的骰值"
    );

    let (sink, worker) = make_worker(&store);
    let stats = worker.run_once().await.unwrap();
    assert_eq!(stats.sent_count, 1);
    assert_eq!(sink.intents.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn different_message_ids_form_independent_hidden_roll_events() {
    let store = hidden_store();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut roller = CountingRoller {
        value: 1,
        counter: counter.clone(),
    };

    let first = run_hidden_roll(&store, &mut roller, Some("msg-hr-a"));
    let second = run_hidden_roll(&store, &mut roller, Some("msg-hr-b"));

    assert!(first.contains("私发队列") && second.contains("私发队列"));
    assert_eq!(counter.load(Ordering::SeqCst), 2, "不同消息应各自投骰一次");
    let tasks = store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 2, "不同 message_id 应形成独立暗骰事件");
    assert_eq!(
        tasks
            .iter()
            .map(|task| task.dedupe_key.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2,
        "两个 dedupe_key 互不去重"
    );
}

#[tokio::test]
async fn group_hidden_roll_without_trusted_message_id_is_rejected() {
    let store = hidden_store();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut roller = CountingRoller {
        value: 1,
        counter: counter.clone(),
    };

    let reply = run_hidden_roll(&store, &mut roller, None);

    assert!(reply.contains("缺少可信消息 ID"), "{reply}");
    assert!(reply.contains("未投骰"), "{reply}");
    assert_eq!(counter.load(Ordering::SeqCst), 0, "拒绝时不得投骰");
    assert!(
        store.list_all_for_test().unwrap().is_empty(),
        "不得创建 Outbox 任务"
    );
}
