//! Roll 命令解析、SealDice 兼容语法和请求预算测试。

use super::*;

#[test]
fn parses_default_dm_supported_and_invalid_dice_expressions() {
    assert_eq!(parse_roll_command("/roll"), Some(RollCommand::Default));
    assert_eq!(parse_roll_command("/r"), Some(RollCommand::Default));
    assert_eq!(parse_roll_command("  /ROLL  "), Some(RollCommand::Default));
    assert_eq!(
        parse_roll_command(" /RoLl   晚上要不要出门  "),
        Some(RollCommand::DmCheck {
            expression: None,
            query: "晚上要不要出门".to_owned(),
        })
    );
    for query in ["2 cats", "20 minutes", "C# 值得学吗", "(今晚要不要出门)"] {
        assert_eq!(
            parse_roll_command(&format!("/roll {query}")),
            Some(RollCommand::DmCheck {
                expression: None,
                query: query.to_owned(),
            }),
            "自然语言问题必须进入 AI DM：{query}"
        );
    }
    assert!(matches!(
        parse_roll_command("/r 2d20 我能否说服守卫"),
        Some(RollCommand::DiceBatch {
            expression,
            repetitions: 1,
            reason: Some(reason),
        }) if expression == dice_expression("2d20") && reason == "我能否说服守卫"
    ));
    for (input, reason) in [
        ("/r 2 dogs", "2 dogs"),
        ("/r battle", "battle"),
        ("/r 看#电影", "看#电影"),
    ] {
        assert!(
            matches!(
                parse_roll_command(input),
                Some(RollCommand::DiceBatch {
                    expression,
                    repetitions: 1,
                    reason: Some(parsed_reason),
                }) if expression == dice_expression("d20") && parsed_reason == reason
            ),
            "自然语言原因必须保持完整：{input}"
        );
    }
    assert_eq!(
        parse_roll_command("/roll 1d20 + 3 能否通过"),
        Some(RollCommand::DmCheck {
            expression: Some(dice_expression("1d20+3")),
            query: "能否通过".to_owned(),
        })
    );
    for (input, expression, query) in [
        ("/roll d20优势 说服守卫", "2d20k1", "说服守卫"),
        ("/roll d20劣势 潜行通过", "2d20q1", "潜行通过"),
    ] {
        assert_eq!(
            parse_roll_command(input),
            Some(RollCommand::DmCheck {
                expression: Some(dice_expression(expression)),
                query: query.to_owned(),
            }),
            "{input}"
        );
    }
    for (input, expression) in [
        ("/roll d20", "d20"),
        ("/roll d100", "d100"),
        ("/roll 2d6", "2d6"),
        ("/roll 1D100", "1D100"),
        ("/roll 1d20+3", "1d20+3"),
        ("/roll 2d20+4", "2d20+4"),
        ("/roll 4d6k5", "4d6k5"),
        ("/roll 2d6 + 1", "2d6 + 1"),
        ("/roll 1d8+1d6+4", "1d8+1d6+4"),
        ("/roll 1d20-1d6", "1d20-1d6"),
    ] {
        let parsed = parse_roll_command(input);
        assert!(
            matches!(
                parsed,
                Some(RollCommand::DiceExpression { expression: ref parsed })
                    if *parsed == dice_expression(expression)
            ),
            "{input}: {parsed:?}"
        );
    }
    for input in [
        "/roll 101d6",
        "/roll d101",
        "/roll 0d6",
        "/roll d0",
        "/roll 1d20+1001",
        "/r 4d6k0",
        "/roll d20dh1",
    ] {
        assert_eq!(
            parse_roll_command(input),
            Some(RollCommand::InvalidDiceExpression),
            "{input}"
        );
    }
    for (input, query) in [
        ("/roll 我能不能通过 DC20 的门", "我能不能通过 DC20 的门"),
        ("/roll 我有 2d6 个苹果吗", "我有 2d6 个苹果吗"),
        ("/roll 20 days", "20 days"),
        ("/roll Please pass", "Please pass"),
    ] {
        assert_eq!(
            parse_roll_command(input),
            Some(RollCommand::DmCheck {
                expression: None,
                query: query.to_owned(),
            }),
            "{input}"
        );
    }
    assert_eq!(parse_roll_command("/roll    "), Some(RollCommand::Default));
    assert_eq!(parse_roll_command("/help"), None);
    assert_eq!(parse_roll_command("普通消息"), None);
}

#[test]
fn parses_sealdice_compact_commands_and_aliases() {
    for (input, expression) in [
        ("/r2d6", "2d6"),
        ("/rd20", "1d20"),
        ("/rd优势", "2d20k1"),
        ("/rd劣势", "2d20q1"),
        (".r2d6", "2d6"),
        (".rd优势", "2d20k1"),
        (".rd劣势", "2d20q1"),
        (".rd优势+6", "d20优势+6"),
        (".rd劣势+6", "d20劣势+6"),
        (".rd优势+6+1d4", "d20优势+6+1d4"),
    ] {
        let parsed = parse_roll_command(input);
        assert!(
            matches!(
                parsed,
                Some(RollCommand::DiceExpression { expression: ref parsed })
                    if *parsed == dice_expression(expression)
            ),
            "{input}: {parsed:?}"
        );
    }
    assert!(matches!(
        parse_roll_command("/r2d6xxx"),
        Some(RollCommand::DiceBatch {
            expression,
            repetitions: 1,
            reason: Some(reason),
        }) if expression == dice_expression("2d6") && reason == "xxx"
    ));
    for input in [
        "/r测试",
        "/rd测试",
        ".r测试",
        ".rd测试",
        "/r 测试",
        "/rd 测试",
        ".r 测试",
        ".rd 测试",
    ] {
        assert!(
            matches!(
                parse_roll_command(input),
                Some(RollCommand::DiceBatch {
                    expression,
                    repetitions: 1,
                    reason: Some(reason),
                }) if expression == dice_expression("d20") && reason == "测试"
            ),
            "{input}"
        );
    }
    assert!(matches!(
        parse_roll_command("/rap 是惩罚骰"),
        Some(RollCommand::DiceBatch {
            expression,
            repetitions: 1,
            reason: Some(reason),
        }) if expression == dice_expression("p") && reason == "是惩罚骰"
    ));
    assert!(matches!(
        parse_roll_command("/rab奖励骰"),
        Some(RollCommand::DiceBatch {
            expression,
            repetitions: 1,
            reason: Some(reason),
        }) if expression == dice_expression("b") && reason == "奖励骰"
    ));
    for input in ["/rapid", "/rabbit", ".rapid", ".rabbit"] {
        assert_eq!(parse_roll_command(input), None, "unknown command: {input}");
    }
    assert!(matches!(
        parse_roll_command("/rap 失败原因"),
        Some(RollCommand::DiceBatch {
            expression,
            repetitions: 1,
            reason: Some(reason),
        }) if expression == dice_expression("p") && reason == "失败原因"
    ));
    assert!(matches!(
        parse_roll_command("/r 2#d20"),
        Some(RollCommand::DiceBatch {
            expression,
            repetitions: 2,
            reason: None,
        }) if expression == dice_expression("d20")
    ));
    assert_eq!(
        parse_roll_command("/roll 2#d20 问题"),
        Some(RollCommand::RepeatedDmCheckUnsupported)
    );
}

#[test]
fn repeated_bare_d_expression_uses_the_configured_default_die_sides() {
    for (rule_system, expected) in [
        (RollRuleSystem::Dnd, "1d20+1"),
        (RollRuleSystem::Coc, "1d100+1"),
    ] {
        let parsed =
            parse_roll_command_with_default_die_sides(".r2#d+1", rule_system.default_die_sides());
        assert!(
            matches!(
                parsed,
                Some(RollCommand::DiceBatch {
                    ref expression,
                    repetitions: 2,
                    reason: None,
                }) if expression.to_string() == expected
            ),
            "{rule_system:?}: {parsed:?}"
        );
    }
}

#[test]
fn rejects_oversized_or_controlled_local_reasons_before_building_a_batch() {
    let oversized = "x".repeat(LOCAL_ROLL_REASON_MAX_CHARS + 1);
    for alias in ["r", "rd", "rap", "rab"] {
        assert_eq!(
            parse_roll_command(&format!("/{alias} {oversized}")),
            Some(RollCommand::InvalidLocalReason),
            "{alias} 必须拒绝超长原因"
        );
        assert_eq!(
            parse_roll_command(&format!("/{alias} 第一行\n第二行")),
            Some(RollCommand::InvalidLocalReason),
            "{alias} 必须拒绝换行原因"
        );
    }
}

#[test]
fn dm_timeout_is_clipped_to_the_request_budget_with_fallback_reserve() {
    assert_eq!(
        dm_timeout_for_request(Duration::from_secs(30)),
        DM_CHECK_TIMEOUT
    );
    assert_eq!(
        dm_timeout_for_request(Duration::from_secs(1)),
        Duration::from_millis(750)
    );
    assert_eq!(
        dm_timeout_for_request(Duration::from_millis(100)),
        Duration::ZERO
    );
}
