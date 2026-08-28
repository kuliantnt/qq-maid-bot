//! 不调用 AI DM 的本地骰点执行与回执测试。

use super::*;

#[tokio::test]
async fn default_roll_never_calls_provider() {
    let provider = Arc::new(MockProvider::failing(LlmError::provider(
        "must not call",
        "test",
    ))) as DynLlmProvider;
    let reply = execute_roll_command_with_roller(
        &provider,
        Some("unused".to_owned()),
        RollCommand::Default,
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 20);
            13
        },
    )
    .await;
    assert_eq!(reply.reply, "🎲 掷出了 13 / 20");
    assert_eq!(reply.metrics.provider, "rust");
    assert_eq!(reply.diagnostics()["roll_execution_kind"], "local");
}

#[tokio::test]
async fn simple_dice_expressions_roll_locally_without_provider() {
    let provider = MockProvider::replying(fortune_json());
    let requests = provider.requests.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let mut values = [2, 5].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        Some("unused".to_owned()),
        RollCommand::DiceExpression {
            expression: dice_expression("2d6"),
        },
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 6);
            values.next().expect("2d6 should roll exactly twice")
        },
    )
    .await;

    assert_eq!(reply.reply, "🎲 2d6：2 + 5 = 7");
    assert!(values.next().is_none());
    assert!(requests.lock().unwrap().is_empty());

    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        RollCommand::DiceExpression {
            expression: dice_expression("1d100"),
        },
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 100);
            73
        },
    )
    .await;
    assert_eq!(reply.reply, "🎲 掷出了 73 / 100");
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn compact_reason_and_repeated_rolls_stay_local_and_keep_dice_icon() {
    let provider = Arc::new(MockProvider::failing(LlmError::provider(
        "must not call",
        "test",
    ))) as DynLlmProvider;
    let mut values = [2, 5].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        RollCommand::DiceBatch {
            expression: dice_expression("d6"),
            repetitions: 2,
            reason: Some("测试".to_owned()),
        },
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 6);
            values.next().expect("two independent rounds")
        },
    )
    .await;
    assert!(reply.reply.starts_with("🎲"));
    assert!(reply.reply.contains("第1轮：掷出了 2 / 6"));
    assert!(reply.reply.contains("第2轮：掷出了 5 / 6"));
    assert!(reply.reply.contains("“测试”"));
}

#[tokio::test]
async fn oversized_keep_count_preserves_all_dice_for_compact_sealdice_command() {
    let provider = MockProvider::failing(LlmError::provider("must not call", "test"));
    let events = provider.events.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let command = parse_roll_command(".r4d6k5原因").expect("compact keep command should parse");
    assert!(matches!(
        &command,
        RollCommand::DiceBatch {
            expression,
            repetitions: 1,
            reason: Some(reason),
        } if *expression == dice_expression("4d6k5") && reason == "原因"
    ));

    let mut values = [6, 6, 5, 2].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        command,
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 6);
            values.next().expect("4d6 should roll four dice")
        },
    )
    .await;

    assert_eq!(reply.reply, "🎲 “原因” 4d6k5：{6 | 6 | 5 | 2} = 19");
    assert!(values.next().is_none());
    assert!(events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn compact_penalty_alias_uses_valid_percentile_roll_template() {
    let provider = Arc::new(MockProvider::failing(LlmError::provider(
        "must not call",
        "test",
    ))) as DynLlmProvider;
    let command = parse_roll_command("。rap 测试").expect("compact penalty alias should parse");
    let mut values = [8, 10, 6].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        command,
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 10);
            values
                .next()
                .expect("penalty expression should roll three d10s")
        },
    )
    .await;

    assert_eq!(reply.reply, "🎲 “测试” p：D100=68（惩罚 6） = 68");
    assert!(reply.diagnostics()["roll_execution_kind"] == "local");
}

#[tokio::test]
async fn sealdice_default_advantage_expression_uses_roll_template_without_provider() {
    let provider = MockProvider::failing(LlmError::provider(
        "must not call for compact advantage",
        "test",
    ));
    let events = provider.events.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let command = parse_roll_command(".rd优势+6+1d4").expect("compact advantage should parse");
    let mut values = [20, 17, 4].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        command,
        Duration::from_secs(1),
        |sides| {
            assert!(matches!(sides, 20 | 4));
            values
                .next()
                .expect("advantage expression should roll three dice")
        },
    )
    .await;

    assert!(reply.reply.starts_with("🎲 2d20k1+6+1d4："));
    assert!(reply.reply.ends_with("= 30"));
    assert_eq!(reply.diagnostics()["roll_execution_kind"], "local");
    assert!(events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn slash_r_reason_is_local_but_roll_reason_keeps_explicit_ai_dm_path() {
    let provider = MockProvider::failing(LlmError::provider("must not call for /r reason", "test"));
    let events = provider.events.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let command = parse_roll_command("/r d50 开锁").expect("/r reason should parse locally");
    assert!(matches!(command, RollCommand::DiceBatch { .. }));
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        command,
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 50);
            17
        },
    )
    .await;
    assert!(reply.reply.starts_with("🎲"));
    assert!(reply.reply.contains("“开锁”"));
    assert_eq!(reply.diagnostics()["roll_execution_kind"], "local");
    assert!(events.lock().unwrap().is_empty());

    let command = parse_roll_command("/r 测试").expect("/r reason should use default d20");
    assert!(matches!(command, RollCommand::DiceBatch { .. }));
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        command,
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 20);
            12
        },
    )
    .await;
    assert!(reply.reply.contains("“测试”"));
    assert_eq!(reply.diagnostics()["roll_execution_kind"], "local");
    assert!(events.lock().unwrap().is_empty());

    assert_eq!(
        parse_roll_command("/roll d50 开锁"),
        Some(RollCommand::DmCheck {
            expression: Some(dice_expression("d50")),
            query: "开锁".to_owned(),
        })
    );
}

#[tokio::test]
async fn named_roll_reply_uses_manual_display_name_argument() {
    let provider = Arc::new(MockProvider::failing(LlmError::provider(
        "must not call",
        "test",
    ))) as DynLlmProvider;
    let reply = execute_roll_command_with_display_name(
        &provider,
        None,
        RollCommand::DiceBatch {
            expression: dice_expression("p"),
            repetitions: 1,
            reason: Some("是惩罚骰".to_owned()),
        },
        Duration::from_secs(1),
        Some("人妻".to_owned()),
    )
    .await;
    assert!(reply.reply.starts_with("🎲 <人妻> 的“是惩罚骰”"));
}

#[tokio::test]
async fn extended_dice_expression_rolls_locally_with_modifier_and_multiple_terms() {
    let provider = MockProvider::replying(fortune_json());
    let requests = provider.requests.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let mut values = [3, 5].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        RollCommand::DiceExpression {
            expression: dice_expression("1d8+1d6+4"),
        },
        Duration::from_secs(1),
        |sides| {
            assert!(matches!(sides, 8 | 6));
            values.next().expect("expression should roll two dice")
        },
    )
    .await;

    assert_eq!(reply.reply, "🎲 1d8+1d6+4：3 + 5 + 4 = 12");
    assert!(values.next().is_none());
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_dice_expression_never_calls_provider_or_roller() {
    let provider = MockProvider::replying(fortune_json());
    let requests = provider.requests.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let reply = execute_roll_command_with_roller(
        &provider,
        Some("unused".to_owned()),
        RollCommand::InvalidDiceExpression,
        Duration::from_secs(1),
        |_| panic!("unsupported dice expression must not roll"),
    )
    .await;

    assert_eq!(reply.reply, INVALID_DICE_EXPRESSION_REPLY);
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_local_reason_returns_bounded_reply_without_provider_or_roller() {
    let provider = MockProvider::replying(fortune_json());
    let requests = provider.requests.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let reply = execute_roll_command_with_roller(
        &provider,
        Some("unused".to_owned()),
        RollCommand::InvalidLocalReason,
        Duration::from_secs(1),
        |_| panic!("非法本地原因不能触发投骰"),
    )
    .await;

    assert_eq!(reply.reply, INVALID_LOCAL_ROLL_REASON_REPLY);
    assert!(reply.reply.chars().count() < 100);
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn repeated_dm_check_is_rejected_without_provider_or_roller() {
    let provider = MockProvider::replying(fortune_json());
    let requests = provider.requests.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let command = parse_roll_command("/roll 2#d20 问题")
        .expect("repeated DM check should be handled by roll domain");
    let reply = execute_roll_command_with_roller(
        &provider,
        Some("unused".to_owned()),
        command,
        Duration::from_secs(1),
        |_| panic!("rejected repeated DM check must not roll"),
    )
    .await;

    assert_eq!(reply.reply, REPEATED_DM_CHECK_REPLY);
    assert_eq!(reply.diagnostics()["roll_execution_kind"], "local");
    assert!(requests.lock().unwrap().is_empty());
}
