//! Memory Tool 结果到通用 Agent outcome 的领域适配。

use std::str::FromStr;

use qq_maid_llm::provider::ToolExecutionResult;

use crate::runtime::respond::{
    agent_outcome::{
        OutcomePresentation, ResponseBlock, ToolEffect, ToolExecutionOutcome, ToolOutcomeStatus,
    },
    common::CommandBody,
};

use super::{
    MemoryKind, SAVE_MEMORY_TOOL_NAME, format_memory_saved_reply, memory_write_error_reply,
};

pub(crate) fn tool_outcome_from_result(
    result: &ToolExecutionResult,
) -> Option<ToolExecutionOutcome> {
    if result.name != SAVE_MEMORY_TOOL_NAME {
        return None;
    }
    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = result
        .output
        .get("error_code")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let block = match status {
        ToolOutcomeStatus::Succeeded => {
            let scope = string_field(&result.output, "scope")
                .and_then(|value| MemoryKind::from_str(&value).ok())
                .unwrap_or(MemoryKind::Personal);
            let content = string_field(&result.output, "content").unwrap_or_default();
            ResponseBlock::MutationReceipt(CommandBody::plain(format_memory_saved_reply(
                scope, &content,
            )))
        }
        ToolOutcomeStatus::RequiresClarification => {
            ResponseBlock::Clarification(CommandBody::plain(
                string_field(&result.output, "question")
                    .unwrap_or_else(|| "这条记忆是个人记忆，还是当前群画像？".to_owned()),
            ))
        }
        ToolOutcomeStatus::Skipped => {
            ResponseBlock::Warning(CommandBody::plain("本次记忆写入已跳过，没有保存。"))
        }
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => ResponseBlock::Error(
            CommandBody::plain(string_field(&result.output, "message").unwrap_or_else(|| {
                memory_write_error_reply(error_code.as_deref().unwrap_or("memory_write_failed"))
                    .to_owned()
            })),
        ),
    };
    Some(ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "memory".to_owned(),
        status,
        effect: ToolEffect::Created,
        presentation: OutcomePresentation::Trusted,
        blocks: vec![block],
        error_code,
        command: Some("memory".to_owned()),
    })
}

fn string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
