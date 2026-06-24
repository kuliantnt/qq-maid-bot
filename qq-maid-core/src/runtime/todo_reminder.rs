//! Todo 每日提醒后台调度。
//!
//! 当前提醒只面向可验证 private target 的个人待办：群内 Todo 仍保留现有查询/操作语义，
//! 但不会主动推回群里，避免暴露按个人 owner 归属的待办内容。

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeZone, Utc};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::{
    config::DailyReminderTime,
    runtime::push::{
        GatewayPushClient, GatewayPushError, GatewayPushTarget, GatewayPushTargetType,
    },
    storage::todo::{
        TodoItem, TodoReminderOwnerQueryResult, TodoReminderOwnerSkipReason, TodoStore,
    },
    util::time_context::{
        format_todo_time_for_display, local_date_from_timestamp, shanghai_offset,
    },
};

const MAX_ITEMS_PER_SECTION: usize = 10;
// 每日提醒默认只在固定时点触发一次；若这一轮存在临时失败，需要在当天补跑，
// 避免把本应今天送达的提醒直接拖到下一次日常调度。
const FAILED_RUN_RETRY_DELAY: Duration = Duration::from_secs(300);
// 调度层只做一次当天补跑：成功 owner 已通过 sent_markers 跳过，失败 owner 可被重试，
// 同时避免 gateway 长时间不可用时在同一天内无限循环占用后台任务。
const MAX_SCHEDULED_ATTEMPTS_PER_DAY: usize = 2;

#[derive(Debug, Clone, Copy)]
pub struct TodoReminderSchedulerConfig {
    pub enabled: bool,
    pub reminder_time: DailyReminderTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoReminderRunStats {
    pub candidate_owner_count: usize,
    pub skipped_owner_count: usize,
    pub sent_owner_count: usize,
    pub failed_owner_count: usize,
    pub empty_owner_count: usize,
    pub already_sent_owner_count: usize,
    pub duplicate_owner_count: usize,
}

pub struct TodoReminderScheduler {
    store: TodoStore,
    push_client: GatewayPushClient,
    config: TodoReminderSchedulerConfig,
    sent_markers: Arc<Mutex<HashSet<String>>>,
    retry_delay: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormattedReminder {
    markdown: String,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReminderDisplayItem {
    title: String,
    due_label: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ReminderBuckets {
    today: Vec<ReminderDisplayItem>,
    overdue: Vec<ReminderDisplayItem>,
    no_date: Vec<ReminderDisplayItem>,
}

enum ReminderClassification {
    Today(ReminderDisplayItem),
    Overdue(ReminderDisplayItem),
    NoDate(ReminderDisplayItem),
    Future,
}

impl TodoReminderScheduler {
    pub fn new(
        store: TodoStore,
        push_client: GatewayPushClient,
        config: TodoReminderSchedulerConfig,
    ) -> Self {
        Self {
            store,
            push_client,
            config,
            sent_markers: Arc::new(Mutex::new(HashSet::new())),
            retry_delay: FAILED_RUN_RETRY_DELAY,
        }
    }

    #[cfg(test)]
    fn with_retry_delay_for_test(mut self, retry_delay: Duration) -> Self {
        self.retry_delay = retry_delay;
        self
    }

    pub fn spawn(self) {
        if !self.config.enabled {
            info!("todo daily reminder disabled");
            return;
        }
        tokio::spawn(async move {
            info!(
                reminder_time = %self.config.reminder_time,
                "todo daily reminder scheduler enabled"
            );
            self.run_loop().await;
        });
    }

    async fn run_loop(self) {
        loop {
            let now = Utc::now().with_timezone(&shanghai_offset());
            let next_run = next_run_after(now, self.config.reminder_time);
            let wait_duration = (next_run - now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(0));
            debug!(
                next_run_at = %next_run.to_rfc3339(),
                reminder_time = %self.config.reminder_time,
                "todo daily reminder waiting for next run"
            );
            tokio::time::sleep(wait_duration).await;
            self.run_scheduled_cycle_for_date(next_run.date_naive())
                .await;
        }
    }

    async fn run_scheduled_cycle_for_date(&self, scheduled_date: NaiveDate) {
        let mut attempt = 1usize;
        loop {
            match self.run_once_for_date(scheduled_date).await {
                Ok(stats) if stats.failed_owner_count == 0 => {
                    if attempt > 1 {
                        info!(
                            scheduled_date = %scheduled_date,
                            attempt,
                            "todo daily reminder retry finished successfully"
                        );
                    }
                    return;
                }
                Ok(stats) => {
                    warn!(
                        scheduled_date = %scheduled_date,
                        attempt,
                        failed_owner_count = stats.failed_owner_count,
                        "todo daily reminder cycle had failed owners; scheduling same-day retry"
                    );
                }
                Err(err) => {
                    warn!(
                        scheduled_date = %scheduled_date,
                        attempt,
                        error = %err,
                        "todo daily reminder cycle failed; scheduling same-day retry"
                    );
                }
            }

            if attempt >= MAX_SCHEDULED_ATTEMPTS_PER_DAY {
                warn!(
                    scheduled_date = %scheduled_date,
                    attempt,
                    "todo daily reminder retry attempts exhausted for today"
                );
                return;
            }

            let now = Utc::now().with_timezone(&shanghai_offset());
            let Some(retry_at) = next_retry_after(now, scheduled_date, self.retry_delay) else {
                warn!(
                    scheduled_date = %scheduled_date,
                    attempt,
                    "todo daily reminder retry window closed for today"
                );
                return;
            };
            let wait_duration = (retry_at - now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(0));
            debug!(
                scheduled_date = %scheduled_date,
                attempt,
                retry_at = %retry_at.to_rfc3339(),
                "todo daily reminder waiting to retry failed cycle"
            );
            tokio::time::sleep(wait_duration).await;
            attempt += 1;
        }
    }

    pub async fn run_once(&self) -> Result<TodoReminderRunStats, String> {
        self.run_once_for_date(Utc::now().with_timezone(&shanghai_offset()).date_naive())
            .await
    }

    async fn run_once_for_date(&self, today: NaiveDate) -> Result<TodoReminderRunStats, String> {
        prune_sent_markers(&self.sent_markers, today);

        let owner_result = self
            .store
            .list_private_reminder_owners()
            .map_err(|err| err.message().to_owned())?;
        log_skipped_owners(&owner_result);

        let mut stats = TodoReminderRunStats {
            candidate_owner_count: owner_result.candidates.len(),
            skipped_owner_count: owner_result.skipped.len(),
            ..TodoReminderRunStats::default()
        };
        let mut seen_owners = HashSet::new();
        for owner in owner_result.candidates {
            if !seen_owners.insert(owner.owner_key.clone()) {
                stats.duplicate_owner_count += 1;
                continue;
            }
            if was_sent_today(&self.sent_markers, &owner.owner_key, today) {
                stats.already_sent_owner_count += 1;
                continue;
            }

            let items = self
                .store
                .list_pending_for_private_scopes(&owner.owner_key, &owner.private_scope_keys)
                .map_err(|err| err.message().to_owned())?;
            let Some(message) = format_reminder_message(&items, today) else {
                stats.empty_owner_count += 1;
                continue;
            };

            let target = GatewayPushTarget {
                target_type: GatewayPushTargetType::Private,
                target_id: owner.private_target_id.clone(),
            };
            match self
                .push_client
                .send(&target, "markdown", &message.markdown, Some(&message.text))
                .await
            {
                Ok(()) => {
                    mark_sent_today(&self.sent_markers, &owner.owner_key, today);
                    stats.sent_owner_count += 1;
                    info!(
                        owner = %short_hash(&owner.owner_key),
                        target = %short_hash(&owner.private_target_id),
                        "todo daily reminder sent"
                    );
                }
                Err(err) => {
                    stats.failed_owner_count += 1;
                    warn!(
                        owner = %short_hash(&owner.owner_key),
                        target = %short_hash(&owner.private_target_id),
                        error = %safe_push_error(&err),
                        "todo daily reminder push failed"
                    );
                }
            }
        }
        Ok(stats)
    }
}

fn next_run_after(
    now: DateTime<FixedOffset>,
    reminder_time: DailyReminderTime,
) -> DateTime<FixedOffset> {
    let offset = shanghai_offset();
    let today = now.date_naive();
    let today_run = offset
        .with_ymd_and_hms(
            today.year(),
            today.month(),
            today.day(),
            reminder_time.hour.into(),
            reminder_time.minute.into(),
            0,
        )
        .single()
        .expect("Asia/Shanghai uses a stable fixed offset");
    if now <= today_run {
        today_run
    } else {
        let tomorrow = today.succ_opt().expect("valid next date");
        offset
            .with_ymd_and_hms(
                tomorrow.year(),
                tomorrow.month(),
                tomorrow.day(),
                reminder_time.hour.into(),
                reminder_time.minute.into(),
                0,
            )
            .single()
            .expect("Asia/Shanghai uses a stable fixed offset")
    }
}

fn next_retry_after(
    now: DateTime<FixedOffset>,
    scheduled_date: NaiveDate,
    retry_delay: Duration,
) -> Option<DateTime<FixedOffset>> {
    if now.date_naive() != scheduled_date {
        return None;
    }
    let retry_at = now + chrono::Duration::from_std(retry_delay).ok()?;
    (retry_at.date_naive() == scheduled_date).then_some(retry_at)
}

fn format_reminder_message(items: &[TodoItem], today: NaiveDate) -> Option<FormattedReminder> {
    let mut buckets = ReminderBuckets::default();
    for item in items {
        match classify_item(item, today) {
            ReminderClassification::Today(display) => buckets.today.push(display),
            ReminderClassification::Overdue(display) => buckets.overdue.push(display),
            ReminderClassification::NoDate(display) => buckets.no_date.push(display),
            ReminderClassification::Future => {}
        }
    }
    if buckets.today.is_empty() && buckets.overdue.is_empty() && buckets.no_date.is_empty() {
        return None;
    }

    let markdown = render_reminder("## 今日待办提醒", &buckets, true);
    let text = render_reminder("【今日待办提醒】", &buckets, false);
    Some(FormattedReminder { markdown, text })
}

fn render_reminder(header: &str, buckets: &ReminderBuckets, markdown: bool) -> String {
    let mut output = String::from(header);
    append_section(&mut output, "今日任务", &buckets.today, markdown);
    append_section(&mut output, "逾期任务", &buckets.overdue, markdown);
    append_section(&mut output, "无日期任务", &buckets.no_date, markdown);
    output.push_str("\n\n查看更多 /todo");
    output
}

fn append_section(output: &mut String, title: &str, items: &[ReminderDisplayItem], markdown: bool) {
    if items.is_empty() {
        return;
    }
    output.push_str(if markdown { "\n\n### " } else { "\n\n" });
    output.push_str(title);
    for item in items.iter().take(MAX_ITEMS_PER_SECTION) {
        output.push_str("\n- ");
        output.push_str(&render_item_line(item, markdown));
    }
    let omitted = items.len().saturating_sub(MAX_ITEMS_PER_SECTION);
    if omitted > 0 {
        output.push_str(&format!("\n- 另有 {omitted} 项未展示"));
    }
}

fn render_item_line(item: &ReminderDisplayItem, markdown: bool) -> String {
    let title = if markdown {
        escape_markdown(&item.title)
    } else {
        item.title.clone()
    };
    match &item.due_label {
        Some(due_label) => format!("{due_label} {title}"),
        None => title,
    }
}

fn classify_item(item: &TodoItem, today: NaiveDate) -> ReminderClassification {
    if let Some(due_at) = non_empty(item.due_at.as_deref()) {
        return local_date_from_timestamp(due_at)
            .map(|date| {
                classify_due_date(
                    date,
                    today,
                    item,
                    Some(format_todo_time_for_display(due_at)),
                )
            })
            .unwrap_or_else(|| ReminderClassification::NoDate(display_item(item, None)));
    }
    if let Some(due_date) = non_empty(item.due_date.as_deref()) {
        return NaiveDate::parse_from_str(due_date, "%Y-%m-%d")
            .map(|date| {
                classify_due_date(
                    date,
                    today,
                    item,
                    Some(format_todo_time_for_display(due_date)),
                )
            })
            .unwrap_or_else(|_| ReminderClassification::NoDate(display_item(item, None)));
    }
    ReminderClassification::NoDate(display_item(item, None))
}

fn classify_due_date(
    due_date: NaiveDate,
    today: NaiveDate,
    item: &TodoItem,
    due_label: Option<String>,
) -> ReminderClassification {
    let display = display_item(item, due_label);
    if due_date == today {
        ReminderClassification::Today(display)
    } else if due_date < today {
        ReminderClassification::Overdue(display)
    } else {
        ReminderClassification::Future
    }
}

fn display_item(item: &TodoItem, due_label: Option<String>) -> ReminderDisplayItem {
    ReminderDisplayItem {
        title: sanitize_title(&item.title),
        due_label,
    }
}

fn sanitize_title(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "未命名待办".to_owned()
    } else {
        collapsed
    }
}

fn escape_markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '*' | '_' | '[' | ']' | '(' | ')' | '`' | '#') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn sent_marker(owner_key: &str, date: NaiveDate) -> String {
    format!("{date}|{owner_key}")
}

fn was_sent_today(markers: &Arc<Mutex<HashSet<String>>>, owner_key: &str, date: NaiveDate) -> bool {
    markers
        .lock()
        .unwrap()
        .contains(&sent_marker(owner_key, date))
}

fn mark_sent_today(markers: &Arc<Mutex<HashSet<String>>>, owner_key: &str, date: NaiveDate) {
    markers.lock().unwrap().insert(sent_marker(owner_key, date));
}

fn prune_sent_markers(markers: &Arc<Mutex<HashSet<String>>>, date: NaiveDate) {
    let prefix = format!("{date}|");
    markers
        .lock()
        .unwrap()
        .retain(|marker| marker.starts_with(&prefix));
}

fn log_skipped_owners(result: &TodoReminderOwnerQueryResult) {
    for skipped in &result.skipped {
        let reason = match skipped.reason {
            TodoReminderOwnerSkipReason::InvalidPrivateScope => "invalid_private_scope",
            TodoReminderOwnerSkipReason::ConflictingPrivateTargets => "conflicting_private_targets",
        };
        warn!(
            owner = %short_hash(&skipped.owner_key),
            reason,
            scope_count = skipped.private_scope_keys.len(),
            scope_hashes = ?hash_values(&skipped.private_scope_keys),
            parsed_target_hashes = ?hash_values(&skipped.parsed_target_ids),
            "todo reminder skipped owner candidate"
        );
    }
}

fn hash_values(values: &[String]) -> Vec<String> {
    values.iter().map(|value| short_hash(value)).collect()
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut output = String::with_capacity(10);
    for byte in digest.iter().take(5) {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn safe_push_error(err: &GatewayPushError) -> String {
    match err {
        GatewayPushError::Status { status, .. } => format!("push endpoint returned {status}"),
        _ => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
    use serde::Deserialize;

    use crate::{
        storage::todo::{TodoItemDraft, TodoTimePrecision},
        storage::{APP_MIGRATIONS, database::SqliteDatabase},
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CapturedPushRequest {
        target_id: String,
        message_type: String,
        text: String,
        fallback_text: Option<String>,
    }

    #[derive(Clone)]
    struct PushServerState {
        requests: Arc<Mutex<Vec<CapturedPushRequest>>>,
        failing_targets: HashSet<String>,
        transient_failures: Arc<Mutex<HashMap<String, usize>>>,
    }

    #[derive(Debug, Deserialize)]
    struct PushPayload {
        target_id: String,
        message_type: String,
        text: String,
        #[serde(default)]
        fallback_text: Option<String>,
    }

    fn test_store() -> TodoStore {
        TodoStore::new(SqliteDatabase::open_temp("qq-maid-todo-reminder", APP_MIGRATIONS).unwrap())
    }

    fn reminder_scheduler(store: TodoStore, endpoint: String) -> TodoReminderScheduler {
        TodoReminderScheduler::new(
            store,
            GatewayPushClient::new(endpoint, None, 5).unwrap(),
            TodoReminderSchedulerConfig {
                enabled: true,
                reminder_time: DailyReminderTime { hour: 9, minute: 0 },
            },
        )
    }

    fn push_status_for_target(state: &PushServerState, target_id: &str) -> StatusCode {
        if state.failing_targets.contains(target_id) {
            return StatusCode::BAD_GATEWAY;
        }
        let mut transient_failures = state.transient_failures.lock().unwrap();
        match transient_failures.get_mut(target_id) {
            Some(remaining) if *remaining > 0 => {
                *remaining -= 1;
                StatusCode::BAD_GATEWAY
            }
            _ => StatusCode::OK,
        }
    }

    async fn spawn_push_server(
        failing_targets: &[&str],
    ) -> (
        String,
        Arc<Mutex<Vec<CapturedPushRequest>>>,
        tokio::task::JoinHandle<()>,
    ) {
        async fn receive_push(
            State(state): State<PushServerState>,
            Json(payload): Json<PushPayload>,
        ) -> (StatusCode, Json<serde_json::Value>) {
            state.requests.lock().unwrap().push(CapturedPushRequest {
                target_id: payload.target_id.clone(),
                message_type: payload.message_type.clone(),
                text: payload.text.clone(),
                fallback_text: payload.fallback_text.clone(),
            });
            let status = push_status_for_target(&state, &payload.target_id);
            (
                status,
                Json(serde_json::json!({ "ok": status.is_success() })),
            )
        }

        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = PushServerState {
            requests: requests.clone(),
            failing_targets: failing_targets
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            transient_failures: Arc::new(Mutex::new(HashMap::new())),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/internal/push", post(receive_push))
            .with_state(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{addr}/internal/push"), requests, handle)
    }

    async fn spawn_push_server_with_transient_failures(
        failing_targets: &[(&str, usize)],
    ) -> (
        String,
        Arc<Mutex<Vec<CapturedPushRequest>>>,
        tokio::task::JoinHandle<()>,
    ) {
        async fn receive_push(
            State(state): State<PushServerState>,
            Json(payload): Json<PushPayload>,
        ) -> (StatusCode, Json<serde_json::Value>) {
            state.requests.lock().unwrap().push(CapturedPushRequest {
                target_id: payload.target_id.clone(),
                message_type: payload.message_type.clone(),
                text: payload.text.clone(),
                fallback_text: payload.fallback_text.clone(),
            });
            let status = push_status_for_target(&state, &payload.target_id);
            (
                status,
                Json(serde_json::json!({ "ok": status.is_success() })),
            )
        }

        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = PushServerState {
            requests: requests.clone(),
            failing_targets: HashSet::new(),
            transient_failures: Arc::new(Mutex::new(
                failing_targets
                    .iter()
                    .map(|(target, count)| ((*target).to_owned(), *count))
                    .collect(),
            )),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/internal/push", post(receive_push))
            .with_state(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{addr}/internal/push"), requests, handle)
    }

    fn create_todo(
        store: &TodoStore,
        owner: &crate::storage::todo::TodoOwner,
        title: &str,
        due_date: Option<&str>,
        due_at: Option<&str>,
    ) {
        store
            .create(
                owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: due_date.map(str::to_owned),
                    due_at: due_at.map(str::to_owned),
                    time_precision: if due_at.is_some() {
                        TodoTimePrecision::DateTime
                    } else if due_date.is_some() {
                        TodoTimePrecision::Date
                    } else {
                        TodoTimePrecision::None
                    },
                },
            )
            .unwrap();
    }

    #[tokio::test]
    async fn run_once_sends_one_private_reminder_per_owner_per_day() {
        let (endpoint, requests, handle) = spawn_push_server(&[]).await;
        let store = test_store();
        let owner_same_scope = TodoStore::owner(Some("u1"), "private:u1");
        let owner_dirty_scope = TodoStore::owner(Some("u1"), "private: u1");
        let future_owner = TodoStore::owner(Some("u2"), "private:u2");
        create_todo(
            &store,
            &owner_same_scope,
            "今天检查日志",
            Some("2026-06-24"),
            None,
        );
        create_todo(
            &store,
            &owner_dirty_scope,
            "昨天补充说明",
            Some("2026-06-23"),
            None,
        );
        create_todo(&store, &future_owner, "明天再做", Some("2026-06-25"), None);

        let scheduler = reminder_scheduler(store, endpoint);
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();

        let first = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(first.sent_owner_count, 1);
        assert_eq!(first.empty_owner_count, 1);
        let captured = requests.lock().unwrap().clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].target_id, "u1");
        assert_eq!(captured[0].message_type, "markdown");
        assert!(captured[0].text.contains("今日任务"));
        assert!(captured[0].text.contains("逾期任务"));
        assert!(captured[0].text.contains("今天检查日志"));
        assert!(captured[0].text.contains("昨天补充说明"));
        assert!(captured[0].text.contains("查看更多 /todo"));
        assert!(!captured[0].text.contains("[1]"));
        assert!(
            captured[0]
                .fallback_text
                .as_deref()
                .unwrap_or_default()
                .contains("查看更多 /todo")
        );

        let second = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(second.already_sent_owner_count, 1);
        assert_eq!(requests.lock().unwrap().len(), 1);

        handle.abort();
    }

    #[tokio::test]
    async fn run_once_future_only_is_silent_and_does_not_mark_sent() {
        let (endpoint, requests, handle) = spawn_push_server(&[]).await;
        let store = test_store();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        create_todo(&store, &owner, "未来任务", Some("2026-06-25"), None);

        let scheduler = reminder_scheduler(store.clone(), endpoint);
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();

        let first = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(first.sent_owner_count, 0);
        assert_eq!(first.empty_owner_count, 1);
        assert!(requests.lock().unwrap().is_empty());

        create_todo(&store, &owner, "今天补记", Some("2026-06-24"), None);
        let second = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(second.sent_owner_count, 1);
        assert_eq!(requests.lock().unwrap().len(), 1);

        handle.abort();
    }

    #[tokio::test]
    async fn run_once_failure_does_not_block_other_owners_and_explicit_rerun_retries_failed_owner()
    {
        let (endpoint, requests, handle) = spawn_push_server(&["u1"]).await;
        let store = test_store();
        let failing_owner = TodoStore::owner(Some("u1"), "private:u1");
        let success_owner = TodoStore::owner(Some("u2"), "private:u2");
        create_todo(
            &store,
            &failing_owner,
            "今天失败一次",
            Some("2026-06-24"),
            None,
        );
        create_todo(
            &store,
            &success_owner,
            "昨天成功一次",
            Some("2026-06-23"),
            None,
        );

        let scheduler = reminder_scheduler(store, endpoint);
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();

        let first = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(first.sent_owner_count, 1);
        assert_eq!(first.failed_owner_count, 1);
        assert_eq!(requests.lock().unwrap().len(), 2);

        let second = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(second.sent_owner_count, 0);
        assert_eq!(second.failed_owner_count, 1);
        assert_eq!(second.already_sent_owner_count, 1);
        let captured = requests.lock().unwrap().clone();
        assert_eq!(captured.len(), 3);
        assert_eq!(
            captured
                .iter()
                .filter(|request| request.target_id == "u1")
                .count(),
            2
        );
        assert_eq!(
            captured
                .iter()
                .filter(|request| request.target_id == "u2")
                .count(),
            1
        );

        handle.abort();
    }

    #[tokio::test]
    async fn scheduled_cycle_retries_failed_owner_on_same_day() {
        let (endpoint, requests, handle) =
            spawn_push_server_with_transient_failures(&[("u1", 1)]).await;
        let store = test_store();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        // 使用当前日期，避免 next_retry_after 中系统时钟日期与硬编码日期不匹配
        // 导致重试窗口关闭，只收到一次推送请求。
        let today = chrono::Local::now().date_naive();
        let today_str = today.format("%Y-%m-%d").to_string();
        create_todo(&store, &owner, "当天自动补跑", Some(&today_str), None);

        let scheduler =
            reminder_scheduler(store, endpoint).with_retry_delay_for_test(Duration::ZERO);

        scheduler.run_scheduled_cycle_for_date(today).await;

        let captured = requests.lock().unwrap().clone();
        assert_eq!(captured.len(), 2);
        assert_eq!(
            captured
                .iter()
                .filter(|request| request.target_id == "u1")
                .count(),
            2
        );

        handle.abort();
    }

    #[test]
    fn formatter_uses_due_at_precedence_and_hides_future_items() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();
        let items = vec![
            TodoItem {
                id: "1".to_owned(),
                user_id: Some("u1".to_owned()),
                scope_key: "private:u1".to_owned(),
                title: "due-at 优先".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-20".to_owned()),
                due_at: Some("2026-06-23T16:30:00+00:00".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                status: crate::storage::todo::TodoStatus::Pending,
                created_at: "2026-06-20T00:00:00+08:00".to_owned(),
                updated_at: "2026-06-20T00:00:00+08:00".to_owned(),
                completed_at: None,
                cancelled_at: None,
            },
            TodoItem {
                id: "2".to_owned(),
                user_id: Some("u1".to_owned()),
                scope_key: "private:u1".to_owned(),
                title: "未来 due_at 覆盖过期 due_date".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-20".to_owned()),
                due_at: Some("2026-06-25 09:00:00".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                status: crate::storage::todo::TodoStatus::Pending,
                created_at: "2026-06-20T00:00:00+08:00".to_owned(),
                updated_at: "2026-06-20T00:00:00+08:00".to_owned(),
                completed_at: None,
                cancelled_at: None,
            },
            TodoItem {
                id: "3".to_owned(),
                user_id: Some("u1".to_owned()),
                scope_key: "private:u1".to_owned(),
                title: "坏 due-at 归无日期".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-20".to_owned()),
                due_at: Some("bad data".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                status: crate::storage::todo::TodoStatus::Pending,
                created_at: "2026-06-20T00:00:00+08:00".to_owned(),
                updated_at: "2026-06-20T00:00:00+08:00".to_owned(),
                completed_at: None,
                cancelled_at: None,
            },
            TodoItem {
                id: "4".to_owned(),
                user_id: Some("u1".to_owned()),
                scope_key: "private:u1".to_owned(),
                title: "无日期任务".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
                status: crate::storage::todo::TodoStatus::Pending,
                created_at: "2026-06-20T00:00:00+08:00".to_owned(),
                updated_at: "2026-06-20T00:00:00+08:00".to_owned(),
                completed_at: None,
                cancelled_at: None,
            },
        ];

        let formatted = format_reminder_message(&items, today).unwrap();

        assert!(formatted.markdown.contains("今日任务"));
        assert!(formatted.markdown.contains("due-at 优先"));
        assert!(!formatted.markdown.contains("未来 due_at 覆盖过期 due_date"));
        assert!(formatted.markdown.contains("无日期任务"));
        assert!(formatted.markdown.contains("坏 due-at 归无日期"));
        assert!(formatted.markdown.contains("查看更多 /todo"));
        assert!(formatted.text.contains("due-at 优先"));
        assert!(formatted.text.contains("无日期任务"));
    }
}
