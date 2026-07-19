use super::*;

#[test]
fn core_response_keeps_public_fields_from_respond_response() {
    let response = CoreResponse::from(RespondResponse {
        ok: true,
        text: Some("text".to_owned()),
        markdown: Some("**text**".to_owned()),
        output_parts: Vec::new(),
        handled: Some(true),
        session_id: Some("session-1".to_owned()),
        command: Some("chat".to_owned()),
        diagnostics: Some(serde_json::json!({"k":"v"})),
        visible_entity_snapshot: None,
        metrics: LlmMetrics {
            provider: "test".to_owned(),
            model: "test".to_owned(),
            stream: false,
            ttfe_ms: None,
            ttft_ms: None,
            total_latency_ms: 1,
        },
        usage: None,
        error: None,
    });

    // Core→Gateway 正文只通过结构化 output 表达，旧 text/markdown 字段已删除。
    assert_eq!(response.text_content(), Some("text"));
    assert_eq!(response.markdown_content(), Some("**text**"));
    let output = response.output.as_ref().expect("assistant output");
    assert_eq!(output.text_fallback, "text");
    assert_eq!(output.markdown.as_deref(), Some("**text**"));
    assert_eq!(
        output.parts,
        vec![OutputPart::Markdown {
            markdown: "**text**".to_owned()
        }]
    );
    assert_eq!(response.handled, Some(true));
    assert_eq!(response.session_id.as_deref(), Some("session-1"));
    assert_eq!(response.command.as_deref(), Some("chat"));
    assert_eq!(response.diagnostics.unwrap()["k"], "v");
}

#[test]
fn assistant_output_text_builds_plain_fallback_part() {
    let output = AssistantOutput::text("hello");

    assert_eq!(output.text_fallback, "hello");
    assert_eq!(output.markdown, None);
    assert_eq!(
        output.parts,
        vec![OutputPart::Text {
            text: "hello".to_owned()
        }]
    );
}

#[test]
fn core_response_with_output_sets_structured_output() {
    let response = CoreResponse {
        output: None,
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    }
    .with_output(AssistantOutput::markdown("fallback", "# title"));

    assert_eq!(response.text_content(), Some("fallback"));
    assert_eq!(response.markdown_content(), Some("# title"));
    assert_eq!(
        response.output.as_ref().map(|output| output.parts.clone()),
        Some(vec![OutputPart::Markdown {
            markdown: "# title".to_owned()
        }])
    );
}

#[test]
fn text_content_and_markdown_content_read_structured_output() {
    // 正文访问器只读取结构化 output，旧 text/markdown 兼容字段已删除。
    let response = CoreResponse {
        output: Some(AssistantOutput::markdown(
            "structured fallback",
            "# structured",
        )),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    };

    assert_eq!(response.text_content(), Some("structured fallback"));
    assert_eq!(response.markdown_content(), Some("# structured"));
}

#[test]
fn markdown_content_is_none_when_output_only_has_text() {
    // output 仅含纯文本 part（markdown=None）时，markdown_content 返回 None。
    let response = CoreResponse {
        output: Some(AssistantOutput::text("plain")),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    };

    assert_eq!(response.text_content(), Some("plain"));
    assert_eq!(response.markdown_content(), None);
}

#[test]
fn output_absence_without_suppressed_marker_does_not_suppress_reply() {
    let response = CoreResponse {
        output: None,
        handled: Some(false),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    };

    assert_eq!(response.text_content(), None);
    assert_eq!(response.markdown_content(), None);
    assert!(!response.suppresses_reply());
}

#[test]
fn explicit_suppressed_marker_suppresses_reply() {
    let response = CoreResponse {
        output: None,
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: Some(serde_json::json!({
            "suppressed": true,
            "reason": "test_suppressed_response",
        })),
        visible_entity_snapshot: None,
    };

    assert!(response.suppresses_reply());
}
