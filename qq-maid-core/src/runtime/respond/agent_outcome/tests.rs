use super::{
    AgentFallbackReason, AgentTurnOutcome, AgentTurnStatus, OutcomePresentation, ProvenanceSource,
    ResponseBlock, ToolEffect, ToolExecutionOutcome, ToolOutcomeStatus,
};
use crate::runtime::respond::common::CommandBody;
use qq_maid_llm::provider::ToolExecutionResult;
use serde_json::json;

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
fn internal_success_requires_model_but_can_keep_valid_model_reply() {
    let turn = AgentTurnOutcome::from_outcomes(vec![outcome(
        "knowledge_search",
        "knowledge",
        ToolOutcomeStatus::Succeeded,
        ToolEffect::ReadOnly,
        Vec::new(),
    )]);

    assert!(!turn.can_render_deterministic_reply());
    assert!(!turn.has_renderable_deterministic_body());
    assert!(turn.can_use_model_reply_as_primary());
}

#[test]
fn internal_skip_does_not_discard_valid_model_reply() {
    let turn = AgentTurnOutcome::from_outcomes(vec![
        outcome(
            "get_weather",
            "weather",
            ToolOutcomeStatus::Succeeded,
            ToolEffect::ReadOnly,
            vec![ResponseBlock::FactCard(CommandBody::plain("可信天气结果"))],
        ),
        outcome(
            "knowledge_search",
            "knowledge",
            ToolOutcomeStatus::Skipped,
            ToolEffect::ReadOnly,
            Vec::new(),
        ),
    ]);

    assert!(turn.can_use_model_reply_as_primary());
}

#[test]
fn mixed_trusted_and_internal_fallback_is_explicitly_partial() {
    let turn = AgentTurnOutcome::from_outcomes(vec![
        outcome(
            "get_weather",
            "weather",
            ToolOutcomeStatus::Succeeded,
            ToolEffect::ReadOnly,
            vec![ResponseBlock::FactCard(CommandBody::plain("可信天气结果"))],
        ),
        outcome(
            "knowledge_search",
            "knowledge",
            ToolOutcomeStatus::Succeeded,
            ToolEffect::ReadOnly,
            Vec::new(),
        ),
    ]);

    assert!(!turn.can_render_deterministic_reply());
    assert!(turn.has_renderable_deterministic_body());
    assert!(turn.can_render_agent_failure_fallback());
    let body = turn.render_fallback_body(AgentFallbackReason::ModelReplyUnavailable);
    assert!(body.text.contains("可信天气结果"));
    assert!(body.text.contains("只包含可确定展示的部分结果"));
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
fn deterministic_fallback_keeps_search_source_once() {
    let turn = AgentTurnOutcome::from_outcomes_with_visible_snapshot_and_provenance(
        vec![outcome(
            "web_search",
            "search",
            ToolOutcomeStatus::Succeeded,
            ToolEffect::ReadOnly,
            vec![ResponseBlock::FactCard(CommandBody::plain("搜索答案"))],
        )],
        None,
        vec![ProvenanceSource {
            title: "官方来源".to_owned(),
            url: "https://example.test/source".to_owned(),
            snippet: "官方摘要".to_owned(),
            identity_in_deterministic_body: false,
            snippet_in_deterministic_body: false,
        }],
        Vec::new(),
    );

    let body = turn.render_body_with_provenance();

    assert_eq!(body.text.matches("官方来源").count(), 1);
    assert_eq!(body.text.matches("https://example.test/source").count(), 1);
    assert_eq!(body.text.matches("官方摘要").count(), 1);
}

#[test]
fn embedded_search_source_is_not_appended_again() {
    let turn = AgentTurnOutcome::from_outcomes_with_visible_snapshot_and_provenance(
        vec![outcome(
            "web_search",
            "search",
            ToolOutcomeStatus::Succeeded,
            ToolEffect::ReadOnly,
            vec![ResponseBlock::FactCard(CommandBody::plain(
                "搜索答案\n官方来源（https://example.test/source）：官方摘要",
            ))],
        )],
        None,
        vec![ProvenanceSource {
            title: "官方来源".to_owned(),
            url: "https://example.test/source".to_owned(),
            snippet: "官方摘要".to_owned(),
            identity_in_deterministic_body: true,
            snippet_in_deterministic_body: true,
        }],
        Vec::new(),
    );

    let body = turn.render_body_with_provenance();

    assert!(!body.text.contains("参考来源："));
    assert_eq!(body.text.matches("官方来源").count(), 1);
}

#[test]
fn mutation_success_requires_deterministic_reply() {
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
    assert!(!turn.can_use_model_reply_as_primary());
}

#[test]
fn succeeded_status_cannot_hide_deterministic_error_block() {
    let turn = AgentTurnOutcome::from_outcomes(vec![outcome(
        "get_train_schedule",
        "train",
        ToolOutcomeStatus::Succeeded,
        ToolEffect::ReadOnly,
        vec![ResponseBlock::Error(CommandBody::plain(
            "车次查询结果无法解析",
        ))],
    )]);

    assert!(!turn.can_use_model_reply_as_primary());
    assert_eq!(
        turn.render_fallback_body(AgentFallbackReason::ToolOutcomeAuthoritative)
            .text,
        "车次查询结果无法解析"
    );
}
