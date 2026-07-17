use std::{
    future::pending,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::watch,
    time::{Instant, sleep, timeout},
};

use super::config::{OpsCodexConfig, OpsCommandConfig};

const READ_BUFFER_BYTES: usize = 8192;
const NORMAL_OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(500);
const TERMINATED_OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(150);
#[cfg(unix)]
const PROCESS_GROUP_TERM_GRACE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpsExecutionStatus {
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    SpawnFailed,
}

impl OpsExecutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::SpawnFailed => "spawn_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsExecutionResult {
    pub command: String,
    pub status: OpsExecutionStatus,
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub error_type: Option<String>,
    /// 非 Unix 平台当前只能终止直接子进程；取消/超时时必须在结果中如实提示。
    pub tree_termination_limited: bool,
}

struct ExecutionSpec {
    program: String,
    args: Vec<String>,
    working_directory: Option<String>,
    timeout: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

pub(super) async fn execute(
    name: &str,
    config: &OpsCommandConfig,
    args: &[String],
) -> OpsExecutionResult {
    execute_spec(
        name,
        ExecutionSpec {
            program: config.program.clone(),
            args: args.to_vec(),
            working_directory: None,
            timeout: Duration::from_secs(config.timeout_seconds),
            max_stdout_bytes: config.max_stdout_bytes,
            max_stderr_bytes: config.max_stderr_bytes,
        },
        None,
        Arc::new(Mutex::new(None)),
    )
    .await
}

pub(super) async fn execute_codex(
    config: &OpsCodexConfig,
    prompt: &str,
    cancellation: watch::Receiver<bool>,
    process_id: Arc<Mutex<Option<u32>>>,
) -> OpsExecutionResult {
    execute_spec(
        "codex",
        ExecutionSpec {
            program: config.program.clone(),
            args: codex_argv(config, prompt),
            working_directory: Some(config.working_directory.clone()),
            timeout: Duration::from_secs(config.timeout_seconds),
            max_stdout_bytes: config.max_stdout_bytes,
            max_stderr_bytes: config.max_stderr_bytes,
        },
        config.cancellable.then_some(cancellation),
        process_id,
    )
    .await
}

pub(super) fn codex_argv(config: &OpsCodexConfig, prompt: &str) -> Vec<String> {
    vec![
        "exec".to_owned(),
        "--profile".to_owned(),
        config.profile.clone(),
        "--sandbox".to_owned(),
        config.sandbox.clone(),
        "--cd".to_owned(),
        config.working_directory.clone(),
        "--color".to_owned(),
        "never".to_owned(),
        "--".to_owned(),
        prompt.to_owned(),
    ]
}

async fn execute_spec(
    name: &str,
    spec: ExecutionSpec,
    mut cancellation: Option<watch::Receiver<bool>>,
    process_id: Arc<Mutex<Option<u32>>>,
) -> OpsExecutionResult {
    let started_at = Instant::now();
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(directory) = &spec.working_directory {
        command.current_dir(directory);
    }
    #[cfg(unix)]
    {
        // 每个 Ops 程序成为独立进程组 leader；取消和超时可覆盖其派生工具进程。
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return spawn_failed(name, started_at.elapsed()),
    };
    *process_id.lock().unwrap() = child.id();

    let stdout_task = child
        .stdout
        .take()
        .map(|stdout| start_capture(stdout, spec.max_stdout_bytes));
    let stderr_task = child
        .stderr
        .take()
        .map(|stderr| start_capture(stderr, spec.max_stderr_bytes));

    let wait_outcome = wait_for_exit(&mut child, spec.timeout, &mut cancellation).await;
    let (status, exit_code, error_type, terminated, tree_termination_limited) = match wait_outcome {
        WaitOutcome::Exited(Ok(exit)) if exit.success() => (
            OpsExecutionStatus::Succeeded,
            exit.code(),
            None,
            false,
            false,
        ),
        WaitOutcome::Exited(Ok(exit)) => (
            OpsExecutionStatus::Failed,
            exit.code(),
            Some("non_zero_exit".to_owned()),
            false,
            false,
        ),
        WaitOutcome::Exited(Err(_)) => {
            let termination = terminate_task(&mut child).await;
            (
                OpsExecutionStatus::Failed,
                termination.exit_status.and_then(|status| status.code()),
                Some("wait_failed".to_owned()),
                true,
                termination.tree_termination_limited,
            )
        }
        WaitOutcome::TimedOut => {
            let termination = terminate_task(&mut child).await;
            (
                OpsExecutionStatus::TimedOut,
                termination.exit_status.and_then(|status| status.code()),
                Some("timed_out".to_owned()),
                true,
                termination.tree_termination_limited,
            )
        }
        WaitOutcome::Cancelled => {
            let termination = terminate_task(&mut child).await;
            (
                OpsExecutionStatus::Cancelled,
                termination.exit_status.and_then(|status| status.code()),
                Some("cancelled".to_owned()),
                true,
                termination.tree_termination_limited,
            )
        }
    };
    let drain_grace = if terminated {
        TERMINATED_OUTPUT_DRAIN_GRACE
    } else {
        NORMAL_OUTPUT_DRAIN_GRACE
    };
    let (stdout, stderr) = tokio::join!(
        finish_capture(stdout_task, drain_grace),
        finish_capture(stderr_task, drain_grace)
    );
    *process_id.lock().unwrap() = None;

    OpsExecutionResult {
        command: name.to_owned(),
        status,
        exit_code,
        elapsed: started_at.elapsed(),
        stdout: sanitize_output(&stdout.bytes),
        stderr: sanitize_output(&stderr.bytes),
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
        error_type,
        tree_termination_limited,
    }
}

enum WaitOutcome {
    Exited(std::io::Result<ExitStatus>),
    TimedOut,
    Cancelled,
}

async fn wait_for_exit(
    child: &mut Child,
    max_runtime: Duration,
    cancellation: &mut Option<watch::Receiver<bool>>,
) -> WaitOutcome {
    tokio::select! {
        result = child.wait() => WaitOutcome::Exited(result),
        _ = sleep(max_runtime) => WaitOutcome::TimedOut,
        _ = cancellation_requested(cancellation) => WaitOutcome::Cancelled,
    }
}

async fn cancellation_requested(cancellation: &mut Option<watch::Receiver<bool>>) {
    let Some(cancellation) = cancellation else {
        pending::<()>().await;
        return;
    };
    loop {
        if *cancellation.borrow() {
            return;
        }
        if cancellation.changed().await.is_err() {
            pending::<()>().await;
            return;
        }
    }
}

struct TerminationOutcome {
    exit_status: Option<ExitStatus>,
    tree_termination_limited: bool,
}

#[cfg(unix)]
async fn terminate_task(child: &mut Child) -> TerminationOutcome {
    let mut tree_termination_limited = false;
    if let Some(pid) = child.id()
        && let Ok(pid) = i32::try_from(pid)
    {
        // SAFETY: 负 PID 只向本任务 spawn 时创建的独立进程组发送固定信号。
        tree_termination_limited |= !signal_process_group(pid, libc::SIGTERM);
        sleep(PROCESS_GROUP_TERM_GRACE).await;
        // 即使 leader 已退出，进程组仍可能由派生进程持有，因此始终补发 SIGKILL。
        tree_termination_limited |= !signal_process_group(pid, libc::SIGKILL);
    } else {
        tree_termination_limited = true;
    }
    let exit_status = timeout(Duration::from_secs(1), child.wait())
        .await
        .ok()
        .and_then(Result::ok);
    tree_termination_limited |= exit_status.is_none();
    TerminationOutcome {
        exit_status,
        tree_termination_limited,
    }
}

#[cfg(unix)]
fn signal_process_group(pid: i32, signal: i32) -> bool {
    // SAFETY: 调用方只传入刚 spawn 的独立进程组 leader PID 和固定 TERM/KILL 信号。
    let result = unsafe { libc::kill(-pid, signal) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
async fn terminate_task(child: &mut Child) -> TerminationOutcome {
    // Windows 首期没有 Job Object：这里只能可靠终止直接子进程，结果和文档必须提示限制。
    let _ = child.kill().await;
    let exit_status = timeout(Duration::from_secs(1), child.wait())
        .await
        .ok()
        .and_then(Result::ok);
    TerminationOutcome {
        exit_status,
        tree_termination_limited: true,
    }
}

fn spawn_failed(name: &str, elapsed: Duration) -> OpsExecutionResult {
    OpsExecutionResult {
        command: name.to_owned(),
        status: OpsExecutionStatus::SpawnFailed,
        exit_code: None,
        elapsed,
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        error_type: Some("spawn_failed".to_owned()),
        tree_termination_limited: false,
    }
}

#[derive(Clone)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

struct CaptureTask {
    handle: tokio::task::JoinHandle<()>,
    output: Arc<Mutex<CapturedOutput>>,
}

fn start_capture(reader: impl AsyncRead + Unpin + Send + 'static, limit: usize) -> CaptureTask {
    let output = Arc::new(Mutex::new(CapturedOutput {
        bytes: Vec::with_capacity(limit.min(READ_BUFFER_BYTES)),
        truncated: false,
    }));
    let task_output = output.clone();
    let handle = tokio::spawn(collect_limited(reader, limit, task_output));
    CaptureTask { handle, output }
}

async fn collect_limited(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
    output: Arc<Mutex<CapturedOutput>>,
) {
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let mut output = output.lock().unwrap();
        let remaining = limit.saturating_sub(output.bytes.len());
        let keep = remaining.min(read);
        output.bytes.extend_from_slice(&buffer[..keep]);
        output.truncated |= keep < read;
    }
}

async fn finish_capture(task: Option<CaptureTask>, max_wait: Duration) -> CapturedOutput {
    let Some(mut task) = task else {
        return CapturedOutput {
            bytes: Vec::new(),
            truncated: false,
        };
    };
    if timeout(max_wait, &mut task.handle).await.is_err() {
        // 主进程退出后，继承管道的派生进程不得无限拖住结果通知。
        task.handle.abort();
        let _ = task.handle.await;
        task.output.lock().unwrap().truncated = true;
    }
    task.output.lock().unwrap().clone()
}

fn sanitize_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\r' | '\t'))
        .collect::<String>()
        .trim()
        .to_owned()
}
