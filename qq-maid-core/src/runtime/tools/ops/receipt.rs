use qq_maid_common::markdown::{escape_inline, escape_text, to_chat_text, to_qq};

use super::{OpsExecutionResult, OpsExecutionStatus};

/// 保守低于 Gateway 默认 5000 字符软限制，为平台 payload 包装保留余量。
pub(super) const OPS_RESULT_PART_MAX_CHARS: usize = 4000;
const OUTPUT_BODY_MAX_RENDERED_CHARS: usize = 3200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderedOpsPart {
    pub text: String,
    pub markdown: String,
}

pub(super) fn render_progress(task_id: &str, elapsed: std::time::Duration) -> RenderedOpsPart {
    let markdown = [
        "# Codex 任务仍在运行".to_owned(),
        format!("**任务 ID：** `{}`", escape_inline(task_id)),
        "**状态：** 仍在运行".to_owned(),
        format!("**已运行：** {}", format_progress_elapsed(elapsed)),
        format!("**取消：** `/ops cancel {}`", escape_inline(task_id)),
    ]
    .join("  \n");
    RenderedOpsPart {
        text: to_chat_text(&markdown),
        markdown,
    }
}

pub(super) fn render_result(
    result: &OpsExecutionResult,
    task_id: Option<&str>,
) -> Vec<RenderedOpsPart> {
    let mut parts = vec![render_summary(result, task_id)];
    append_output_parts(
        &mut parts,
        result,
        task_id,
        "标准输出",
        &result.stdout,
        result.stdout_truncated,
    );
    append_output_parts(
        &mut parts,
        result,
        task_id,
        "标准错误",
        &result.stderr,
        result.stderr_truncated,
    );
    debug_assert!(parts.iter().all(|part| {
        part.text.chars().count() <= OPS_RESULT_PART_MAX_CHARS
            && part.markdown.chars().count() <= OPS_RESULT_PART_MAX_CHARS
    }));
    parts
}

fn render_summary(result: &OpsExecutionResult, task_id: Option<&str>) -> RenderedOpsPart {
    let status = status_label(result.status);
    let elapsed = format_elapsed(result.elapsed);
    let mut markdown = vec![
        format!("# 运维任务：{}", escape_inline(&result.command)),
        format!("**状态：** {status}"),
    ];
    if let Some(task_id) = task_id {
        markdown.insert(1, format!("**任务 ID：** `{}`", escape_inline(task_id)));
    }
    markdown.push(format!(
        "**退出码：** {}",
        result
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "不可用".to_owned())
    ));
    markdown.push(format!("**耗时：** {elapsed}"));
    markdown.push(format!(
        "**输出截断：** {}",
        if result.stdout_truncated || result.stderr_truncated {
            "是"
        } else {
            "否"
        }
    ));

    if result.status == OpsExecutionStatus::SpawnFailed {
        markdown.extend([String::new(), "**错误：** 配置的程序无法启动。".to_owned()]);
    }
    if result.tree_termination_limited {
        let warning = "当前平台只确认终止了直接子进程，派生进程可能仍需人工检查。";
        markdown.extend([String::new(), format!("**终止范围提示：** {warning}")]);
    }
    let markdown = markdown.join("  \n");
    RenderedOpsPart {
        text: to_chat_text(&markdown),
        markdown,
    }
}

fn append_output_parts(
    parts: &mut Vec<RenderedOpsPart>,
    result: &OpsExecutionResult,
    task_id: Option<&str>,
    label: &str,
    output: &str,
    truncated: bool,
) {
    if output.is_empty() && !truncated {
        return;
    }
    let chunks = split_output(output);
    let chunk_count = chunks.len().max(1);
    for (index, chunk) in chunks.iter().enumerate() {
        parts.push(render_output_part(
            result,
            task_id,
            label,
            index + 1,
            chunk_count,
            chunk,
            truncated && index + 1 == chunk_count,
        ));
    }
    if chunks.is_empty() {
        parts.push(render_output_part(
            result, task_id, label, 1, 1, "", truncated,
        ));
    }
}

fn render_output_part(
    result: &OpsExecutionResult,
    task_id: Option<&str>,
    label: &str,
    index: usize,
    count: usize,
    output: &str,
    truncated: bool,
) -> RenderedOpsPart {
    let segment = if count > 1 {
        format!("{label}（{index}/{count}）")
    } else {
        label.to_owned()
    };
    let mut markdown = vec![format!("# 运维任务：{}", escape_inline(&result.command))];
    if let Some(task_id) = task_id {
        markdown.push(format!("**任务 ID：** `{}`", escape_inline(task_id)));
    }
    markdown.extend([String::new(), format!("**{segment}：**")]);
    if !output.is_empty() {
        let output = if result.command == "codex" && label == "标准输出" {
            // Codex 会生成 Markdown 和本地文件链接；统一走 QQ 安全子集，非 HTTP 链接只保留标签。
            to_qq(output)
        } else {
            escape_text(output)
        };
        markdown.push(output);
    }
    if truncated {
        let marker = "（输出已按配置保留上限截断）";
        markdown.push(marker.to_owned());
    }
    let markdown = markdown.join("  \n");
    RenderedOpsPart {
        text: to_chat_text(&markdown),
        markdown,
    }
}

fn split_output(output: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut rendered_chars = 0usize;
    for character in output.chars() {
        let escaped_chars = escape_text(&character.to_string()).chars().count();
        if !current.is_empty()
            && rendered_chars.saturating_add(escaped_chars) > OUTPUT_BODY_MAX_RENDERED_CHARS
        {
            chunks.push(std::mem::take(&mut current));
            rendered_chars = 0;
        }
        current.push(character);
        rendered_chars = rendered_chars.saturating_add(escaped_chars);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn status_label(status: OpsExecutionStatus) -> &'static str {
    match status {
        OpsExecutionStatus::Succeeded => "成功",
        OpsExecutionStatus::Failed => "执行失败",
        OpsExecutionStatus::TimedOut => "执行超时",
        OpsExecutionStatus::Cancelled => "已取消",
        OpsExecutionStatus::SpawnFailed => "启动失败",
    }
}

fn format_elapsed(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds < 10.0 {
        format!("{seconds:.1} 秒")
    } else {
        format!("{seconds:.0} 秒")
    }
}

fn format_progress_elapsed(elapsed: std::time::Duration) -> String {
    let total_seconds = elapsed.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if minutes == 0 {
        format!("{seconds} 秒")
    } else {
        format!("{minutes} 分 {seconds} 秒")
    }
}
