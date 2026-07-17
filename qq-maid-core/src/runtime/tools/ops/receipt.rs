use qq_maid_common::markdown::{escape_inline, escape_text};

use super::{OpsExecutionResult, OpsExecutionStatus};

pub(super) struct RenderedOpsResult {
    pub text: String,
    pub markdown: String,
}

pub(super) fn render_result(result: &OpsExecutionResult) -> RenderedOpsResult {
    let status = status_label(result.status);
    let elapsed = format_elapsed(result.elapsed);
    let mut text = vec![
        format!("运维任务：{}", result.command),
        format!("状态：{status}"),
    ];
    let mut markdown = vec![
        format!("# 运维任务：{}", escape_inline(&result.command)),
        format!("**状态：** {status}"),
    ];
    if let Some(exit_code) = result.exit_code {
        text.push(format!("退出码：{exit_code}"));
        markdown.push(format!("**退出码：** {exit_code}"));
    }
    text.push(format!("耗时：{elapsed}"));
    markdown.push(format!("**耗时：** {elapsed}"));

    if result.status == OpsExecutionStatus::SpawnFailed {
        text.extend([
            String::new(),
            "错误：".to_owned(),
            "配置的程序无法启动。".to_owned(),
        ]);
        markdown.extend([
            String::new(),
            "**错误：**".to_owned(),
            "配置的程序无法启动。".to_owned(),
        ]);
    }
    append_output(
        &mut text,
        &mut markdown,
        "标准输出",
        &result.stdout,
        result.stdout_truncated,
    );
    append_output(
        &mut text,
        &mut markdown,
        "标准错误",
        &result.stderr,
        result.stderr_truncated,
    );
    RenderedOpsResult {
        text: text.join("\n"),
        markdown: markdown.join("  \n"),
    }
}

fn append_output(
    text: &mut Vec<String>,
    markdown: &mut Vec<String>,
    label: &str,
    output: &str,
    truncated: bool,
) {
    if output.is_empty() && !truncated {
        return;
    }
    text.extend([String::new(), format!("{label}：")]);
    markdown.extend([String::new(), format!("**{label}：**")]);
    if !output.is_empty() {
        text.push(output.to_owned());
        markdown.push(escape_text(output));
    }
    if truncated {
        text.push("（输出已按配置上限截断）".to_owned());
        markdown.push("（输出已按配置上限截断）".to_owned());
    }
}

fn status_label(status: OpsExecutionStatus) -> &'static str {
    match status {
        OpsExecutionStatus::Succeeded => "成功",
        OpsExecutionStatus::Failed => "执行失败",
        OpsExecutionStatus::TimedOut => "执行超时",
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
