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
/// replace 模式不能覆盖用户已经看到的正文。正常增长直接提交新全文；只要候选
/// 改写了已接受正文的任意前缀，就由上层结束旧流并把完整候选作为新的用户可见回复
/// 发送。这样同长度或更长的重生成也不会被错误拼接到旧正文尾部。
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

    CumulativeTextAction::Rollover(incoming.to_owned())
}
