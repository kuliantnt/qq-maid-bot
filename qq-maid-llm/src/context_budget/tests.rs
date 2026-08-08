use super::*;
use serde::Serializer;
use serde_json::json;
use std::io::Write;

fn config(limit: usize) -> ContextBudgetConfig {
    ContextBudgetConfig {
        context_window_chars: limit + 10,
        output_reserve_chars: 10,
        protected_recent_turns: 1,
    }
}

#[test]
fn evicts_by_kind_priority_and_keeps_original_order() {
    let items = vec![
        BudgetItem::new(BudgetItemKind::Required, "system", 20),
        BudgetItem::new(BudgetItemKind::Knowledge, "knowledge", 30),
        BudgetItem::new(BudgetItemKind::Memory, "memory", 30),
        BudgetItem::new(BudgetItemKind::OldHistory, "old", 30),
        BudgetItem::new(BudgetItemKind::Session, "session", 30),
        BudgetItem::new(BudgetItemKind::RecentHistoryProtected, "recent", 20),
        BudgetItem::new(BudgetItemKind::Required, "user", 20),
    ];

    let budgeted = apply_context_budget(items, config(90)).unwrap();

    assert_eq!(budgeted.items, vec!["system", "memory", "recent", "user"]);
    assert_eq!(budgeted.report.evicted_chars, 90);
}

#[test]
fn protected_items_exceeding_limit_returns_context_budget_error() {
    let items = vec![
        BudgetItem::new(BudgetItemKind::Required, "system", 60),
        BudgetItem::new(BudgetItemKind::RecentHistoryProtected, "recent", 60),
        BudgetItem::new(BudgetItemKind::OldHistory, "old", 10),
    ];

    let err = apply_context_budget(items, config(100)).unwrap_err();

    assert_eq!(err.code, "context_budget_exceeded");
    assert_eq!(err.stage, "context_budget");
}

#[test]
fn reserve_must_be_smaller_than_context_window() {
    let err = ContextBudgetConfig {
        context_window_chars: 100,
        output_reserve_chars: 100,
        protected_recent_turns: 1,
    }
    .validate()
    .unwrap_err();

    assert_eq!(err.code, "config");
}

#[test]
fn tool_loop_compaction_keeps_call_and_result_pairing() {
    let payload = json!({
        "messages": [
            {"role": "user", "content": "请查资料"},
            {"role": "assistant", "tool_calls": [{"id": "call-1", "type": "function", "function": {"name": "knowledge_search", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "call-1", "content": "重要证据".repeat(80)}
        ],
        "tools": [{"type": "function", "function": {"name": "knowledge_search"}}],
        "tool_choice": "auto"
    });
    let (fitted, disabled) = fit_tool_loop_payload(
        ContextBudgetConfig {
            context_window_chars: 420,
            output_reserve_chars: 40,
            protected_recent_turns: 0,
        },
        payload,
        "tool_loop",
    )
    .unwrap();
    assert!(disabled);
    assert_eq!(fitted["messages"][1]["tool_calls"][0]["id"], "call-1");
    assert_eq!(fitted["messages"][2]["tool_call_id"], "call-1");
    assert!(fitted.get("tools").is_none());
    assert!(fitted.get("tool_choice").is_none());
    assert!(
        fitted["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("工具结果已省略")
    );
}

#[test]
fn short_chat_tool_outputs_are_compacted_as_strings_and_keep_pairing() {
    let payload = json!({
        "messages": [
            {"role": "user", "content": "查资料"},
            {"role": "assistant", "tool_calls": [
                {"id": "call-1", "type": "function", "function": {"name": "search", "arguments": "{}"}},
                {"id": "call-2", "type": "function", "function": {"name": "weather", "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": "call-1", "content": "a".repeat(80)},
            {"role": "tool", "tool_call_id": "call-2", "content": "b".repeat(80)}
        ],
        "tools": [{"type": "function", "function": {"name": "search"}}],
        "tool_choice": "auto",
        "parallel_tool_calls": false
    });
    let (fitted, disabled) = fit_tool_loop_payload(
        ContextBudgetConfig {
            context_window_chars: 440,
            output_reserve_chars: 20,
            protected_recent_turns: 0,
        },
        payload,
        "tool_loop",
    )
    .unwrap();

    assert!(disabled);
    assert!(fitted.get("tools").is_none());
    assert!(fitted.get("tool_choice").is_none());
    assert!(fitted.get("parallel_tool_calls").is_none());
    assert_eq!(fitted["messages"][1]["tool_calls"][0]["id"], "call-1");
    assert_eq!(fitted["messages"][1]["tool_calls"][1]["id"], "call-2");
    for (index, call_id) in [(2, "call-1"), (3, "call-2")] {
        assert_eq!(fitted["messages"][index]["tool_call_id"], call_id);
        assert!(fitted["messages"][index]["content"].is_string());
        assert!(
            fitted["messages"][index]["content"]
                .as_str()
                .unwrap()
                .contains("工具结果已省略")
        );
    }
    serde_json::to_string(&fitted).unwrap();
}

#[test]
fn short_responses_tool_outputs_are_compacted_as_strings_and_keep_pairing() {
    let payload = json!({
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "查资料"}]},
            {"type": "function_call", "call_id": "call-1", "name": "search", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call-1", "output": "a".repeat(80)},
            {"type": "function_call", "call_id": "call-2", "name": "weather", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call-2", "output": "b".repeat(80)}
        ],
        "tools": [{"type": "function", "name": "search"}],
        "parallel_tool_calls": false
    });
    let (fitted, disabled) = fit_tool_loop_payload(
        ContextBudgetConfig {
            context_window_chars: 445,
            output_reserve_chars: 20,
            protected_recent_turns: 0,
        },
        payload,
        "tool_loop",
    )
    .unwrap();

    assert!(disabled);
    assert!(fitted.get("tools").is_none());
    assert!(fitted.get("tool_choice").is_none());
    assert!(fitted.get("parallel_tool_calls").is_none());
    for (index, call_id) in [(2, "call-1"), (4, "call-2")] {
        assert_eq!(fitted["input"][index]["call_id"], call_id);
        assert!(fitted["input"][index]["output"].is_string());
        assert!(
            fitted["input"][index]["output"]
                .as_str()
                .unwrap()
                .contains("工具结果已省略")
        );
    }
    serde_json::to_string(&fitted).unwrap();
}

#[test]
fn responses_tool_loop_counts_structured_images_without_base64_body() {
    let first_data_url = format!("data:image/png;base64,{}", "a".repeat(80_000));
    let second_data_url = format!("data:image/jpeg;base64,{}", "b".repeat(90_000));
    let payload = json!({
        "input": [{
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "依次看这两张图"},
                {"type": "input_image", "image_url": first_data_url},
                {"type": "input_image", "image_url": second_data_url}
            ]
        }],
        "tools": [{"type": "function", "name": "inspect"}]
    });

    let (fitted, disabled) = fit_tool_loop_payload(config(4_000), payload.clone(), "tool_loop")
        .expect("structured image data should use the independent media estimate");

    assert!(!disabled);
    assert_eq!(
        fitted, payload,
        "the real provider payload must stay intact"
    );
    let estimate = tool_loop_budget_estimate(&fitted, "tool_loop").unwrap();
    assert_eq!(estimate.structured_image_count, 2);
    assert!(estimate.structured_image_data_chars > 160_000);
    assert_eq!(estimate.structured_image_budget_chars, 2_048);
    assert!(estimate.budgeted_chars < 4_000);
}

#[test]
fn large_tool_loop_input_exceeds_warning_threshold() {
    // 工具结果把上下文推到告警阈值（100_000 字符）以上时，估算必须如实上报，
    // 供 `fit_tool_loop_payload` 输出大上下文 warn；同时请求仍可在窗口内完成。
    let payload = json!({
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "请完成"}]},
            {"type": "function_call", "call_id": "call-1", "name": "knowledge_search", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call-1", "output": "长证据".repeat(40_000)}
        ],
        "tools": [{"type": "function", "name": "knowledge_search"}]
    });
    let config = ContextBudgetConfig {
        context_window_chars: 200_000,
        output_reserve_chars: 1_000,
        protected_recent_turns: 0,
    };

    let estimate = tool_loop_budget_estimate(&payload, "tool_loop").unwrap();
    assert!(
        estimate.budgeted_chars > LARGE_TOOL_LOOP_WARN_CHARS,
        "estimate {} must exceed warning threshold {}",
        estimate.budgeted_chars,
        LARGE_TOOL_LOOP_WARN_CHARS
    );
    let (fitted, disabled) = fit_tool_loop_payload(config, payload.clone(), "tool_loop").unwrap();
    assert!(
        !disabled,
        "large but in-window input must not force finalization"
    );
    assert_eq!(fitted, payload);
}

#[test]
fn multi_round_tool_loop_input_stays_within_budget() {
    // Tool Loop 多轮推进时，每轮追加 function_call + function_call_output；
    // 预算层必须在任何一轮都把可发送输入压回 `window - reserve` 以内，
    // 防止会话 input 无限增长把请求内存推高（Issue #361）。
    let config = ContextBudgetConfig {
        context_window_chars: 8_000,
        output_reserve_chars: 500,
        protected_recent_turns: 0,
    };
    let max_input_chars = config.effective_input_limit();
    let mut input = vec![json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": "查询并完成"}],
    })];

    for round in 0..5usize {
        input.push(json!({
            "type": "function_call",
            "call_id": format!("call-{round}"),
            "name": "complete_todos",
            "arguments": "{}",
        }));
        input.push(json!({
            "type": "function_call_output",
            "call_id": format!("call-{round}"),
            "output": "数据".repeat(2_000),
        }));
        let payload = json!({"input": input, "tools": [
            {"type": "function", "name": "complete_todos", "parameters": {"type": "object"}}
        ]});
        let (fitted, disabled) = fit_tool_loop_payload(config, payload, "tool_loop").unwrap();
        let estimate = tool_loop_budget_estimate(&fitted, "tool_loop").unwrap();
        assert!(
            estimate.budgeted_chars <= max_input_chars,
            "round {round}: sent input {} chars must stay within {max_input_chars}",
            estimate.budgeted_chars
        );
        if round >= 2 {
            assert!(
                disabled,
                "round {round}: oversized input must enter forced finalization"
            );
        }
    }
}

#[test]
fn chat_tool_loop_counts_structured_images_without_base64_body() {
    let payload = json!({
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "看图"},
                {"type": "image_url", "image_url": {
                    "url": format!("data:image/webp;base64,{}", "a".repeat(100_000))
                }}
            ]
        }],
        "tools": [{"type": "function", "function": {"name": "inspect"}}]
    });

    let (fitted, disabled) = fit_tool_loop_payload(config(2_500), payload.clone(), "tool_loop")
        .expect("chat image data should use the same independent media estimate");

    assert!(!disabled);
    assert_eq!(fitted, payload);
    let estimate = tool_loop_budget_estimate(&fitted, "tool_loop").unwrap();
    assert_eq!(estimate.structured_image_count, 1);
    assert_eq!(estimate.structured_image_budget_chars, 1_024);
}

#[test]
fn plain_text_data_url_is_not_exempt_from_tool_loop_budget() {
    let payload = json!({
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!("data:image/png;base64,{}", "a".repeat(8_000))
            }]
        }],
        "tools": []
    });

    let err = fit_tool_loop_payload(config(2_000), payload, "tool_loop").unwrap_err();

    assert_eq!(err.code, "context_budget_exceeded");
    assert_eq!(err.stage, "tool_loop");
}

#[test]
fn unrelated_image_url_field_is_not_exempt_from_tool_loop_budget() {
    let payload = json!({
        "messages": [{
            "role": "user",
            "content": [{
                "type": "text",
                "image_url": {"url": format!("data:image/png;base64,{}", "a".repeat(8_000))}
            }]
        }],
        "tools": []
    });

    let err = fit_tool_loop_payload(config(2_000), payload, "tool_loop").unwrap_err();

    assert_eq!(err.code, "context_budget_exceeded");
}

struct FailingSerialize;

impl Serialize for FailingSerialize {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(serde::ser::Error::custom("serialize failed"))
    }
}

#[test]
fn estimated_json_chars_returns_error_on_serialize_failure() {
    let err = estimated_json_chars(&FailingSerialize, "context_budget").unwrap_err();

    assert_eq!(err.code, "context_budget_estimate_error");
    assert_eq!(err.stage, "context_budget");
    assert!(err.message.contains("failed to estimate JSON chars"));
}

#[test]
fn json_char_counter_handles_split_utf8_sequences() {
    // 中文（3 字节）与 emoji（4 字节）跨任意字节边界分片写入时，计数必须
    // 与完整字符串的字符数一致，且不保留正文。
    let text = "完成待办🎉 收尾";
    let bytes = text.as_bytes();
    for chunk_size in 1..=4 {
        let mut writer = JsonCharCounter::default();
        for chunk in bytes.chunks(chunk_size) {
            writer.write_all(chunk).unwrap();
        }
        assert_eq!(
            writer.chars(),
            text.chars().count(),
            "chunk size {chunk_size} must not lose or duplicate characters"
        );
    }
}

#[test]
fn estimated_json_chars_counting_matches_string_serialization() {
    // 计数 writer 与旧 `to_string().chars().count()` 口径必须一致，
    // 只是不再保留完整 String 副本。
    let value = json!({
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "中文正文".repeat(20)}]},
            {"type": "function_call_output", "call_id": "call-1", "output": "工具结果正文".repeat(40)}
        ],
        "tools": [{"type": "function", "name": "完成待办"}]
    });
    let expected = serde_json::to_string(&value).unwrap().chars().count();
    assert_eq!(
        estimated_json_chars_counting(&value, "context_budget").unwrap(),
        expected
    );
}

#[test]
fn estimated_json_chars_counting_returns_error_on_serialize_failure() {
    let err = estimated_json_chars_counting(&FailingSerialize, "context_budget").unwrap_err();

    assert_eq!(err.code, "context_budget_estimate_error");
    assert_eq!(err.stage, "context_budget");
    assert!(err.message.contains("failed to estimate JSON chars"));
}
