//! Tool Loop 执行结果的通用编排层。
//!
//! 这里只理解工具结果的通用状态、领域效果和可信响应块顺序，不解析 Todo 等
//! 具体业务字段。各领域适配器负责把单次工具结果转换为 `ResponseBlock`。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::provider::ToolExecutionResult;

use super::common::CommandBody;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::runtime::respond) enum ToolOutcomeStatus {
    Succeeded,
    PendingConfirmation,
    RequiresClarification,
    Failed,
    Skipped,
}

impl ToolOutcomeStatus {
    pub(in crate::runtime::respond) fn from_tool_result(result: &ToolExecutionResult) -> Self {
        if result.output.get("skipped").and_then(Value::as_bool) == Some(true) {
            return Self::Skipped;
        }
        if result
            .output
            .get("requires_confirmation")
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Self::PendingConfirmation;
        }
        if result
            .output
            .get("requires_clarification")
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Self::RequiresClarification;
        }
        if !result.succeeded || result.output.get("ok").and_then(Value::as_bool) == Some(false) {
            return Self::Failed;
        }
        Self::Succeeded
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::PendingConfirmation => "pending_confirmation",
            Self::RequiresClarification => "requires_clarification",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::runtime::respond) enum ToolEffect {
    ReadOnly,
    Created,
    Updated,
    Completed,
    Cancelled,
    Deleted,
    ExternalSideEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::runtime::respond) enum OutcomePresentation {
    Trusted,
    Internal,
    Unhandled,
}

impl OutcomePresentation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Internal => "internal",
            Self::Unhandled => "unhandled",
        }
    }
}

impl ToolEffect {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Deleted => "deleted",
            Self::ExternalSideEffect => "external_side_effect",
        }
    }

    fn is_completed_side_effect(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(in crate::runtime::respond) enum ResponseBlock {
    FactCard(CommandBody),
    MutationReceipt(CommandBody),
    RelatedList(CommandBody),
    Confirmation(CommandBody),
    Clarification(CommandBody),
    Warning(CommandBody),
    Error(CommandBody),
}

impl ResponseBlock {
    fn body(&self) -> &CommandBody {
        match self {
            Self::FactCard(body)
            | Self::MutationReceipt(body)
            | Self::RelatedList(body)
            | Self::Confirmation(body)
            | Self::Clarification(body)
            | Self::Warning(body)
            | Self::Error(body) => body,
        }
    }

    fn order(&self) -> u8 {
        match self {
            Self::FactCard(_) => 0,
            Self::MutationReceipt(_) | Self::RelatedList(_) => 1,
            Self::Confirmation(_) => 2,
            Self::Clarification(_) => 3,
            Self::Error(_) | Self::Warning(_) => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime::respond) struct ToolExecutionOutcome {
    pub tool_name: String,
    pub domain: String,
    pub status: ToolOutcomeStatus,
    pub effect: ToolEffect,
    pub presentation: OutcomePresentation,
    pub blocks: Vec<ResponseBlock>,
    pub error_code: Option<String>,
    pub command: Option<String>,
}

impl ToolExecutionOutcome {
    pub(in crate::runtime::respond) fn generic(result: &ToolExecutionResult) -> Self {
        Self {
            tool_name: result.name.clone(),
            domain: "generic".to_owned(),
            status: ToolOutcomeStatus::from_tool_result(result),
            effect: ToolEffect::ReadOnly,
            presentation: OutcomePresentation::Unhandled,
            blocks: Vec::new(),
            error_code: structured_error_code(&result.output),
            command: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::runtime::respond) enum AgentTurnStatus {
    Succeeded,
    PartialSuccess,
    PendingConfirmation,
    RequiresClarification,
    Failed,
}

impl AgentTurnStatus {
    pub(in crate::runtime::respond) fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::PartialSuccess => "partial_success",
            Self::PendingConfirmation => "pending_confirmation",
            Self::RequiresClarification => "requires_clarification",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime::respond) struct AgentTurnOutcome {
    pub status: AgentTurnStatus,
    pub outcomes: Vec<ToolExecutionOutcome>,
    pub blocks: Vec<ResponseBlock>,
}

impl AgentTurnOutcome {
    pub(in crate::runtime::respond) fn from_outcomes(outcomes: Vec<ToolExecutionOutcome>) -> Self {
        let status = calculate_turn_status(&outcomes);
        let mut indexed_blocks = Vec::new();
        for (outcome_index, outcome) in outcomes.iter().enumerate() {
            for (block_index, block) in outcome.blocks.iter().cloned().enumerate() {
                indexed_blocks.push((block.order(), outcome_index, block_index, block));
            }
        }
        indexed_blocks.sort_by_key(|(order, outcome_index, block_index, _)| {
            (*order, *outcome_index, *block_index)
        });
        let blocks = indexed_blocks
            .into_iter()
            .map(|(_, _, _, block)| block)
            .collect();
        Self {
            status,
            outcomes,
            blocks,
        }
    }

    pub(in crate::runtime::respond) fn can_replace_model_reply(&self) -> bool {
        !self.blocks.is_empty()
            && self
                .outcomes
                .iter()
                .all(|outcome| outcome.presentation != OutcomePresentation::Unhandled)
    }

    pub(in crate::runtime::respond) fn has_unhandled_outcome(&self) -> bool {
        self.outcomes
            .iter()
            .any(|outcome| outcome.presentation == OutcomePresentation::Unhandled)
    }

    pub(in crate::runtime::respond) fn render_body(&self) -> CommandBody {
        let text = self
            .blocks
            .iter()
            .map(|block| block.body().text.trim().to_owned())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let markdown_parts = self
            .blocks
            .iter()
            .map(|block| {
                let body = block.body();
                body.markdown
                    .as_deref()
                    .unwrap_or(body.text.as_str())
                    .trim()
                    .to_owned()
            })
            .filter(|text: &String| !text.is_empty())
            .collect::<Vec<_>>();
        let markdown = if markdown_parts.is_empty() {
            None
        } else {
            Some(markdown_parts.join("\n\n"))
        };
        CommandBody { text, markdown }
    }

    pub(in crate::runtime::respond) fn render_compat_body(&self) -> CommandBody {
        let mut body = self.render_body();
        let unhandled = self
            .outcomes
            .iter()
            .filter(|outcome| outcome.presentation == OutcomePresentation::Unhandled)
            .collect::<Vec<_>>();
        if unhandled.is_empty() {
            return body;
        }

        let mut lines = Vec::new();
        let mut markdown_lines = Vec::new();
        if !body.text.trim().is_empty() {
            lines.push(body.text.trim().to_owned());
            lines.push(String::new());
        }
        if let Some(markdown) = body
            .markdown
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            markdown_lines.push(markdown.trim().to_owned());
            markdown_lines.push(String::new());
        }

        lines.push("⚠️ 部分工具结果未生成确定性展示".to_owned());
        markdown_lines.push("## ⚠️ 部分工具结果未生成确定性展示".to_owned());
        for outcome in unhandled {
            let status_text = match outcome.status {
                ToolOutcomeStatus::Succeeded => "已执行，但当前没有可信展示适配器",
                ToolOutcomeStatus::Failed => "执行失败，当前没有可信错误展示适配器",
                ToolOutcomeStatus::Skipped => "已跳过，当前没有可信展示适配器",
                ToolOutcomeStatus::RequiresClarification => "需要补充信息，当前没有可信展示适配器",
                ToolOutcomeStatus::PendingConfirmation => "需要确认，当前没有可信展示适配器",
            };
            let line = format!("- {}：{}", outcome.tool_name, status_text);
            lines.push(line.clone());
            markdown_lines.push(line);
        }

        body.text = lines.join("\n");
        body.markdown = Some(markdown_lines.join("\n"));
        body
    }

    pub(in crate::runtime::respond) fn primary_command(&self) -> Option<String> {
        [
            ToolOutcomeStatus::Failed,
            ToolOutcomeStatus::RequiresClarification,
            ToolOutcomeStatus::PendingConfirmation,
            ToolOutcomeStatus::Succeeded,
            ToolOutcomeStatus::Skipped,
        ]
        .into_iter()
        .find_map(|status| {
            let iter: Box<dyn Iterator<Item = &ToolExecutionOutcome>> =
                if status == ToolOutcomeStatus::Succeeded {
                    Box::new(self.outcomes.iter().rev())
                } else {
                    Box::new(self.outcomes.iter())
                };
            iter.filter(|outcome| outcome.status == status)
                .find_map(|outcome| outcome.command.clone())
        })
    }

    pub(in crate::runtime::respond) fn primary_error_code(&self) -> Option<String> {
        self.outcomes
            .iter()
            .find(|outcome| {
                matches!(outcome.status, ToolOutcomeStatus::Failed) && outcome.error_code.is_some()
            })
            .and_then(|outcome| outcome.error_code.clone())
            .or_else(|| {
                self.outcomes
                    .iter()
                    .find(|outcome| {
                        matches!(outcome.status, ToolOutcomeStatus::RequiresClarification)
                            && outcome.error_code.is_some()
                    })
                    .and_then(|outcome| outcome.error_code.clone())
            })
            .or_else(|| {
                self.outcomes
                    .iter()
                    .find_map(|outcome| outcome.error_code.clone())
            })
    }

    pub(in crate::runtime::respond) fn diagnostics(&self) -> Value {
        json!({
            "agent_turn_status": self.status.as_str(),
            "tool_outcomes": self.outcomes.iter().map(|outcome| json!({
                "tool": outcome.tool_name,
                "domain": outcome.domain,
                "status": outcome.status.as_str(),
                "effect": outcome.effect.as_str(),
                "presentation": outcome.presentation.as_str(),
                "error_code": outcome.error_code,
            })).collect::<Vec<_>>(),
        })
    }
}

fn calculate_turn_status(outcomes: &[ToolExecutionOutcome]) -> AgentTurnStatus {
    if outcomes.is_empty() {
        return AgentTurnStatus::Succeeded;
    }
    if outcomes
        .iter()
        .all(|outcome| outcome.status == ToolOutcomeStatus::Succeeded)
    {
        return AgentTurnStatus::Succeeded;
    }

    let has_success = outcomes
        .iter()
        .any(|outcome| outcome.status == ToolOutcomeStatus::Succeeded);
    let has_completed_side_effect = outcomes.iter().any(|outcome| {
        outcome.status == ToolOutcomeStatus::Succeeded && outcome.effect.is_completed_side_effect()
    });
    let has_failed_or_skipped = outcomes.iter().any(|outcome| {
        matches!(
            outcome.status,
            ToolOutcomeStatus::Failed | ToolOutcomeStatus::Skipped
        )
    });
    let has_clarification = outcomes
        .iter()
        .any(|outcome| outcome.status == ToolOutcomeStatus::RequiresClarification);
    let has_pending = outcomes
        .iter()
        .any(|outcome| outcome.status == ToolOutcomeStatus::PendingConfirmation);

    if (has_success || has_completed_side_effect)
        && (has_failed_or_skipped || has_clarification || has_pending)
    {
        return AgentTurnStatus::PartialSuccess;
    }
    if has_clarification {
        return AgentTurnStatus::RequiresClarification;
    }
    if has_pending {
        return AgentTurnStatus::PendingConfirmation;
    }
    AgentTurnStatus::Failed
}

fn structured_error_code(output: &Value) -> Option<String> {
    output
        .get("error_code")
        .and_then(Value::as_str)
        .or_else(|| {
            output
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(
        tool: &str,
        domain: &str,
        status: ToolOutcomeStatus,
        effect: ToolEffect,
        blocks: Vec<ResponseBlock>,
    ) -> ToolExecutionOutcome {
        ToolExecutionOutcome {
            tool_name: tool.to_owned(),
            domain: domain.to_owned(),
            status,
            effect,
            presentation: if blocks.is_empty() {
                OutcomePresentation::Internal
            } else {
                OutcomePresentation::Trusted
            },
            blocks,
            error_code: None,
            command: None,
        }
    }

    #[test]
    fn status_uses_ok_false_even_without_error_code() {
        let result = ToolExecutionResult {
            name: "edit_todo".to_owned(),
            output: json!({"ok": false, "message": "failed"}),
            succeeded: false,
        };

        assert_eq!(
            ToolOutcomeStatus::from_tool_result(&result),
            ToolOutcomeStatus::Failed
        );
    }

    #[test]
    fn dependency_skip_has_own_status() {
        let result = ToolExecutionResult {
            name: "complete_todos".to_owned(),
            output: json!({"ok": false, "skipped": true, "reason": "dependency_previous_call_failed"}),
            succeeded: false,
        };

        assert_eq!(
            ToolOutcomeStatus::from_tool_result(&result),
            ToolOutcomeStatus::Skipped
        );
    }

    #[test]
    fn partial_success_keeps_success_and_failure_blocks() {
        let turn = AgentTurnOutcome::from_outcomes(vec![
            outcome(
                "create_todo",
                "todo",
                ToolOutcomeStatus::Succeeded,
                ToolEffect::Created,
                vec![ResponseBlock::MutationReceipt(CommandBody::plain(
                    "✅ 已新增待办",
                ))],
            ),
            outcome(
                "edit_todo",
                "todo",
                ToolOutcomeStatus::Failed,
                ToolEffect::Updated,
                vec![ResponseBlock::Error(CommandBody::plain("⚠️ 编辑失败"))],
            ),
        ]);

        assert_eq!(turn.status, AgentTurnStatus::PartialSuccess);
        assert!(turn.can_replace_model_reply());
        let body = turn.render_body();
        assert!(body.text.contains("✅ 已新增待办"));
        assert!(body.text.contains("⚠️ 编辑失败"));
    }

    #[test]
    fn unhandled_outcome_blocks_full_replacement_even_with_trusted_blocks() {
        let turn = AgentTurnOutcome::from_outcomes(vec![
            outcome(
                "create_todo",
                "todo",
                ToolOutcomeStatus::Succeeded,
                ToolEffect::Created,
                vec![ResponseBlock::MutationReceipt(CommandBody::plain(
                    "✅ 已新增待办",
                ))],
            ),
            ToolExecutionOutcome::generic(&ToolExecutionResult {
                name: "unknown_tool".to_owned(),
                output: json!({"ok": true, "summary": "unadapted result"}),
                succeeded: true,
            }),
        ]);

        assert_eq!(turn.status, AgentTurnStatus::Succeeded);
        assert!(!turn.can_replace_model_reply());
        assert_eq!(
            turn.outcomes[1].presentation,
            OutcomePresentation::Unhandled
        );
    }

    #[test]
    fn simulated_fact_card_and_todo_receipt_are_ordered_by_block_type() {
        let turn = AgentTurnOutcome::from_outcomes(vec![
            outcome(
                "create_todo",
                "todo",
                ToolOutcomeStatus::Succeeded,
                ToolEffect::Created,
                vec![
                    ResponseBlock::MutationReceipt(CommandBody::plain(
                        "✅ 已新增待办\n乘坐 G34 前往北京南",
                    )),
                    ResponseBlock::RelatedList(CommandBody::plain("🚧 当前进行中 · 共 1 项")),
                ],
            ),
            outcome(
                "train_search",
                "train",
                ToolOutcomeStatus::Succeeded,
                ToolEffect::ReadOnly,
                vec![ResponseBlock::FactCard(CommandBody::plain(
                    "🚄 已查到车次\nG34 · 杭州东 → 北京南 · 07:00",
                ))],
            ),
        ]);

        assert_eq!(turn.status, AgentTurnStatus::Succeeded);
        let body = turn.render_body();
        let fact_pos = body.text.find("🚄 已查到车次").unwrap();
        let todo_pos = body.text.find("✅ 已新增待办").unwrap();
        assert!(fact_pos < todo_pos);
    }
}
