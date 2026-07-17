use std::{
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use qq_maid_common::identity_context::{ConversationKind, IdentitySource};
use uuid::Uuid;

use crate::{
    runtime::{
        notification::{NotificationWorker, NotificationWorkerConfig},
        push::{PushError, PushIntent, PushResult, PushSink, PushTargetType},
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
};

use super::{
    OpsConfig, OpsExecutionStatus, OpsRequestContext, OpsService, execute::execute,
    parse_ops_command,
};

fn test_store() -> crate::storage::notification::NotificationOutboxStore {
    let path = std::env::temp_dir().join(format!("qq-maid-ops-{}.db", Uuid::new_v4()));
    let database = SqliteDatabase::open(path, APP_MIGRATIONS).unwrap();
    crate::storage::notification::NotificationOutboxStore::new(database)
}

fn private_context(user_id: Option<&str>) -> OpsRequestContext {
    OpsRequestContext {
        conversation_kind: ConversationKind::Private,
        conversation_id: Some("private-target".to_owned()),
        user_id: user_id.map(str::to_owned),
        user_identity_source: Some(IdentitySource::Event),
        group_member_role: None,
        platform: "onebot".to_owned(),
        account_id: Some("bot-a".to_owned()),
    }
}

fn group_context(group_id: Option<&str>, role: Option<&str>) -> OpsRequestContext {
    OpsRequestContext {
        conversation_kind: ConversationKind::Group,
        conversation_id: group_id.map(str::to_owned),
        user_id: Some("member-1".to_owned()),
        user_identity_source: Some(IdentitySource::Event),
        group_member_role: role.map(str::to_owned),
        platform: "qq_official".to_owned(),
        account_id: Some("app-a".to_owned()),
    }
}

fn config_with(program: &str, private: bool, group: bool) -> OpsConfig {
    OpsConfig::from_toml(&format!(
        r#"
enabled = true

[private]
enabled = {private}
allowed_user_ids = ["admin-1"]

[group]
enabled = {group}
allowed_group_ids = ["group-1"]

[commands.status]
program = "{program}"
timeout_seconds = 2
max_stdout_bytes = 64
max_stderr_bytes = 64
min_args = 0
max_args = 0

[commands.restart]
program = "{program}"
timeout_seconds = 2
max_stdout_bytes = 64
max_stderr_bytes = 64
min_args = 1
max_args = 1

[commands.restart.args.0]
allowed_values = ["gateway", "core"]

[commands.inspect]
program = "{program}"
timeout_seconds = 2
max_stdout_bytes = 64
max_stderr_bytes = 64
min_args = 1
max_args = 1

[commands.inspect.args.0]
pattern = "[a-z][a-z0-9-]{{0,15}}"
"#
    ))
    .unwrap()
}

#[test]
fn parser_only_accepts_exact_ops_boundary() {
    assert_eq!(
        parse_ops_command(" /ops restart gateway ").unwrap().args,
        vec!["gateway"]
    );
    assert!(parse_ops_command("/opsx status").is_none());
    assert!(parse_ops_command("请执行 /ops status").is_none());
}

#[test]
fn config_defaults_are_fully_disabled() {
    let config = OpsConfig::from_toml("").unwrap();
    assert!(!config.enabled);
    assert!(!config.private.enabled);
    assert!(!config.group.enabled);
    assert!(config.commands.is_empty());
}

#[test]
fn public_example_is_valid_and_disabled() {
    let config = OpsConfig::from_toml(include_str!(
        "../../../../../runtime/config/ops.example.toml"
    ))
    .unwrap();
    assert!(!config.enabled);
    assert!(!config.private.enabled);
    assert!(!config.group.enabled);
}

#[test]
fn config_rejects_relative_program_and_invalid_rules() {
    let relative = r#"
[commands.status]
program = "scripts/status.sh"
min_args = 0
max_args = 0
"#;
    assert!(
        OpsConfig::from_toml(relative)
            .unwrap_err()
            .contains("absolute path")
    );

    let invalid_rule = r#"
[commands.status]
program = "/fixed/status"
min_args = 0
max_args = 1
[commands.status.args.0]
pattern = "["
"#;
    assert!(
        OpsConfig::from_toml(invalid_rule)
            .unwrap_err()
            .contains("pattern is invalid")
    );
}

#[test]
fn disabled_and_unauthorized_requests_do_not_create_tasks() {
    let store = test_store();
    let disabled = OpsService::new(OpsConfig::default(), store.clone());
    let reply = disabled.accept(
        parse_ops_command("/ops status").unwrap(),
        private_context(Some("admin-1")),
    );
    assert!(reply.contains("未启用"));

    let service = OpsService::new(config_with("/fixed/status", false, false), store.clone());
    assert!(
        service
            .accept(
                parse_ops_command("/ops status").unwrap(),
                private_context(Some("admin-1")),
            )
            .contains("未开放私聊")
    );
    let private_service = OpsService::new(config_with("/fixed/status", true, false), store.clone());
    assert!(
        private_service
            .accept(
                parse_ops_command("/ops status").unwrap(),
                private_context(Some("other-user")),
            )
            .contains("没有执行")
    );
    assert!(store.list_all_for_test().unwrap().is_empty());
}

#[test]
fn private_permission_rejects_missing_or_weak_identity_source() {
    let store = test_store();
    let service = OpsService::new(config_with("/fixed/status", true, false), store.clone());
    for source in [
        None,
        Some(IdentitySource::LegacyFallback),
        Some(IdentitySource::TextWeak),
    ] {
        let mut context = private_context(Some("admin-1"));
        context.user_identity_source = source;
        let reply = service.accept(parse_ops_command("/ops status").unwrap(), context);
        assert!(reply.contains("身份来源不可信"));
    }
    assert!(store.list_all_for_test().unwrap().is_empty());
}

#[test]
fn empty_allowed_group_list_denies_all_groups() {
    let config = OpsConfig::from_toml(
        r#"
enabled = true
[group]
enabled = true
allowed_group_ids = []
[commands.status]
program = "/fixed/status"
min_args = 0
max_args = 0
"#,
    )
    .unwrap();
    let store = test_store();
    let service = OpsService::new(config, store.clone());
    let reply = service.accept(
        parse_ops_command("/ops status").unwrap(),
        group_context(Some("group-1"), Some("owner")),
    );
    assert!(reply.contains("当前群聊没有"));
    assert!(store.list_all_for_test().unwrap().is_empty());
}

#[test]
fn group_requires_allowed_group_and_trusted_admin_role() {
    let store = test_store();
    let service = OpsService::new(config_with("/fixed/status", false, true), store.clone());
    for (group_id, role) in [
        (Some("group-2"), Some("owner")),
        (Some("group-1"), Some("member")),
        (Some("group-1"), Some("unknown")),
        (Some("group-1"), None),
        (None, Some("admin")),
    ] {
        let reply = service.accept(
            parse_ops_command("/ops status").unwrap(),
            group_context(group_id, role),
        );
        assert!(!reply.contains("已受理"), "unexpected reply: {reply}");
    }
    assert!(store.list_all_for_test().unwrap().is_empty());
}

#[test]
fn unknown_and_invalid_arguments_are_rejected_before_spawn() {
    let store = test_store();
    let service = OpsService::new(config_with("/fixed/status", true, false), store.clone());
    for input in [
        "/ops missing",
        "/ops restart",
        "/ops restart gateway extra",
        "/ops restart database",
        "/ops inspect INVALID",
    ] {
        let reply = service.accept(
            parse_ops_command(input).unwrap(),
            private_context(Some("admin-1")),
        );
        assert!(!reply.contains("已受理"), "unexpected reply: {reply}");
    }
    assert!(store.list_all_for_test().unwrap().is_empty());
}

#[cfg(unix)]
fn write_script(body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!("qq-maid-ops-{}.sh", Uuid::new_v4()));
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
fn command_config(
    program: &std::path::Path,
    timeout_seconds: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> super::config::OpsCommandConfig {
    let config = OpsConfig::from_toml(&format!(
        r#"
[commands.test]
program = "{}"
timeout_seconds = {timeout_seconds}
max_stdout_bytes = {max_stdout_bytes}
max_stderr_bytes = {max_stderr_bytes}
min_args = 0
max_args = 2
"#,
        program.display()
    ))
    .unwrap();
    config.commands["test"].clone()
}

#[cfg(unix)]
#[tokio::test]
async fn executor_passes_literal_argv_and_uses_exit_status() {
    let script = write_script("printf 'argc=%s\\n' \"$#\"\nprintf '<%s>\\n' \"$@\"\nexit 7");
    let config = command_config(&script, 2, 4096, 4096);
    let result = execute(
        "test",
        &config,
        &["a;b".to_owned(), "$(not-executed)".to_owned()],
    )
    .await;

    assert_eq!(result.status, OpsExecutionStatus::Failed);
    assert_eq!(result.exit_code, Some(7));
    assert!(result.stdout.contains("argc=2"));
    assert!(result.stdout.contains("<a;b>"));
    assert!(result.stdout.contains("<$(not-executed)>"));
}

#[cfg(unix)]
#[tokio::test]
async fn executor_distinguishes_success_timeout_spawn_failure_and_truncation() {
    let success = write_script("printf '123456789'\nprintf 'abcdefghi' >&2");
    let config = command_config(&success, 2, 4, 5);
    let result = execute("test", &config, &[]).await;
    assert_eq!(result.status, OpsExecutionStatus::Succeeded);
    assert_eq!(result.stdout, "1234");
    assert_eq!(result.stderr, "abcde");
    assert!(result.stdout_truncated);
    assert!(result.stderr_truncated);

    let slow = write_script("sleep 2");
    let timeout_config = command_config(&slow, 1, 4096, 4096);
    let timed_out = execute("test", &timeout_config, &[]).await;
    assert_eq!(timed_out.status, OpsExecutionStatus::TimedOut);
    assert!(timed_out.elapsed < Duration::from_millis(1500));

    let missing = std::env::temp_dir().join(format!("missing-{}", Uuid::new_v4()));
    let missing_config = command_config(&missing, 2, 4096, 4096);
    let spawn_failed = execute("test", &missing_config, &[]).await;
    assert_eq!(spawn_failed.status, OpsExecutionStatus::SpawnFailed);
}

#[cfg(unix)]
async fn wait_for_task(
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
struct AlwaysFailSink {
    attempts: Mutex<usize>,
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
    worker.run_once().await.unwrap();
    // Outbox 时间字符串按秒边界比较；跨过当前秒后再领取重试任务。
    tokio::time::sleep(Duration::from_millis(1100)).await;
    worker.run_once().await.unwrap();

    assert_eq!(*sink.attempts.lock().unwrap(), 2);
    assert_eq!(fs::read_to_string(counter).unwrap(), "x");
}
