//! AI DM 失败后的本地骰点降级回归测试。

use super::*;

#[tokio::test]
async fn invalid_or_result_bearing_ai_outputs_use_explicit_fallback() {
    for invalid_reply in [
        r#"{"type":"fortune","check_name":"命运检定","success_meaning":"出门","failure_meaning":"宅家"}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":"10","success_meaning":"出门","failure_meaning":"宅家"}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":2147483648,"success_meaning":"出门","failure_meaning":"宅家"}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":1,"success_meaning":"出门","failure_meaning":"宅家"}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"出门","failure_meaning":"宅家","roll":20}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"出门","failure_meaning":"宅家","total":20}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"出门","failure_meaning":"宅家","success":true}"#,
    ] {
        let provider = Arc::new(MockProvider::replying(invalid_reply)) as DynLlmProvider;
        let reply = execute_roll_command_with_roller(
            &provider,
            None,
            RollCommand::DmCheck {
                expression: None,
                query: "出门吗".to_owned(),
            },
            Duration::from_secs(1),
            |_| 7,
        )
        .await;
        assert_eq!(
            reply.reply,
            "AI DM 暂时无法判断本次检定难度，本次仅进行普通 D20 投掷。\n\n🎲 掷出了 7 / 20"
        );
        assert_eq!(
            reply.diagnostics()["roll_fallback_reason"],
            "roll_dm_invalid_output"
        );
    }
}

#[tokio::test]
async fn provider_error_and_timeout_fall_back_to_local_d20() {
    let failed =
        Arc::new(MockProvider::failing(LlmError::provider("boom", "test"))) as DynLlmProvider;
    let failed_reply = execute_roll_command_with_roller(
        &failed,
        None,
        RollCommand::DmCheck {
            expression: None,
            query: "出门吗".to_owned(),
        },
        Duration::from_secs(1),
        |_| 9,
    )
    .await;
    assert!(failed_reply.reply.ends_with("🎲 掷出了 9 / 20"));
    assert_eq!(
        failed_reply.diagnostics()["roll_execution_kind"],
        "ai_dm_fallback"
    );
    assert_eq!(
        failed_reply.diagnostics()["roll_fallback_reason"],
        "provider_error"
    );

    let delayed =
        Arc::new(MockProvider::replying(fortune_json()).delayed(Duration::from_millis(50)))
            as DynLlmProvider;
    let timeout_reply = execute_roll_command_with_roller(
        &delayed,
        None,
        RollCommand::DmCheck {
            expression: None,
            query: "出门吗".to_owned(),
        },
        Duration::from_millis(1),
        |_| 11,
    )
    .await;
    assert!(timeout_reply.reply.ends_with("🎲 掷出了 11 / 20"));
    assert_eq!(
        timeout_reply.diagnostics()["roll_fallback_reason"],
        "timeout"
    );
}

#[tokio::test]
async fn coc_provider_error_falls_back_to_local_d100_with_matching_copy() {
    let provider =
        Arc::new(MockProvider::failing(LlmError::provider("boom", "test"))) as DynLlmProvider;
    let reply = execute_roll_command_with_rule_system_and_roller(
        &provider,
        None,
        RollCommand::DmCheck {
            expression: None,
            query: "出门吗".to_owned(),
        },
        Duration::from_secs(1),
        RollRuleSystem::Coc,
        |sides| {
            assert_eq!(sides, 100);
            73
        },
    )
    .await;

    assert_eq!(
        reply.reply,
        "AI DM 暂时无法判断本次检定难度，本次仅进行普通 D100 投掷。\n\n🎲 掷出了 73 / 100"
    );
    assert!(!reply.reply.contains("D20"));
    assert_eq!(
        reply.diagnostics()["roll_fallback_reason"],
        "provider_error"
    );
}

#[tokio::test]
async fn coc_invalid_structured_output_falls_back_to_local_d100_with_matching_copy() {
    let provider = Arc::new(MockProvider::replying(
        r#"{"type":"fortune","check_name":"命运检定","success_meaning":"出门","failure_meaning":"宅家"}"#,
    )) as DynLlmProvider;
    let reply = execute_roll_command_with_rule_system_and_roller(
        &provider,
        None,
        RollCommand::DmCheck {
            expression: None,
            query: "出门吗".to_owned(),
        },
        Duration::from_secs(1),
        RollRuleSystem::Coc,
        |sides| {
            assert_eq!(sides, 100);
            41
        },
    )
    .await;

    assert_eq!(
        reply.reply,
        "AI DM 暂时无法判断本次检定难度，本次仅进行普通 D100 投掷。\n\n🎲 掷出了 41 / 100"
    );
    assert!(!reply.reply.contains("D20"));
    assert_eq!(
        reply.diagnostics()["roll_fallback_reason"],
        "roll_dm_invalid_output"
    );
}

#[tokio::test]
async fn explicit_expression_fallback_keeps_existing_copy_and_expression() {
    let provider =
        Arc::new(MockProvider::failing(LlmError::provider("boom", "test"))) as DynLlmProvider;
    let mut values = [4, 5].into_iter();
    let reply = execute_roll_command_with_rule_system_and_roller(
        &provider,
        None,
        RollCommand::DmCheck {
            expression: Some(dice_expression("2d6+1")),
            query: "能否成功".to_owned(),
        },
        Duration::from_secs(1),
        RollRuleSystem::Coc,
        |sides| {
            assert_eq!(sides, 6);
            values.next().expect("2d6 should roll exactly twice")
        },
    )
    .await;

    assert_eq!(
        reply.reply,
        "AI DM 暂时无法判断本次检定难度，本次仅进行指定骰子表达式投掷。\n\n🎲 2d6+1：4 + 5 + 1 = 10"
    );
    assert!(values.next().is_none());
}
