//! Core 错误分类、QQ 可见错误映射和安全文本过滤。

use qq_maid_core::service::CoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RespondError {
    #[error("core request failed: {0}")]
    Core(#[from] CoreError),
}

impl RespondError {
    pub fn log_summary(&self) -> String {
        match self {
            Self::Core(error) => format!("{}@{}", error.code, error.stage),
        }
    }

    pub fn qq_visible_kind(&self) -> String {
        match self {
            Self::Core(error) if error.code == "timeout" => "timeout".to_owned(),
            Self::Core(error) if error.code == "config" => "config".to_owned(),
            Self::Core(error) => format!("{}@{}", error.code, error.stage),
        }
    }
}

pub fn respond_error_to_qq_text(err: &RespondError) -> String {
    match err {
        RespondError::Core(error) => {
            respond_error_info_to_qq_text(&error.code, &error.stage, &error.message)
        }
    }
}

fn respond_error_info_to_qq_text(code: &str, stage: &str, message: &str) -> String {
    let code = code.trim();
    let stage = stage.trim();
    let safe_message = sanitize_visible_error_message(message);
    match code {
        "timeout" => "LLM 请求超时，请稍后重试。".to_owned(),
        "config" => "LLM 服务配置未完成，请联系维护者处理".to_owned(),
        "safety_blocked" => {
            "这条消息触发了上游安全拦截，我没法按原样继续。可以换个说法再试。".to_owned()
        }
        "unsupported_input_part" => safe_message.unwrap_or_else(|| {
            "我收到图片或文件了，但当前模型暂时不支持图片/文件理解。你可以补充文字说明，我先帮你记录。".to_owned()
        }),
        "invalid_request" | "bad_request" => safe_message
            .map(|message| format!("请求格式有误：{message}"))
            .unwrap_or_else(|| "请求格式有误，请调整后再试".to_owned()),
        "not_found" => safe_message
            .map(|message| format!("没有找到相关结果：{message}"))
            .unwrap_or_else(|| "没有找到相关结果，请换个说法再试".to_owned()),
        "io_error" => "服务存储暂时不可用，请稍后再试".to_owned(),
        "authentication_failed" => "LLM 服务鉴权失败，请联系维护者处理。".to_owned(),
        "rate_limited" => "LLM 请求受到限流，请稍后重试。".to_owned(),
        "network_error" | "http_error" => "LLM 网络连接失败，请稍后重试。".to_owned(),
        "upstream_unavailable" | "provider_error" => {
            "上游服务暂时不可用，请稍后再试".to_owned()
        }
        _ => safe_message
            .map(|message| format!("处理失败：{message}"))
            .unwrap_or_else(|| format!("处理失败（阶段：{stage}，错误码：{code}）")),
    }
}

/// 只允许把较安全、较短、且不含敏感痕迹的错误文本直接展示给 QQ 用户。
fn sanitize_visible_error_message(message: &str) -> Option<String> {
    let compact = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }

    let lower = compact.to_ascii_lowercase();
    let blocked_fragments = [
        "authorization",
        "bearer ",
        "access_token",
        "refresh_token",
        "token=",
        "secret=",
        "openid",
        "http://",
        "https://",
        "/home/",
        ".env",
        "-----begin",
    ];
    if compact.contains("sk-")
        || compact.contains('\\')
        || blocked_fragments
            .iter()
            .any(|fragment| lower.contains(fragment))
    {
        return None;
    }

    Some(truncate_visible_message(&compact, 120))
}

fn truncate_visible_message(text: &str, limit: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= limit {
        return text.to_owned();
    }
    let keep = limit.saturating_sub(1);
    format!("{}…", chars.into_iter().take(keep).collect::<String>())
}
