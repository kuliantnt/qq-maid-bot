//! Tool Loop 执行结果的通用编排层。
//!
//! 这里只理解工具结果的通用状态、领域效果和可信响应块顺序，不解析 Todo 等
//! 具体业务字段。各领域适配器负责把单次工具结果转换为 `ResponseBlock`。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use qq_maid_llm::provider::ToolExecutionResult;

use crate::service::VisibleEntitySnapshot;

use super::common::{CommandBody, join_body_text, structured_command_body};

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
/// 时把它作为模型正文后的参考信息追加，确定性回退时也必须保留一次可信来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProvenanceSource {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// 确定性搜索正文是否已经结构化展示来源身份（title / URL）。
    pub identity_in_deterministic_body: bool,
    /// 确定性搜索正文是否已经结构化展示来源摘要。
    pub snippet_in_deterministic_body: bool,
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
        self.is_side_effect()
    }

    /// 只读结果可以由模型整合；其余 effect 都必须保留服务端的确定性回执。
    fn is_side_effect(self) -> bool {
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

/// 确定性正文接管最终回复的原因。
///
/// 模型没有生成文本与模型文本因工具状态不可信而被弃用是两类不同故障，回退文案
/// 必须据此给出真实定位，不能把业务工具失败误报为模型生成失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentFallbackReason {
    ModelReplyUnavailable,
    ToolOutcomeAuthoritative,
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
    /// 模型已经发出新的 Tool Call，但 Tool Loop 在调用形成结果前终止。
    ///
    /// 这是整轮编排元数据，不是 synthetic Tool Result，不能进入领域投影。
    pub tool_loop_incomplete: bool,
    /// 模型最终正文通过结构化展示契约声明实际展示的 Tool Result 下标。
    ///
    /// 这里只保存通用结果索引；具体领域仍需校验工具成功状态、重试覆盖和自身
    /// 的可见块，不能把模型声明直接当作用户已经看到的业务事实。
    pub published_tool_result_indexes: Vec<usize>,
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

    #[cfg(test)]
    pub(crate) fn from_outcomes_with_visible_snapshot_and_provenance(
        outcomes: Vec<ToolExecutionOutcome>,
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
        provenance: Vec<ProvenanceSource>,
        unknown_result_tools: Vec<String>,
    ) -> Self {
        Self::from_outcomes_with_visible_snapshot_and_provenance_and_incomplete(
            outcomes,
            visible_entity_snapshot,
            provenance,
            unknown_result_tools,
            false,
        )
    }

    pub(crate) fn from_outcomes_with_visible_snapshot_and_provenance_and_incomplete(
        outcomes: Vec<ToolExecutionOutcome>,
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
        provenance: Vec<ProvenanceSource>,
        unknown_result_tools: Vec<String>,
        tool_loop_incomplete: bool,
    ) -> Self {
        let unknown_result_tools = deduplicate_tool_names(unknown_result_tools);
        let incomplete = tool_loop_incomplete || !unknown_result_tools.is_empty();
        let status = calculate_turn_status(&outcomes, incomplete);
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
            tool_loop_incomplete,
            published_tool_result_indexes: Vec::new(),
            visible_entity_snapshot,
        }
    }

    /// 当前整轮是否拥有可安全展示的完整确定性结果。
    pub(crate) fn can_render_deterministic_reply(&self) -> bool {
        !self.has_incomplete_result()
            && !self.outcomes.is_empty()
            && self
                .outcomes
                .iter()
                .all(outcome_has_complete_deterministic_body)
    }

    /// 当前整轮是否至少有一段可安全展示的确定性正文。
    ///
    /// 最终模型失败时，完整结果可以直接降级；混合了 Internal 结果时只能展示
    /// 已知部分并明确警告；一段正文都没有时必须传播原始生成错误。
    pub(crate) fn has_renderable_deterministic_body(&self) -> bool {
        self.outcomes.iter().any(outcome_has_deterministic_body)
    }

    /// 判断 Agent 最终生成失败后是否仍可安全回退到已投影结果。
    ///
    /// 普通失败至少要有一段可信正文；含有未适配结果时由兼容 renderer 保留可信
    /// 部分并追加明确警告。不可确定展示的 Internal 结果不会被伪造成成功，也不应
    /// 让已经确认的 Weather 或 Todo 写入事实被丢掉。
    pub(crate) fn can_render_agent_failure_fallback(&self) -> bool {
        !self.outcomes.is_empty() && self.has_renderable_deterministic_body()
    }

    /// 自然语言 Agent 是否可以把模型正文作为本轮唯一主体。
    ///
    /// 只读结果可以由模型整合；任何可能改变持久化或外部状态的结果都必须回到
    /// 确定性回执。否则模型漏调一项写操作时，仍可能用一段“全部已完成”的总结
    /// 覆盖真实工具轨迹。Internal/Skipped 的只读状态仍可保留同轮有效模型正文。
    pub(crate) fn can_use_model_reply_as_primary(&self) -> bool {
        if self.has_incomplete_result() || self.has_unhandled_outcome() {
            return false;
        }
        if self
            .outcomes
            .iter()
            .any(|outcome| outcome.effect.is_side_effect())
        {
            return false;
        }
        self.outcomes.iter().all(|outcome| {
            // 领域 presenter 可能因截断或畸形的“成功”JSON 生成确定性错误块；
            // 此时 Tool 状态不能反过来吞掉真实投影错误。
            !(outcome.status == ToolOutcomeStatus::Succeeded
                && outcome
                    .blocks
                    .iter()
                    .any(|block| matches!(block, ResponseBlock::Error(_))))
                && (outcome.status == ToolOutcomeStatus::Succeeded
                    || (outcome.status == ToolOutcomeStatus::Skipped
                        && outcome.presentation == OutcomePresentation::Internal)
                    || (outcome.status == ToolOutcomeStatus::Failed
                        && outcome.effect == ToolEffect::ReadOnly
                        && outcome.error_code.as_deref() == Some("empty_result")))
        })
    }

    pub(crate) fn has_incomplete_result(&self) -> bool {
        self.tool_loop_incomplete || !self.unknown_result_tools.is_empty()
    }

    pub(crate) fn render_provenance(&self) -> CommandBody {
        render_provenance_sources(&self.provenance)
    }

    /// 自然语言 Agent 成功时允许追加到模型正文后的用户可见补充内容。
    ///
    /// 这里只追加只读搜索来源；副作用回执和 Todo 列表会在需要时由确定性正文
    /// 接管，不能让模型总结覆盖真实工具结果。
    pub(crate) fn render_natural_language_supplement(&self) -> CommandBody {
        self.render_provenance()
    }

    /// 确定性结果正文并附加尚未嵌入正文的可信来源。
    pub(crate) fn render_body_with_provenance(&self) -> CommandBody {
        let body = self.render_body();
        let provenance = self.render_deterministic_provenance();
        join_command_bodies(&body, &provenance)
    }

    /// 最终模型失败时使用的安全回退。
    ///
    /// 有确定性块时只展示这些已验证内容；只有内部状态而没有用户正文时，返回
    /// 明确的重试提示，不能把空字符串当成成功回复。
    pub(crate) fn render_fallback_body(&self, reason: AgentFallbackReason) -> CommandBody {
        let body = self.render_body_with_provenance();
        if !body.text.trim().is_empty() {
            if self.can_render_deterministic_reply() {
                return body;
            }
            let warning = match reason {
                AgentFallbackReason::ModelReplyUnavailable => CommandBody::dual(
                    "⚠️ 以上只包含可确定展示的部分结果；另有工具结果需要模型整理，但最终回复生成失败，请稍后重试。",
                    "## ⚠️ 结果不完整\n\n以上只包含可确定展示的部分结果；另有工具结果需要模型整理，但最终回复生成失败，请稍后重试。",
                ),
                AgentFallbackReason::ToolOutcomeAuthoritative => CommandBody::dual(
                    "⚠️ 以上只包含可确定展示的部分结果；另有工具结果无法直接展示，请根据以上提示调整后重试。",
                    "## ⚠️ 结果不完整\n\n以上只包含可确定展示的部分结果；另有工具结果无法直接展示，请根据以上提示调整后重试。",
                ),
            };
            return join_command_bodies(&body, &warning);
        }
        if self
            .outcomes
            .iter()
            .any(|outcome| outcome.status == ToolOutcomeStatus::Succeeded)
        {
            return match reason {
                AgentFallbackReason::ModelReplyUnavailable => {
                    CommandBody::plain("工具已完成，但模型未能整理出可用回复，请稍后重试。")
                }
                AgentFallbackReason::ToolOutcomeAuthoritative => CommandBody::plain(
                    "本轮工具结果未能生成可安全展示的正文，请根据工具状态调整后重试。",
                ),
            };
        }
        CommandBody::plain("本轮工具没有生成可直接展示的结果，请稍后重试。")
    }

    pub(crate) fn has_unhandled_outcome(&self) -> bool {
        self.outcomes
            .iter()
            .any(|outcome| outcome.presentation == OutcomePresentation::Unhandled)
    }

    pub(crate) fn render_body(&self) -> CommandBody {
        Self::render_block_bodies(self.blocks.iter())
    }

    pub(crate) fn has_related_list(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::RelatedList(_)))
    }

    fn render_block_bodies<'a, I>(blocks: I) -> CommandBody
    where
        I: IntoIterator<Item = &'a ResponseBlock>,
    {
        let bodies = blocks
            .into_iter()
            .map(ResponseBlock::body)
            .collect::<Vec<_>>();
        let text = bodies
            .iter()
            .map(|body| body.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let markdown_parts = bodies
            .iter()
            .map(|body| {
                body.markdown
                    .as_deref()
                    .unwrap_or(body.text.as_str())
                    .trim()
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();
        let markdown = (!markdown_parts.is_empty()).then(|| markdown_parts.join("\n\n"));
        CommandBody { text, markdown }
    }

    fn render_deterministic_provenance(&self) -> CommandBody {
        let sources = self
            .provenance
            .iter()
            .cloned()
            .filter_map(|mut source| {
                if source.identity_in_deterministic_body {
                    return None;
                }
                if source.snippet_in_deterministic_body {
                    source.snippet.clear();
                }
                Some(source)
            })
            .collect::<Vec<_>>();
        render_provenance_sources(&sources)
    }

    pub(crate) fn render_compat_body(&self) -> CommandBody {
        let mut body = self.render_body_with_provenance();
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

    /// 已知结果仍可展示，但整轮不完整必须追加提示，不能把已知结果当成整轮成功，
    /// 也不能把未完成调用伪造成 Tool Result。
    pub(crate) fn render_incomplete_body(&self) -> CommandBody {
        let body = if self.has_unhandled_outcome() {
            self.render_compat_body()
        } else {
            self.render_body_with_provenance()
        };

        let mut text_lines = Vec::new();
        let mut markdown_lines = Vec::new();
        if self.tool_loop_incomplete {
            text_lines.push("⚠️ Tool Loop 未完整结束，部分工具调用未执行".to_owned());
            text_lines.push("- 以上仅展示已经确认的工具结果，本轮不能视为完整成功".to_owned());
            markdown_lines.push("## ⚠️ Tool Loop 未完整结束，部分工具调用未执行".to_owned());
            markdown_lines.push("- 以上仅展示已经确认的工具结果，本轮不能视为完整成功".to_owned());
        }
        if !self.unknown_result_tools.is_empty() {
            text_lines.push("⚠️ 部分工具执行结果未知，无法确认是否成功".to_owned());
            text_lines.extend(
                self.unknown_result_tools
                    .iter()
                    .map(|tool| format!("- {tool}：执行状态未知")),
            );
            markdown_lines.push("## ⚠️ 部分工具执行结果未知，无法确认是否成功".to_owned());
            markdown_lines.extend(
                self.unknown_result_tools
                    .iter()
                    .map(|tool| format!("- {tool}：执行状态未知")),
            );
        }
        let warning = CommandBody {
            text: text_lines.join("\n"),
            markdown: Some(markdown_lines.join("\n")),
        };
        join_command_bodies(&body, &warning)
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
            "tool_loop_incomplete": self.tool_loop_incomplete,
            "incomplete": self.has_incomplete_result(),
        })
    }
}

fn outcome_has_complete_deterministic_body(outcome: &ToolExecutionOutcome) -> bool {
    outcome.presentation == OutcomePresentation::Trusted && outcome_has_deterministic_body(outcome)
}

fn outcome_has_deterministic_body(outcome: &ToolExecutionOutcome) -> bool {
    outcome.blocks.iter().any(|block| {
        let body = block.body();
        !body.text.trim().is_empty()
            || body
                .markdown
                .as_deref()
                .is_some_and(|markdown| !markdown.trim().is_empty())
    })
}

fn calculate_turn_status(outcomes: &[ToolExecutionOutcome], incomplete: bool) -> AgentTurnStatus {
    if outcomes.is_empty() {
        return if incomplete {
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
    if incomplete && status == AgentTurnStatus::Succeeded {
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
    let mut unique: Vec<ProvenanceSource> = Vec::new();
    for source in sources {
        let duplicate = unique.iter_mut().find(|existing| {
            if !source.url.is_empty() && !existing.url.is_empty() {
                source.url == existing.url
            } else {
                source.title == existing.title
                    && source.url == existing.url
                    && source.snippet == existing.snippet
            }
        });
        if let Some(existing) = duplicate {
            existing.identity_in_deterministic_body |= source.identity_in_deterministic_body;
            existing.snippet_in_deterministic_body |= source.snippet_in_deterministic_body;
        } else {
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

fn render_provenance_sources(sources: &[ProvenanceSource]) -> CommandBody {
    if sources.is_empty() {
        return CommandBody::plain("");
    }

    let mut text_lines = vec!["参考来源：".to_owned()];
    let mut markdown_lines = vec!["参考来源：".to_owned()];
    for source in sources {
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

fn join_command_bodies(first: &CommandBody, second: &CommandBody) -> CommandBody {
    if second.text.trim().is_empty() && second.markdown.as_deref().is_none_or(str::is_empty) {
        return first.clone();
    }
    if first.text.trim().is_empty() && first.markdown.as_deref().is_none_or(str::is_empty) {
        return second.clone();
    }
    let first_markdown = first.markdown.as_deref().unwrap_or(first.text.as_str());
    let second_markdown = second.markdown.as_deref().unwrap_or(second.text.as_str());
    let markdown = join_body_text(first_markdown, second_markdown);
    CommandBody {
        text: join_body_text(&first.text, &second.text),
        markdown: (!markdown.is_empty()).then_some(markdown),
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
mod tests;
