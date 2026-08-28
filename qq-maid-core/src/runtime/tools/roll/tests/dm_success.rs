//! AI DM 成功路径、上下文边界和输入校验测试。

use super::*;

#[tokio::test]
async fn default_d20_entertainment_context_precedes_roll_and_request_contains_no_roll_result() {
    let provider = MockProvider::replying(fortune_json());
    let events = provider.events.clone();
    let requests = provider.requests.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let roller_events = events.clone();
    let reply = execute_roll_command_with_roller(
        &provider,
        Some("mock:dm".to_owned()),
        RollCommand::DmCheck {
            expression: None,
            query: "晚上要不要出门".to_owned(),
        },
        Duration::from_secs(1),
        move |sides| {
            assert_eq!(sides, 20);
            roller_events.lock().unwrap().push("rng");
            14
        },
    )
    .await;

    assert_eq!(*events.lock().unwrap(), ["model", "rng"]);
    assert!(reply.reply.contains("难度：中等（DC 11）"));
    assert!(reply.reply.contains("投掷：14"));
    assert!(reply.reply.contains("✅ 成功"));
    assert!(reply.reply.contains("今晚适合出门。"));
    assert_eq!(reply.metrics.provider, "mock");
    assert_eq!(reply.metrics.model, "mock:dm");
    assert_eq!(reply.diagnostics()["roll_execution_kind"], "ai_dm_success");
    assert_eq!(reply.diagnostics()["dice_expression"], "1d20");
    assert_eq!(reply.diagnostics()["dice_minimum"], 1);
    assert_eq!(reply.diagnostics()["dice_maximum"], 20);
    assert_eq!(reply.diagnostics()["difficulty"], "medium");
    assert_eq!(reply.diagnostics()["computed_dc"], 11);
    assert_eq!(reply.diagnostics()["dc_strategy"], "entertainment_range");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].metadata["purpose"], "roll_dm_check");
    assert_eq!(requests[0].metadata["dice_expression"], "1d20");
    assert_eq!(requests[0].metadata["dice_minimum"], "1");
    assert_eq!(requests[0].metadata["dice_maximum"], "20");
    let user_context = &requests[0].messages.last().unwrap().content;
    assert!(user_context.contains("用户问题：晚上要不要出门"));
    assert!(user_context.contains("骰式：1d20"));
    assert!(user_context.contains("最小总值：1"));
    assert!(user_context.contains("最大总值：20"));
    let serialized = serde_json::to_string(&requests[0]).unwrap();
    assert!(!serialized.contains("\"roll\""));
    assert!(!serialized.contains("投掷：14"));
}

#[tokio::test]
async fn coc_entertainment_check_uses_d100_and_roll_under_diagnostics() {
    let provider = MockProvider::replying(fortune_json());
    let requests = provider.requests.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let reply = execute_roll_command_with_rule_system_and_roller(
        &provider,
        None,
        RollCommand::DmCheck {
            expression: None,
            query: "能否成功".to_owned(),
        },
        Duration::from_secs(1),
        RollRuleSystem::Coc,
        |sides| {
            assert_eq!(sides, 100);
            40
        },
    )
    .await;

    assert!(reply.reply.contains("目标值 50，需 ≤ 50"));
    assert!(reply.reply.contains("✅ 成功"));
    assert_eq!(reply.diagnostics()["dice_expression"], "1d100");
    assert_eq!(reply.diagnostics()["computed_dc"], 50);
    assert_eq!(reply.diagnostics()["rule_system"], "coc");
    assert_eq!(reply.diagnostics()["dc_comparison"], "less_or_equal");
    let requests = requests.lock().unwrap();
    assert_eq!(requests[0].metadata["rule_system"], "coc");
    assert_eq!(requests[0].metadata["dc_comparison"], "less_or_equal");
}

#[tokio::test]
async fn custom_2d20_plus_modifier_uses_core_entertainment_dc_before_roll() {
    let provider = MockProvider::replying(
        r#"{"type":"ability","check_name":"说服检定","difficulty":"easy","success_meaning":"守卫同意放行","failure_meaning":"守卫拒绝放行"}"#,
    );
    let requests = provider.requests.clone();
    let events = provider.events.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let roller_events = events.clone();
    let mut values = [14, 18].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        Some("mock:dm".to_owned()),
        RollCommand::DmCheck {
            expression: Some(dice_expression("2d20+4")),
            query: "我能否说服守卫".to_owned(),
        },
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 20);
            roller_events.lock().unwrap().push("rng");
            values.next().expect("2d20 should roll exactly twice")
        },
    )
    .await;

    assert_eq!(*events.lock().unwrap(), ["model", "rng", "rng"]);
    assert!(reply.reply.contains("难度：容易（DC 20）"));
    assert!(reply.reply.contains("投掷：2d20+4：14 + 18 + 4 = 36"));
    assert!(reply.reply.contains("✅ 成功"));
    assert_eq!(reply.diagnostics()["dice_expression"], "2d20+4");
    assert_eq!(reply.diagnostics()["dice_minimum"], 6);
    assert_eq!(reply.diagnostics()["dice_maximum"], 44);
    assert_eq!(reply.diagnostics()["difficulty"], "easy");
    assert_eq!(reply.diagnostics()["computed_dc"], 20);
    assert_eq!(reply.diagnostics()["dc_strategy"], "entertainment_range");
    assert!(values.next().is_none());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let serialized = serde_json::to_string(&requests[0]).unwrap();
    assert!(serialized.contains("我能否说服守卫"));
    assert!(serialized.contains("2d20+4"));
    assert!(serialized.contains("最小总值：6"));
    assert!(serialized.contains("最大总值：44"));
    for actual_roll_value in ["14", "18", "36"] {
        assert!(
            !serialized.contains(actual_roll_value),
            "request leaked actual roll value {actual_roll_value}: {serialized}"
        );
    }
}

#[tokio::test]
async fn dm_receives_canonical_expression_and_range_for_each_supported_shape() {
    for (input, canonical, minimum, maximum, dc) in [
        ("2d20", "2d20", 2, 40, 21),
        ("2d20+4", "2d20+4", 6, 44, 25),
        ("d100", "1d100", 1, 100, 51),
        ("1d20+3", "1d20+3", 4, 23, 14),
        ("1d8+1d6+4", "1d8+1d6+4", 6, 18, 12),
        ("d20优势", "2d20k1", 1, 20, 11),
        ("d20劣势", "2d20q1", 1, 20, 11),
    ] {
        let provider = MockProvider::replying(
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"medium","success_meaning":"成功","failure_meaning":"失败"}"#,
        );
        let requests = provider.requests.clone();
        let provider = Arc::new(provider) as DynLlmProvider;
        let result = execute_roll_command_with_roller(
            &provider,
            None,
            RollCommand::DmCheck {
                expression: Some(dice_expression(input)),
                query: "测试范围".to_owned(),
            },
            Duration::from_secs(1),
            |sides| sides,
        )
        .await;

        assert_eq!(result.diagnostics()["roll_execution_kind"], "ai_dm_success");
        assert!(result.reply.contains(&format!("DC {dc}")), "{input}");
        let requests = requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.metadata["dice_expression"], canonical, "{input}");
        assert_eq!(
            request.metadata["dice_minimum"],
            minimum.to_string(),
            "{input}"
        );
        assert_eq!(
            request.metadata["dice_maximum"],
            maximum.to_string(),
            "{input}"
        );
        let context = &request.messages.last().unwrap().content;
        assert!(context.contains(&format!("骰式：{canonical}")), "{input}");
        assert!(context.contains(&format!("最小总值：{minimum}")), "{input}");
        assert!(context.contains(&format!("最大总值：{maximum}")), "{input}");
    }
}

#[tokio::test]
async fn oversized_query_is_rejected_without_model_or_roll() {
    let provider = MockProvider::replying(fortune_json());
    let events = provider.events.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        RollCommand::DmCheck {
            expression: None,
            query: "问".repeat(DM_QUERY_MAX_CHARS + 1),
        },
        Duration::from_secs(1),
        |_| panic!("oversized query must not roll"),
    )
    .await;
    assert_eq!(
        reply.reply,
        format!("判定问题过长，请控制在 {DM_QUERY_MAX_CHARS} 个字符以内。")
    );
    assert!(events.lock().unwrap().is_empty());
}
