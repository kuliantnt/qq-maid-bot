//! Agent Tool Turn 的最终正文合成。
//!
//! Tool Result 的领域投影仍由 `runtime/tools` 完成；本模块只根据明确的回复来源，
//! 决定模型正文、来源、确定性回执和兼容提示如何组成最终 `RespondOutput`。

use super::{agent_outcome::AgentTurnOutcome, common::CommandBody, llm_service::RespondOutput};

/// 标记本轮最终正文的业务来源。
///
/// 不能通过用户文本或工具名称反推：普通自然语言 Agent 和确定性命令虽然共用
/// Tool Loop 投影，但最终展示职责不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentReplySource {
    NaturalLanguageAgent,
    DeterministicCommand,
}

/// 根据回复来源完成一次唯一的最终正文合成。
pub(crate) fn compose_tool_turn_output(
    output: &mut RespondOutput,
    outcome: &AgentTurnOutcome,
    source: AgentReplySource,
) {
    if outcome.has_incomplete_result() {
        // 未知工具可能已经产生副作用；已知结果可以保留，但模型成功正文不能
        // 与“状态未知”的警告并列，避免把整轮误报为成功。
        apply_body(output, outcome.render_incomplete_body());
        return;
    }
    if outcome.has_unhandled_outcome() {
        apply_body(output, outcome.render_compat_body());
        return;
    }

    if !outcome.can_render_deterministic_reply() {
        // 只有内部状态、没有可信用户正文时，保留模型输出或普通空回复 fallback；
        // 不凭空把未适配结果包装成成功。
        return;
    }

    match source {
        AgentReplySource::DeterministicCommand => {
            apply_body(output, outcome.render_body());
        }
        AgentReplySource::NaturalLanguageAgent => {
            if outcome.can_use_model_reply_as_primary() && !output.model_reply_empty {
                append_model_reply_with_provenance(output, outcome);
            } else {
                // 正常成功时模型正文是唯一主体；模型为空、失败或工具状态需要
                // 确定性解释时，才使用已经完成且可信的领域 renderer 降级。
                apply_body(output, outcome.render_body());
            }
        }
    }
}

fn append_model_reply_with_provenance(output: &mut RespondOutput, outcome: &AgentTurnOutcome) {
    let provenance = outcome.render_provenance();
    if provenance.text.trim().is_empty() {
        return;
    }

    let model_text = output.text.trim();
    let model_markdown = output
        .markdown
        .as_deref()
        .unwrap_or(output.reply.as_str())
        .trim();
    output.text = join_bodies(model_text, provenance.text.trim());

    let markdown = provenance
        .markdown
        .as_deref()
        .unwrap_or(provenance.text.as_str())
        .trim();
    let composed_markdown = join_bodies(model_markdown, markdown);
    output.reply = composed_markdown.clone();
    output.markdown = Some(composed_markdown);
}

fn apply_body(output: &mut RespondOutput, body: CommandBody) {
    output.reply = body.markdown.clone().unwrap_or_else(|| body.text.clone());
    output.text = body.text;
    output.markdown = body.markdown;
}

fn join_bodies(first: &str, second: &str) -> String {
    match (first.trim(), second.trim()) {
        (first, second) if !first.is_empty() && !second.is_empty() => {
            format!("{first}\n\n{second}")
        }
        (first, _) if !first.is_empty() => first.to_owned(),
        (_, second) => second.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::respond::agent_outcome::{
        AgentTurnOutcome, AgentTurnStatus, OutcomePresentation, ResponseBlock, ToolEffect,
        ToolExecutionOutcome, ToolOutcomeStatus,
    };
    use crate::runtime::respond::common::CommandBody;
    use crate::util::metrics::LlmMetrics;
    use qq_maid_llm::agent_loop::AgentRunDiagnostics;

    fn output(reply: &str, model_reply_empty: bool) -> RespondOutput {
        RespondOutput {
            reply: reply.to_owned(),
            text: reply.to_owned(),
            markdown: None,
            parts: Vec::new(),
            metrics: LlmMetrics {
                provider: "test".to_owned(),
                model: "test".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 0,
            },
            usage: None,
            agent: AgentRunDiagnostics::default(),
            model_reply_empty,
        }
    }

    fn outcome(status: ToolOutcomeStatus, effect: ToolEffect, block: &str) -> ToolExecutionOutcome {
        ToolExecutionOutcome {
            tool_name: "test_tool".to_owned(),
            domain: "test".to_owned(),
            status,
            effect,
            presentation: OutcomePresentation::Trusted,
            blocks: vec![ResponseBlock::FactCard(CommandBody::plain(block))],
            error_code: None,
            command: None,
        }
    }

    #[test]
    fn natural_language_success_keeps_model_as_only_body() {
        let turn = AgentTurnOutcome::from_outcomes(vec![outcome(
            ToolOutcomeStatus::Succeeded,
            ToolEffect::Created,
            "确定性回执",
        )]);
        let mut output = output("模型总结", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(output.text, "模型总结");
        assert!(!output.text.contains("确定性回执"));
    }

    #[test]
    fn natural_language_empty_model_reply_uses_deterministic_fallback() {
        let turn = AgentTurnOutcome::from_outcomes(vec![outcome(
            ToolOutcomeStatus::Succeeded,
            ToolEffect::ReadOnly,
            "确定性结果",
        )]);
        let mut output = output("模型空正文 fallback", true);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(output.text, "确定性结果");
    }

    #[test]
    fn deterministic_command_keeps_existing_card() {
        let turn = AgentTurnOutcome::from_outcomes(vec![outcome(
            ToolOutcomeStatus::Succeeded,
            ToolEffect::Completed,
            "命令回执",
        )]);
        let mut output = output("不应展示的模型正文", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::DeterministicCommand);

        assert_eq!(output.text, "命令回执");
    }

    #[test]
    fn known_result_and_unknown_result_block_model_success_body() {
        let turn = AgentTurnOutcome::from_outcomes_with_visible_snapshot_and_provenance(
            vec![outcome(
                ToolOutcomeStatus::Succeeded,
                ToolEffect::ReadOnly,
                "已确认的查询结果",
            )],
            None,
            Vec::new(),
            vec!["unknown_tool".to_owned()],
        );
        let mut output = output("模型声称所有工具都已成功", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(
            turn.status,
            crate::runtime::respond::agent_outcome::AgentTurnStatus::PartialSuccess
        );
        assert_eq!(turn.outcomes.len(), 1);
        assert!(!output.text.contains("模型声称所有工具都已成功"));
        assert!(output.text.contains("已确认的查询结果"));
        assert!(output.text.contains("unknown_tool"));
        assert!(output.text.contains("状态未知"));
    }

    #[test]
    fn incomplete_tool_loop_keeps_known_result_without_claiming_full_success() {
        let turn =
            AgentTurnOutcome::from_outcomes_with_visible_snapshot_and_provenance_and_incomplete(
                vec![outcome(
                    ToolOutcomeStatus::Succeeded,
                    ToolEffect::ReadOnly,
                    "已确认的天气结果",
                )],
                None,
                Vec::new(),
                Vec::new(),
                true,
            );
        let mut output = output("模型声称后续工具也已完成", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(turn.status, AgentTurnStatus::PartialSuccess);
        assert!(turn.tool_loop_incomplete);
        assert!(!output.text.contains("模型声称后续工具也已完成"));
        assert!(output.text.contains("已确认的天气结果"));
        assert!(output.text.contains("Tool Loop 未完整结束"));
        assert!(output.text.contains("不能视为完整成功"));
    }

    #[test]
    fn only_unknown_result_is_not_treated_as_a_model_success() {
        let turn = AgentTurnOutcome::from_outcomes_with_visible_snapshot_and_provenance(
            Vec::new(),
            None,
            Vec::new(),
            vec!["unknown_write".to_owned()],
        );
        let mut output = output("已成功写入", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(
            turn.status,
            crate::runtime::respond::agent_outcome::AgentTurnStatus::Failed
        );
        assert!(!output.text.contains("已成功写入"));
        assert!(output.text.contains("unknown_write"));
        assert!(output.text.contains("无法确认是否成功"));
    }

    #[test]
    fn known_write_and_unknown_write_keep_only_verified_result_with_warning() {
        let turn = AgentTurnOutcome::from_outcomes_with_visible_snapshot_and_provenance(
            vec![outcome(
                ToolOutcomeStatus::Succeeded,
                ToolEffect::Created,
                "✅ 已新增已确认待办",
            )],
            None,
            Vec::new(),
            vec!["create_todo".to_owned()],
        );
        let mut output = output("两个待办都已成功新增", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert!(!output.text.contains("两个待办都已成功新增"));
        assert!(output.text.contains("✅ 已新增已确认待办"));
        assert!(output.text.contains("create_todo"));
    }
}
