use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::{Instant, timeout},
};

use super::config::OpsCommandConfig;

const READ_BUFFER_BYTES: usize = 8192;
const TIMEOUT_OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpsExecutionStatus {
    Succeeded,
    Failed,
    TimedOut,
    SpawnFailed,
}

impl OpsExecutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
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
}

pub(super) async fn execute(
    name: &str,
    config: &OpsCommandConfig,
    args: &[String],
) -> OpsExecutionResult {
    let started_at = Instant::now();
    let mut command = Command::new(&config.program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            return OpsExecutionResult {
                command: name.to_owned(),
                status: OpsExecutionStatus::SpawnFailed,
                exit_code: None,
                elapsed: started_at.elapsed(),
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                error_type: Some("spawn_failed".to_owned()),
            };
        }
    };

    let stdout_task = child
        .stdout
        .take()
        .map(|stdout| start_capture(stdout, config.max_stdout_bytes));
    let stderr_task = child
        .stderr
        .take()
        .map(|stderr| start_capture(stderr, config.max_stderr_bytes));
    let wait_result = timeout(Duration::from_secs(config.timeout_seconds), child.wait()).await;
    let (status, exit_code, error_type, timed_out) = match wait_result {
        Ok(Ok(exit)) if exit.success() => (OpsExecutionStatus::Succeeded, exit.code(), None, false),
        Ok(Ok(exit)) => (
            OpsExecutionStatus::Failed,
            exit.code(),
            Some("non_zero_exit".to_owned()),
            false,
        ),
        Ok(Err(_)) => (
            OpsExecutionStatus::Failed,
            None,
            Some("wait_failed".to_owned()),
            false,
        ),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (
                OpsExecutionStatus::TimedOut,
                None,
                Some("timed_out".to_owned()),
                true,
            )
        }
    };
    let drain_grace = timed_out.then_some(TIMEOUT_OUTPUT_DRAIN_GRACE);
    let stdout = finish_capture(stdout_task, drain_grace).await;
    let stderr = finish_capture(stderr_task, drain_grace).await;

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

async fn finish_capture(task: Option<CaptureTask>, max_wait: Option<Duration>) -> CapturedOutput {
    match task {
        Some(mut task) => {
            if let Some(max_wait) = max_wait
                && timeout(max_wait, &mut task.handle).await.is_err()
            {
                // 派生进程可能继续持有管道。超时后不再允许它拖住 Ops 结果通知。
                task.handle.abort();
                let _ = task.handle.await;
            } else if max_wait.is_none() {
                let _ = task.handle.await;
            }
            task.output.lock().unwrap().clone()
        }
        None => CapturedOutput {
            bytes: Vec::new(),
            truncated: false,
        },
    }
}

fn sanitize_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\r' | '\t'))
        .collect::<String>()
        .trim()
        .to_owned()
}
