//! Tool Loop 执行结果的通用编排层。
//!
//! 这里只理解工具结果的通用状态、领域效果和可信响应块顺序，不解析 Todo 等
//! 具体业务字段。各领域适配器负责把单次工具结果转换为 `ResponseBlock`。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use qq_maid_llm::provider::ToolExecutionResult;

use crate::service::VisibleEntitySnapshot;

use super::common::{CommandBody, structured_command_body};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolOutcomeStatus {
    Succeeded,
    PendingConfirmation,
    RequiresClarification,
    Failed,
    Skipped,
}

impl ToolOutcomeStatus {
    pub(crate) fn from_tool_result(result: &ToolExecutionResult) -> Self {
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
pub(crate) enum ToolEffect {
    ReadOnly,
    Created,
    Updated,
    Completed,
    Deleted,
    ExternalSideEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutcomePresentation {
    Trusted,
    Internal,
    Unhandled,
}

/// 工具结果可以附带的来源信息。
///
/// 来源是搜索结果的 provenance，不是搜索工具的完整正文；自然语言 Agent 成功
/// 时只把它作为模型正文后的参考信息追加，模型最终失败时才回退到完整结果卡片。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProvenanceSource {
    pub title: String,
    pub url: String,
    pub snippet: String,
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
pub(crate) enum ResponseBlock {
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
pub(crate) struct ToolExecutionOutcome {
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
    pub(crate) fn generic(result: &ToolExecutionResult) -> Self {
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
pub(crate) enum AgentTurnStatus {
    Succeeded,
    PartialSuccess,
    PendingConfirmation,
    RequiresClarification,
    Failed,
}

impl AgentTurnStatus {
    pub(crate) fn as_str(self) -> &'static str {
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
pub(crate) struct AgentTurnOutcome {
    pub status: AgentTurnStatus,
    pub outcomes: Vec<ToolExecutionOutcome>,
    pub blocks: Vec<ResponseBlock>,
    pub provenance: Vec<ProvenanceSource>,
    /// 已启动但尚未形成可信结果的工具；它不是一个 Tool Result，不能进入领域投影。
    pub unknown_result_tools: Vec<String>,
    pub visible_entity_snapshot: Option<VisibleEntitySnapshot>,
}

impl AgentTurnOutcome {
    #[cfg(test)]
    pub(crate) fn from_outcomes(outcomes: Vec<ToolExecutionOutcome>) -> Self {
        Self::from_outcomes_with_visible_snapshot_and_provenance(
            outcomes,
            None,
            Vec::new(),
            Vec::new(),
        )
    }

    pub(crate) fn from_outcomes_with_visible_snapshot_and_provenance(
        outcomes: Vec<ToolExecutionOutcome>,
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
        provenance: Vec<ProvenanceSource>,
        unknown_result_tools: Vec<String>,
    ) -> Self {
        let unknown_result_tools = deduplicate_tool_names(unknown_result_tools);
        let status = calculate_turn_status(&outcomes, !unknown_result_tools.is_empty());
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
            provenance: deduplicate_provenance(provenance),
            unknown_result_tools,
            visible_entity_snapshot,
        }
    }

    /// 当前整轮是否拥有可安全展示的完整确定性结果。
    pub(crate) fn can_render_deterministic_reply(&self) -> bool {
        self.unknown_result_tools.is_empty()
            && !self.blocks.is_empty()
            && self
                .outcomes
                .iter()
                .all(|outcome| outcome.presentation != OutcomePresentation::Unhandled)
    }

    /// 自然语言 Agent 是否可以把模型正文作为本轮唯一主体。
    ///
    /// 成功的写操作也走这里；Todo 成功验真仍由 Todo postprocessor 在合成后执行。
    /// 只有真实失败（`empty_result` 是“查询完成但无证据”的兼容状态）才回到确定性
    /// 错误/回执，避免模型把失败工具说成成功。
    pub(crate) fn can_use_model_reply_as_primary(&self) -> bool {
        if !self.unknown_result_tools.is_empty() || !self.can_render_deterministic_reply() {
            return false;
        }
        self.outcomes.iter().all(|outcome| {
            outcome.status == ToolOutcomeStatus::Succeeded
                || (outcome.status == ToolOutcomeStatus::Failed
                    && outcome.effect == ToolEffect::ReadOnly
                    && outcome.error_code.as_deref() == Some("empty_result"))
        })
    }

    pub(crate) fn has_incomplete_result(&self) -> bool {
        !self.unknown_result_tools.is_empty()
    }

    pub(crate) fn render_provenance(&self) -> CommandBody {
        if self.provenance.is_empty() {
            return CommandBody::plain("");
        }

        let mut text_lines = vec!["参考来源：".to_owned()];
        let mut markdown_lines = vec!["参考来源：".to_owned()];
        for source in &self.provenance {
            let text_reference = source_text_reference(source);
            let markdown_reference = source_markdown_reference(source);
            let text_line = append_source_snippet(text_reference, &source.snippet);
            let markdown_line = append_source_snippet(markdown_reference, &source.snippet);
            text_lines.push(format!("- {text_line}"));
            markdown_lines.push(format!("- {markdown_line}"));
        }

        let mut body = structured_command_body(markdown_lines.join("\n"));
        body.text = text_lines.join("\n");
        body
    }

    pub(crate) fn has_unhandled_outcome(&self) -> bool {
        self.outcomes
            .iter()
            .any(|outcome| outcome.presentation == OutcomePresentation::Unhandled)
    }

    pub(crate) fn render_body(&self) -> CommandBody {
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

    pub(crate) fn render_compat_body(&self) -> CommandBody {
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

    /// 已知结果仍可展示，但未知工具必须以整轮不完整状态追加提示，不能把已知结果
    /// 当成整轮成功，也不能把未知工具伪造成 Tool Result。
    pub(crate) fn render_incomplete_body(&self) -> CommandBody {
        let mut body = if self.has_unhandled_outcome() {
            self.render_compat_body()
        } else {
            self.render_body()
        };
        let text_lines = std::iter::once("⚠️ 部分工具执行结果未知，无法确认是否成功".to_owned())
            .chain(
                self.unknown_result_tools
                    .iter()
                    .map(|tool| format!("- {tool}：执行状态未知")),
            )
            .collect::<Vec<_>>();
        let markdown_lines =
            std::iter::once("## ⚠️ 部分工具执行结果未知，无法确认是否成功".to_owned())
                .chain(
                    self.unknown_result_tools
                        .iter()
                        .map(|tool| format!("- {tool}：执行状态未知")),
                )
                .collect::<Vec<_>>();
        body.text = join_body_with_warning(&body.text, &text_lines.join("\n"));
        body.markdown = Some(join_body_with_warning(
            body.markdown.as_deref().unwrap_or_default(),
            &markdown_lines.join("\n"),
        ));
        body
    }

    pub(crate) fn primary_command(&self) -> Option<String> {
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

    pub(crate) fn primary_error_code(&self) -> Option<String> {
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

    pub(crate) fn diagnostics(&self) -> Value {
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
            "tools_with_unknown_result": self.unknown_result_tools,
        })
    }
}

fn calculate_turn_status(
    outcomes: &[ToolExecutionOutcome],
    has_unknown_result: bool,
) -> AgentTurnStatus {
    if outcomes.is_empty() {
        return if has_unknown_result {
            AgentTurnStatus::Failed
        } else {
            AgentTurnStatus::Succeeded
        };
    }
    let status = if outcomes
        .iter()
        .all(|outcome| outcome.status == ToolOutcomeStatus::Succeeded)
    {
        AgentTurnStatus::Succeeded
    } else {
        let has_success = outcomes
            .iter()
            .any(|outcome| outcome.status == ToolOutcomeStatus::Succeeded);
        let has_completed_side_effect = outcomes.iter().any(|outcome| {
            outcome.status == ToolOutcomeStatus::Succeeded
                && outcome.effect.is_completed_side_effect()
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
            AgentTurnStatus::PartialSuccess
        } else if has_clarification {
            AgentTurnStatus::RequiresClarification
        } else if has_pending {
            AgentTurnStatus::PendingConfirmation
        } else {
            AgentTurnStatus::Failed
        }
    };
    if has_unknown_result && status == AgentTurnStatus::Succeeded {
        AgentTurnStatus::PartialSuccess
    } else {
        status
    }
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

fn deduplicate_provenance(sources: Vec<ProvenanceSource>) -> Vec<ProvenanceSource> {
    let mut unique = Vec::new();
    for source in sources {
        let duplicate = unique.iter().any(|existing: &ProvenanceSource| {
            if !source.url.is_empty() && !existing.url.is_empty() {
                source.url == existing.url
            } else {
                source.title == existing.title
                    && source.url == existing.url
                    && source.snippet == existing.snippet
            }
        });
        if !duplicate {
            unique.push(source);
        }
    }
    unique
}

fn deduplicate_tool_names(names: Vec<String>) -> Vec<String> {
    let mut unique = Vec::new();
    for name in names {
        let name = name.trim();
        if !name.is_empty() && !unique.iter().any(|existing| existing == name) {
            unique.push(name.to_owned());
        }
    }
    unique
}

fn join_body_with_warning(body: &str, warning: &str) -> String {
    if body.trim().is_empty() {
        warning.trim().to_owned()
    } else {
        format!("{}\n\n{}", body.trim(), warning.trim())
    }
}

fn source_text_reference(source: &ProvenanceSource) -> String {
    match (source.title.trim(), source.url.trim()) {
        (title, url) if !title.is_empty() && !url.is_empty() => {
            format!("{title}（{url}）")
        }
        (title, _) if !title.is_empty() => title.to_owned(),
        (_, url) if !url.is_empty() => url.to_owned(),
        _ => "未命名来源".to_owned(),
    }
}

fn source_markdown_reference(source: &ProvenanceSource) -> String {
    match (source.title.trim(), source.url.trim()) {
        (title, url) if !title.is_empty() && !url.is_empty() => {
            format!("[{title}]({url})")
        }
        (title, _) if !title.is_empty() => title.to_owned(),
        (_, url) if !url.is_empty() => url.to_owned(),
        _ => "未命名来源".to_owned(),
    }
}

fn append_source_snippet(reference: String, snippet: &str) -> String {
    let snippet = snippet.trim();
    if snippet.is_empty() {
        reference
    } else {
        format!("{reference}：{snippet}")
    }
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
        assert!(turn.can_render_deterministic_reply());
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
        assert!(!turn.can_render_deterministic_reply());
        assert_eq!(
            turn.outcomes[1].presentation,
            OutcomePresentation::Unhandled
        );
    }

    #[test]
    fn simulated_fact_card_and_light_todo_receipt_are_ordered_by_block_type() {
        let turn = AgentTurnOutcome::from_outcomes(vec![
            outcome(
                "create_todo",
                "todo",
                ToolOutcomeStatus::Succeeded,
                ToolEffect::Created,
                vec![ResponseBlock::MutationReceipt(CommandBody::plain(
                    "✅ 已新增待办\n乘坐 G34 前往北京南",
                ))],
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

    #[test]
    fn readonly_success_preserves_model_reply() {
        let turn = AgentTurnOutcome::from_outcomes(vec![outcome(
            "get_weather",
            "weather",
            ToolOutcomeStatus::Succeeded,
            ToolEffect::ReadOnly,
            vec![ResponseBlock::FactCard(CommandBody::plain(
                "🌦 岱山天气\n当前多云",
            ))],
        )]);

        assert!(turn.can_render_deterministic_reply());
        assert!(turn.can_use_model_reply_as_primary());
    }

    #[test]
    fn readonly_empty_results_preserve_model_reply() {
        let mut empty = outcome(
            "web_search",
            "search",
            ToolOutcomeStatus::Failed,
            ToolEffect::ReadOnly,
            vec![ResponseBlock::Warning(CommandBody::plain("没查到明确结果"))],
        );
        empty.error_code = Some("empty_result".to_owned());
        let turn = AgentTurnOutcome::from_outcomes(vec![empty]);

        assert!(turn.can_use_model_reply_as_primary());
    }

    #[test]
    fn readonly_success_and_empty_result_preserve_model_reply() {
        let success = outcome(
            "web_search",
            "search",
            ToolOutcomeStatus::Succeeded,
            ToolEffect::ReadOnly,
            vec![ResponseBlock::FactCard(CommandBody::plain("有效搜索事实"))],
        );
        let mut empty = outcome(
            "web_search",
            "search",
            ToolOutcomeStatus::Failed,
            ToolEffect::ReadOnly,
            vec![ResponseBlock::Warning(CommandBody::plain("没查到明确结果"))],
        );
        empty.error_code = Some("empty_result".to_owned());
        let turn = AgentTurnOutcome::from_outcomes(vec![success, empty]);

        assert!(turn.can_use_model_reply_as_primary());
    }

    #[test]
    fn mutation_success_still_replaces_model_reply() {
        let turn = AgentTurnOutcome::from_outcomes(vec![outcome(
            "create_todo",
            "todo",
            ToolOutcomeStatus::Succeeded,
            ToolEffect::Created,
            vec![ResponseBlock::MutationReceipt(CommandBody::plain(
                "✅ 已新增待办",
            ))],
        )]);

        assert!(turn.can_render_deterministic_reply());
        assert!(turn.can_use_model_reply_as_primary());
    }
}
