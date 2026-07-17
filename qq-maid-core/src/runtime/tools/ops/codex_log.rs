use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use super::{OpsExecutionResult, OpsExecutionStatus};

const REDACTED: &str = "[REDACTED]";
const FALLBACK_STDERR_MAX_CHARS: usize = 2000;

#[derive(Debug)]
pub(super) struct CodexLogError(io::Error);

impl CodexLogError {
    pub(super) fn code(&self) -> &'static str {
        match self.0.kind() {
            io::ErrorKind::PermissionDenied => "codex_log_permission_denied",
            io::ErrorKind::AlreadyExists => "codex_log_already_exists",
            _ => "codex_log_write_failed",
        }
    }
}

/// 成功时丢弃 Codex 进度 stderr；非成功时落独立日志，并只把查看提示交给 Outbox。
pub(super) fn prepare_for_delivery(
    log_directory: &Path,
    task_id: &str,
    prompt: &str,
    result: &mut OpsExecutionResult,
) -> Result<Option<PathBuf>, CodexLogError> {
    prepare_for_delivery_with_writer(log_directory, task_id, prompt, result, write_error_log)
}

fn prepare_for_delivery_with_writer<F>(
    log_directory: &Path,
    task_id: &str,
    prompt: &str,
    result: &mut OpsExecutionResult,
    writer: F,
) -> Result<Option<PathBuf>, CodexLogError>
where
    F: FnOnce(&Path, &str, &str, &OpsExecutionResult, &str, bool) -> io::Result<PathBuf>,
{
    if result.status == OpsExecutionStatus::Succeeded {
        result.stderr.clear();
        result.stderr_truncated = false;
        return Ok(None);
    }

    let captured_stderr = std::mem::take(&mut result.stderr);
    let captured_truncated = result.stderr_truncated;
    result.stderr_truncated = false;
    match writer(
        log_directory,
        task_id,
        prompt,
        result,
        &captured_stderr,
        captured_truncated,
    ) {
        Ok(path) => {
            result.stderr = format!("详细错误已写入独立日志，请在服务器查看：{}", path.display());
            Ok(Some(path))
        }
        Err(error) => {
            let redacted = redact_prompt(&captured_stderr, prompt);
            let (summary, summary_truncated) = truncate_chars(&redacted, FALLBACK_STDERR_MAX_CHARS);
            result.stderr_truncated = captured_truncated || summary_truncated;
            result.stderr = if summary.is_empty() {
                "独立错误日志写入失败，且未捕获到可展示的标准错误。".to_owned()
            } else {
                format!("独立错误日志写入失败；以下为已脱敏的有限摘要：\n{summary}")
            };
            Err(CodexLogError(error))
        }
    }
}

#[cfg(test)]
pub(super) fn prepare_for_delivery_with_error(
    log_directory: &Path,
    task_id: &str,
    prompt: &str,
    result: &mut OpsExecutionResult,
    error: io::Error,
) -> Result<Option<PathBuf>, CodexLogError> {
    prepare_for_delivery_with_writer(
        log_directory,
        task_id,
        prompt,
        result,
        move |_, _, _, _, _, _| Err(error),
    )
}

fn write_error_log(
    log_directory: &Path,
    task_id: &str,
    prompt: &str,
    result: &OpsExecutionResult,
    captured_stderr: &str,
    captured_truncated: bool,
) -> io::Result<PathBuf> {
    fs::create_dir_all(log_directory)?;
    #[cfg(unix)]
    fs::set_permissions(log_directory, unix_permissions(0o700))?;

    let path = log_directory.join(format!("{task_id}.log"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    writeln!(file, "任务 ID：{task_id}")?;
    writeln!(file, "状态：{}", result.status.as_str())?;
    writeln!(
        file,
        "退出码：{}",
        result
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "不可用".to_owned())
    )?;
    writeln!(
        file,
        "stderr 已截断：{}",
        if captured_truncated { "是" } else { "否" }
    )?;
    writeln!(file, "\n标准错误：")?;
    file.write_all(redact_prompt(captured_stderr, prompt).as_bytes())?;
    if !captured_stderr.ends_with('\n') {
        writeln!(file)?;
    }
    Ok(path)
}

fn redact_prompt(stderr: &str, prompt: &str) -> String {
    let mut redacted = if prompt.is_empty() {
        stderr.to_owned()
    } else {
        stderr.replace(prompt, REDACTED)
    };
    for line in prompt
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        redacted = redacted.replace(line, REDACTED);
    }
    redacted
}

fn truncate_chars(value: &str, max_chars: usize) -> (String, bool) {
    let mut chars = value.chars();
    let summary = chars.by_ref().take(max_chars).collect::<String>();
    (summary, chars.next().is_some())
}

#[cfg(unix)]
fn unix_permissions(mode: u32) -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    fs::Permissions::from_mode(mode)
}
