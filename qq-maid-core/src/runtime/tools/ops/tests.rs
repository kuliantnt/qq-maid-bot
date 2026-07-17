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
    OpsConfig, OpsExecutionResult, OpsExecutionStatus, OpsRequestContext, OpsService,
    execute::{codex_argv, execute, execute_codex},
    parse_ops_command,
    receipt::{OPS_RESULT_PART_MAX_CHARS, render_result},
};

mod prompt_boundary;
mod scope_dedupe;

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
        inbound_id: Some(Uuid::new_v4().to_string()),
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
        inbound_id: Some(Uuid::new_v4().to_string()),
    }
}

fn config_with(program: &str, private: bool, group: bool) -> OpsConfig {
    OpsConfig::from_toml(&format!(
        r#"
enabled = true

[private]
enabled = {private}
allowed_user_ids = ["admin-1", "admin-2"]

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
    assert!(!config.codex.enabled);
    assert!(config.commands.is_empty());
}

#[test]
fn parser_preserves_complete_codex_tail_and_normalizes_cancel_aliases() {
    let task = "修复 构建失败；保留 \"引号\" ; | $(not-shell)";
    let parsed = parse_ops_command(&format!("/ops codex {task}")).unwrap();
    assert_eq!(parsed.name.as_deref(), Some("codex"));
    assert_eq!(parsed.trailing_text.as_deref(), Some(task));
    assert!(parsed.args.is_empty());

    for alias in ["cancel", "stop", "kill", "close"] {
        let parsed = parse_ops_command(&format!("/ops {alias} ops-a82f31")).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("cancel"));
        assert_eq!(parsed.args, vec!["ops-a82f31"]);
    }
}

#[test]
fn reserved_builtin_command_names_are_rejected() {
    for name in ["codex", "list", "cancel", "stop", "kill", "close"] {
        let error = OpsConfig::from_toml(&format!(
            r#"
[commands.{name}]
program = "/fixed/program"
min_args = 0
max_args = 0
"#
        ))
        .unwrap_err();
        assert!(error.contains("reserved"), "unexpected error: {error}");
    }
}

#[cfg(unix)]
#[test]
fn enabled_codex_rejects_dangerous_sandbox_and_nonexistent_workdir() {
    let program = write_script("exit 0");
    let invalid_sandbox = format!(
        r#"
[codex]
enabled = true
program = "{}"
working_directory = "{}"
sandbox = "danger-full-access"
"#,
        program.display(),
        std::env::temp_dir().display()
    );
    assert!(
        OpsConfig::from_toml(&invalid_sandbox)
            .unwrap_err()
            .contains("dangerous mode is forbidden")
    );

    let missing_directory = std::env::temp_dir().join(format!("missing-{}", Uuid::new_v4()));
    let invalid_directory = format!(
        r#"
[codex]
enabled = true
program = "{}"
working_directory = "{}"
sandbox = "workspace-write"
"#,
        program.display(),
        missing_directory.display()
    );
    assert!(
        OpsConfig::from_toml(&invalid_directory)
            .unwrap_err()
            .contains("existing directory")
    );
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
fn codex_ops_config(
    program: &std::path::Path,
    working_directory: &std::path::Path,
    timeout_seconds: u64,
    max_concurrent_tasks: usize,
) -> OpsConfig {
    OpsConfig::from_toml(&format!(
        r#"
enabled = true

[private]
enabled = true
allowed_user_ids = ["admin-1", "admin-2"]

[group]
enabled = true
allowed_group_ids = ["group-1", "group-2"]

[codex]
enabled = true
program = "{}"
working_directory = "{}"
timeout_seconds = {timeout_seconds}
max_prompt_bytes = 8000
max_stdout_bytes = 65536
max_stderr_bytes = 65536
profile = "qq-maid-ops"
sandbox = "workspace-write"
cancellable = true
max_concurrent_tasks = {max_concurrent_tasks}
"#,
        program.display(),
        working_directory.display(),
    ))
    .unwrap()
}

#[cfg(unix)]
fn task_id_from_reply(reply: &str) -> String {
    reply
        .lines()
        .find_map(|line| line.strip_prefix("任务 ID："))
        .expect("accepted Codex reply should include task id")
        .to_owned()
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
#[tokio::test]
async fn executor_bounds_pipe_drain_after_success_and_nonzero_exit() {
    let success = write_script("(sleep 2) &\nprintf success\nexit 0");
    let success_result = execute("test", &command_config(&success, 3, 4096, 4096), &[]).await;
    assert_eq!(success_result.status, OpsExecutionStatus::Succeeded);
    assert_eq!(success_result.exit_code, Some(0));
    assert!(success_result.elapsed < Duration::from_millis(1200));
    assert!(success_result.stdout.contains("success"));
    assert!(success_result.stdout_truncated || success_result.stderr_truncated);

    let failed = write_script("(sleep 2) &\nprintf failed >&2\nexit 7");
    let failed_result = execute("test", &command_config(&failed, 3, 4096, 4096), &[]).await;
    assert_eq!(failed_result.status, OpsExecutionStatus::Failed);
    assert_eq!(failed_result.exit_code, Some(7));
    assert!(failed_result.elapsed < Duration::from_millis(1200));
    assert!(failed_result.stderr.contains("failed"));
    assert!(failed_result.stdout_truncated || failed_result.stderr_truncated);
}

#[cfg(unix)]
#[tokio::test]
async fn executor_timeout_kills_process_group_and_finishes_output_collection() {
    let child_pid_file = std::env::temp_dir().join(format!("ops-child-{}", Uuid::new_v4()));
    let script = write_script(&format!(
        "sleep 30 &\necho $! > '{}'\nwait",
        child_pid_file.display()
    ));
    let result = execute("test", &command_config(&script, 1, 4096, 4096), &[]).await;

    assert_eq!(result.status, OpsExecutionStatus::TimedOut);
    assert!(result.elapsed < Duration::from_millis(1800));
    let child_pid: i32 = fs::read_to_string(&child_pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_process_gone(child_pid).await;
}

#[cfg(unix)]
async fn assert_process_gone(pid: i32) {
    for _ in 0..50 {
        // SAFETY: signal 0 only probes the PID captured from the test child; it sends no signal.
        if unsafe { libc::kill(pid, 0) } == -1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("derived process {pid} is still alive");
}

#[cfg(unix)]
#[tokio::test]
async fn codex_uses_fixed_cli_controls_and_single_literal_prompt_argv() {
    let script = write_script("pwd\nprintf '<%s>\\n' \"$@\"");
    let working_directory = std::env::temp_dir();
    let config = codex_ops_config(&script, &working_directory, 3, 1);
    let prompt = "修复 构建失败；保留 \"引号\" ; | $(not-shell)";
    let argv = codex_argv(&config.codex, prompt);

    assert_eq!(argv.last().map(String::as_str), Some(prompt));
    assert_eq!(argv.iter().filter(|arg| arg.as_str() == prompt).count(), 1);
    assert_eq!(
        argv,
        vec![
            "exec",
            "--skip-git-repo-check",
            "--profile",
            "qq-maid-ops",
            "--sandbox",
            "workspace-write",
            "--cd",
            working_directory.to_str().unwrap(),
            "--color",
            "never",
            "--",
            prompt,
        ]
    );

    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let result = execute_codex(&config.codex, prompt, receiver, Arc::new(Mutex::new(None))).await;
    assert_eq!(result.status, OpsExecutionStatus::Succeeded);
    let lines = result.stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.first().copied(), working_directory.to_str());
    assert_eq!(lines.last().copied(), Some(format!("<{prompt}>").as_str()));
    assert!(!std::path::Path::new("not-shell").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn codex_finds_shebang_interpreter_next_to_fixed_program() {
    use std::os::unix::fs::PermissionsExt;

    let directory = std::env::temp_dir().join(format!("qq-maid-codex-path-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let interpreter = directory.join("ops-node");
    fs::write(&interpreter, "#!/bin/sh\nexec /bin/sh \"$@\"\n").unwrap();
    fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o700)).unwrap();
    let program = directory.join("codex");
    fs::write(
        &program,
        "#!/usr/bin/env ops-node\nprintf '<%s>\\n' \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).all(|entry| entry != directory))
            .unwrap_or(true)
    );

    let working_directory = std::env::temp_dir();
    let config = codex_ops_config(&program, &working_directory, 3, 1);
    let prompt = "检查知识库人数";
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let result = execute_codex(&config.codex, prompt, receiver, Arc::new(Mutex::new(None))).await;

    assert_eq!(result.status, OpsExecutionStatus::Succeeded);
    assert!(
        result
            .stdout
            .lines()
            .any(|line| line == format!("<{prompt}>"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn codex_cancel_terminates_derived_process_group() {
    let child_pid_file = std::env::temp_dir().join(format!("codex-child-{}", Uuid::new_v4()));
    let script = write_script(&format!(
        "sleep 30 &\necho $! > '{}'\nwait",
        child_pid_file.display()
    ));
    let config = codex_ops_config(&script, &std::env::temp_dir(), 20, 1);
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let process_id = Arc::new(Mutex::new(None));
    let execution = tokio::spawn({
        let config = config.codex.clone();
        let process_id = process_id.clone();
        async move { execute_codex(&config, "long task", receiver, process_id).await }
    });
    for _ in 0..100 {
        if child_pid_file.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(child_pid_file.is_file());
    cancel.send(true).unwrap();

    let result = execution.await.unwrap();
    assert_eq!(result.status, OpsExecutionStatus::Cancelled);
    assert!(result.elapsed < Duration::from_secs(2));
    let child_pid = fs::read_to_string(child_pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_process_gone(child_pid).await;
}

#[test]
fn long_result_is_split_into_paired_bounded_notification_parts() {
    let result = OpsExecutionResult {
        command: "codex".to_owned(),
        status: OpsExecutionStatus::Failed,
        exit_code: Some(2),
        elapsed: Duration::from_secs(12),
        stdout: format!("stdout-start-{}-stdout-end", "*_[]".repeat(3000)),
        stderr: format!("stderr-start-{}-stderr-end", "`#$".repeat(3000)),
        stdout_truncated: true,
        stderr_truncated: true,
        error_type: Some("non_zero_exit".to_owned()),
        tree_termination_limited: false,
    };
    let parts = render_result(&result, Some("ops-a82f31"));

    assert!(parts.len() > 4);
    assert!(parts.iter().all(|part| {
        part.markdown.chars().count() <= OPS_RESULT_PART_MAX_CHARS
            && part.text.chars().count() <= OPS_RESULT_PART_MAX_CHARS
            && qq_maid_common::markdown::to_chat_text(&part.markdown) == part.text
    }));
    assert!(parts.iter().any(|part| part.text.contains("stdout-start")));
    assert!(parts.iter().any(|part| part.text.contains("stdout-end")));
    assert!(parts.iter().any(|part| part.text.contains("stderr-start")));
    assert!(parts.iter().any(|part| part.text.contains("stderr-end")));
    assert!(
        parts
            .iter()
            .filter(|part| part.text.contains("输出已按配置保留上限截断"))
            .count()
            >= 2
    );
}

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
        codex_ops_config(&script, &std::env::temp_dir(), 60, 1),
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
    worker.run_once().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    worker.run_once().await.unwrap();
    assert_eq!(*sink.attempts.lock().unwrap(), 2);
    assert_eq!(fs::read_to_string(counter).unwrap(), "x");
}

#[cfg(unix)]
#[tokio::test]
async fn group_tasks_are_isolated_by_group_platform_and_account() {
    let script = write_script("sleep 30");
    let store = test_store();
    let service = OpsService::new(
        codex_ops_config(&script, &std::env::temp_dir(), 60, 1),
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
