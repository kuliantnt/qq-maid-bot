use qq_maid_common::output_part::AssistantOutput;
use qq_maid_core::service::CoreResponse;

/// 官方 StreamSession 的输入状态常量。
pub(crate) const STREAM_INPUT_GENERATING: u8 = 1;
pub(crate) const STREAM_INPUT_DONE: u8 = 10;

pub(crate) fn completed_response_content(response: &CoreResponse) -> Option<&str> {
    response.markdown_content().or(response.text_content())
}

pub(crate) fn response_from_incomplete_stream_text(content: &str) -> CoreResponse {
    CoreResponse {
        output: Some(AssistantOutput::markdown(content, content)),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
        delivery_hint: None,
    }
}

/// 将模型候选全文与已被 QQ 接受的全文对齐。
///
/// replace 模式不能覆盖用户已经看到的前缀。正常增长直接提交新全文；候选切换
/// 或重生成导致前缀改写时，保留已接受前缀并只追加共同前缀之后可证明的新尾部；
/// 如果候选全文回退，则由上层结束旧流，不能在原会话中覆盖正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CumulativeTextAction {
    Keep,
    Update(String),
    Rollover(String),
}

pub(crate) fn reconcile_cumulative_text(accepted: &str, incoming: &str) -> CumulativeTextAction {
    if incoming.is_empty() || incoming == accepted {
        return CumulativeTextAction::Keep;
    }
    if incoming.starts_with(accepted) {
        return CumulativeTextAction::Update(incoming.to_owned());
    }

    let accepted_chars = accepted.chars().count();
    let incoming_chars = incoming.chars().count();
    if incoming_chars < accepted_chars {
        return CumulativeTextAction::Rollover(incoming.to_owned());
    }

    let common_chars = accepted
        .chars()
        .zip(incoming.chars())
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = incoming
        .char_indices()
        .nth(common_chars)
        .map(|(index, _)| &incoming[index..])
        .unwrap_or_default();
    let mut merged = accepted.to_owned();
    merged.push_str(suffix);
    CumulativeTextAction::Update(merged)
}
