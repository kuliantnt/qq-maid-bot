//! Agent Tool Turn 的最终正文合成。
//!
//! Tool Result 的领域投影仍由 `runtime/tools` 完成；本模块只根据明确的回复来源，
//! 决定模型正文、来源、确定性回执和兼容提示如何组成最终 `RespondOutput`。

use super::{
    agent_outcome::{AgentFallbackReason, AgentTurnOutcome},
    common::{CommandBody, join_body_text},
    llm_service::RespondOutput,
};

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

    match source {
        AgentReplySource::DeterministicCommand => {
            if outcome.can_render_deterministic_reply() {
                apply_body(output, outcome.render_body_with_provenance());
            } else {
                apply_body(
                    output,
                    outcome.render_fallback_body(AgentFallbackReason::ToolOutcomeAuthoritative),
                );
            }
        }
        AgentReplySource::NaturalLanguageAgent => {
            if outcome.can_use_model_reply_as_primary() && !output.model_reply_empty {
                append_model_reply_with_supplement(output, outcome);
            } else {
                // 正常成功时模型正文是唯一主体；模型为空、失败或工具状态需要
                // 确定性解释时，才使用已经完成且可信的领域 renderer 降级。
                let reason = if output.model_reply_empty {
                    AgentFallbackReason::ModelReplyUnavailable
                } else {
                    AgentFallbackReason::ToolOutcomeAuthoritative
                };
                apply_body(output, outcome.render_fallback_body(reason));
            }
        }
    }
}

fn append_model_reply_with_supplement(output: &mut RespondOutput, outcome: &AgentTurnOutcome) {
    let supplement = outcome.render_natural_language_supplement();
    if supplement.text.trim().is_empty() {
        return;
    }

    let model_text = output.text.trim();
    let model_markdown = output
        .markdown
        .as_deref()
        .unwrap_or(output.reply.as_str())
        .trim();
    output.text = join_body_text(model_text, supplement.text.trim());

    let markdown = supplement
        .markdown
        .as_deref()
        .unwrap_or(supplement.text.as_str())
        .trim();
    let composed_markdown = join_body_text(model_markdown, markdown);
    output.reply = composed_markdown.clone();
    output.markdown = Some(composed_markdown);
}

fn apply_body(output: &mut RespondOutput, body: CommandBody) {
    output.reply = body.markdown.clone().unwrap_or_else(|| body.text.clone());
    output.text = body.text;
    output.markdown = body.markdown;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::respond::agent_outcome::{
        AgentTurnOutcome, AgentTurnStatus, OutcomePresentation, ProvenanceSource, ResponseBlock,
        ToolEffect, ToolExecutionOutcome, ToolOutcomeStatus,
    };
    use crate::runtime::respond::common::CommandBody;
    use crate::util::metrics::LlmMetrics;
    use qq_maid_common::output_part::{OutputMedia, OutputPart};
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
            display_contract: Default::default(),
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
    fn media_only_model_reply_keeps_attachment_and_trusted_mutation_receipt() {
        let turn = AgentTurnOutcome::from_outcomes(vec![outcome(
            ToolOutcomeStatus::Succeeded,
            ToolEffect::Created,
            "✅ 已新增待办",
        )]);
        let mut output = output("图片已生成。", true);
        output.parts.push(OutputPart::Image {
            media: OutputMedia {
                media_id: Some("generated-image".to_owned()),
                ..OutputMedia::default()
            },
        });

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(output.text, "✅ 已新增待办");
        assert_eq!(output.parts.len(), 1);
    }

    #[test]
    fn empty_model_reply_keeps_source_identity_when_answer_equals_snippet() {
        let answer = "OpenAI 发布了新版本";
        let turn = AgentTurnOutcome::from_outcomes_with_visible_snapshot_and_provenance(
            vec![outcome(
                ToolOutcomeStatus::Succeeded,
                ToolEffect::ReadOnly,
                answer,
            )],
            None,
            vec![ProvenanceSource {
                title: "OpenAI 官方公告".to_owned(),
                url: "https://example.test".to_owned(),
                snippet: answer.to_owned(),
                identity_in_deterministic_body: false,
                snippet_in_deterministic_body: true,
            }],
            Vec::new(),
        );
        let mut output = output("", true);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(output.text.matches(answer).count(), 1);
        assert_eq!(output.text.matches("OpenAI 官方公告").count(), 1);
        assert_eq!(output.text.matches("https://example.test").count(), 1);
    }

    #[test]
    fn natural_language_list_keeps_model_body_without_duplicate_list() {
        let mut list = outcome(
            ToolOutcomeStatus::Succeeded,
            ToolEffect::ReadOnly,
            "1. 待办 A",
        );
        list.blocks = vec![ResponseBlock::RelatedList(CommandBody::dual(
            "待办 · 共 1 项\n1. 待办 A",
            "## 待办 · 共 1 项\n1. 待办 A",
        ))];
        let turn = AgentTurnOutcome::from_outcomes(vec![list]);
        let mut output = output("查询完成", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(output.text, "查询完成");
        assert!(!output.text.contains("待办 · 共 1 项"));
        assert!(!output.text.contains("1. 待办 A"));
    }

    #[test]
    fn internal_success_gets_non_empty_fallback_when_model_reply_fails() {
        let turn = AgentTurnOutcome::from_outcomes(vec![ToolExecutionOutcome {
            tool_name: "knowledge_search".to_owned(),
            domain: "knowledge".to_owned(),
            status: ToolOutcomeStatus::Succeeded,
            effect: ToolEffect::ReadOnly,
            presentation: OutcomePresentation::Internal,
            blocks: Vec::new(),
            error_code: None,
            command: Some("knowledge".to_owned()),
        }]);
        let mut output = output("模型空正文 fallback", true);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(
            output.text,
            "工具已完成，但模型未能整理出可用回复，请稍后重试。"
        );
    }

    #[test]
    fn internal_success_keeps_non_empty_model_reply() {
        let turn = AgentTurnOutcome::from_outcomes(vec![ToolExecutionOutcome {
            tool_name: "knowledge_search".to_owned(),
            domain: "knowledge".to_owned(),
            status: ToolOutcomeStatus::Succeeded,
            effect: ToolEffect::ReadOnly,
            presentation: OutcomePresentation::Internal,
            blocks: Vec::new(),
            error_code: None,
            command: Some("knowledge".to_owned()),
        }]);
        let mut output = output("模型整理出的知识库答案", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(output.text, "模型整理出的知识库答案");
    }

    #[test]
    fn internal_skip_keeps_mixed_turn_model_reply() {
        let turn = AgentTurnOutcome::from_outcomes(vec![
            outcome(
                ToolOutcomeStatus::Succeeded,
                ToolEffect::ReadOnly,
                "可信天气结果",
            ),
            ToolExecutionOutcome {
                tool_name: "knowledge_search".to_owned(),
                domain: "knowledge".to_owned(),
                status: ToolOutcomeStatus::Skipped,
                effect: ToolEffect::ReadOnly,
                presentation: OutcomePresentation::Internal,
                blocks: Vec::new(),
                error_code: None,
                command: Some("knowledge".to_owned()),
            },
        ]);
        let mut output = output("模型综合天气与知识库结果", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert_eq!(output.text, "模型综合天气与知识库结果");
        assert!(!output.text.contains("最终回复生成失败"));
    }

    #[test]
    fn tool_failure_with_model_text_is_not_reported_as_generation_failure() {
        let failed = ToolExecutionOutcome {
            tool_name: "get_weather".to_owned(),
            domain: "weather".to_owned(),
            status: ToolOutcomeStatus::Failed,
            effect: ToolEffect::ReadOnly,
            presentation: OutcomePresentation::Trusted,
            blocks: vec![ResponseBlock::Error(CommandBody::plain(
                "天气查询失败，请检查城市名称",
            ))],
            error_code: Some("provider_error".to_owned()),
            command: Some("weather".to_owned()),
        };
        let internal = ToolExecutionOutcome {
            tool_name: "knowledge_search".to_owned(),
            domain: "knowledge".to_owned(),
            status: ToolOutcomeStatus::Succeeded,
            effect: ToolEffect::ReadOnly,
            presentation: OutcomePresentation::Internal,
            blocks: Vec::new(),
            error_code: None,
            command: Some("knowledge".to_owned()),
        };
        let turn = AgentTurnOutcome::from_outcomes(vec![failed, internal]);
        let mut output = output("模型整理出的正文", false);

        compose_tool_turn_output(&mut output, &turn, AgentReplySource::NaturalLanguageAgent);

        assert!(output.text.contains("天气查询失败"));
        assert!(output.text.contains("部分结果"));
        assert!(!output.text.contains("最终回复生成失败"));
        assert!(!output.text.contains("模型整理出的正文"));
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
