use super::*;

#[test]
fn extract_function_calls_reads_native_responses_items() {
    let body = json!({
        "output": [{
            "type": "function_call",
            "name": "get_weather",
            "call_id": "call_1",
            "arguments": "{\"city\":\"杭州\"}"
        }]
    });

    let calls = extract_function_calls(&body).unwrap();

    assert_eq!(
        calls,
        vec![FunctionCall {
            name: "get_weather".to_owned(),
            call_id: "call_1".to_owned(),
            arguments: "{\"city\":\"杭州\"}".to_owned(),
        }]
    );
}
