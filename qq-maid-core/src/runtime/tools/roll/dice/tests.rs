use super::*;

fn expression(input: &str) -> DiceExpression {
    match parse_expression(input) {
        DiceExpressionParse::Parsed(expression) => expression,
        other => panic!("expected expression {input}, got {other:?}"),
    }
}

fn spec(input: &str) -> DiceRollSpec {
    match parse_roll_spec(input) {
        DiceRollSpecParse::Parsed(spec) => spec,
        other => panic!("expected spec {input}, got {other:?}"),
    }
}

#[test]
fn parses_common_sealdice_expression_shapes() {
    for input in [
        "d20",
        "2d6",
        "1d20+3",
        "2d6 + 1",
        "1d8+1d6+4",
        "1d20-2",
        "1d20-1d6",
        "1d20+1+2",
        "100 + 3 * 2",
        "30 + (-1d20) + 49",
        "d50 * 3 + (2 - p2)",
        "4d6k3",
        "2d20q1",
        "d20kh",
        "d20优势",
        "d20劣势",
        "b",
        "b3",
        "p4",
    ] {
        assert!(
            matches!(parse_expression(input), DiceExpressionParse::Parsed(_)),
            "{input}"
        );
    }
}

#[test]
fn leaves_natural_language_for_the_dm_path() {
    for input in [
        "我有 2d6 个苹果吗",
        "我能不能通过 DC20 的门",
        "DC20",
        "20 days",
        "2 dogs",
        "battle",
        "Please pass",
    ] {
        assert_eq!(
            parse_expression(input),
            DiceExpressionParse::NotDiceExpression,
            "{input}"
        );
    }
    assert!(matches!(
        parse_expression("2 d 6"),
        DiceExpressionParse::Parsed(_)
    ));
}

#[test]
fn rejects_invalid_expression_and_complexity_limits() {
    for (input, expected) in [
        ("0d6", DiceExpressionError::CountOutOfRange),
        ("d0", DiceExpressionError::SidesOutOfRange),
        ("101d6", DiceExpressionError::CountOutOfRange),
        ("d101", DiceExpressionError::SidesOutOfRange),
        ("1d20+1001", DiceExpressionError::ModifierOutOfRange),
        ("1/0", DiceExpressionError::DivisionByZero),
        ("2**13", DiceExpressionError::ExponentOutOfRange),
        ("5a6", DiceExpressionError::InvalidSyntax),
        ("4d6k5", DiceExpressionError::KeepCountOutOfRange),
    ] {
        assert_eq!(
            parse_expression(input),
            DiceExpressionParse::Invalid(expected),
            "{input}"
        );
    }
    assert_eq!(
        parse_expression("1d20+1d20+1d20+1d20+1d20+1d20+1d20+1d20+1d20"),
        DiceExpressionParse::Invalid(DiceExpressionError::TooManyTerms)
    );
    assert!(matches!(
        parse_expression("b+b+b+b+p+p+p+p"),
        DiceExpressionParse::Parsed(_)
    ));
    assert_eq!(
        parse_expression("b+b+b+b+b+b+b+b+b"),
        DiceExpressionParse::Invalid(DiceExpressionError::TooManyTerms)
    );
    assert_eq!(
        parse_expression("100d1+1d1"),
        DiceExpressionParse::Invalid(DiceExpressionError::TooManyDice)
    );
    assert_eq!(
        parse_expression(&format!("1d20+{}", "1".repeat(MAX_EXPRESSION_CHARS))),
        DiceExpressionParse::Invalid(DiceExpressionError::TooLong)
    );
}

#[test]
fn bounds_prefix_scans_before_trying_expression_boundaries() {
    let oversized = format!("d20 {}", "原因".repeat(MAX_PREFIX_INPUT_CHARS));
    assert!(parse_roll_spec_prefix(&oversized).is_none());
    assert!(parse_roll_spec_compact_prefix(&oversized).is_none());
}

#[test]
fn parses_repetitions_and_enforces_limits() {
    let parsed = spec("2#d20");
    assert_eq!(parsed.repetitions, 2);
    assert_eq!(parsed.expression.to_string(), "1d20");
    assert_eq!(
        parse_roll_spec("21#d20"),
        DiceRollSpecParse::Invalid(DiceExpressionError::RepeatCountOutOfRange)
    );
    assert_eq!(
        parse_roll_spec("3#100d20"),
        DiceRollSpecParse::Invalid(DiceExpressionError::TooManyRolls)
    );
}

#[test]
fn parses_expression_prefix_without_consuming_reason() {
    let (expression, reason) = parse_expression_prefix("1d20 + 3 能否通过").expect("prefix");
    assert_eq!(expression.to_string(), "1d20+3");
    assert_eq!(reason, "能否通过");
    let (spec, reason) = parse_roll_spec_prefix("2#d10 原因").expect("repeated prefix");
    assert_eq!(spec.repetitions, 2);
    assert_eq!(reason, "原因");

    let (spec, reason) = parse_roll_spec_compact_prefix("2d6原因").expect("compact prefix");
    assert_eq!(spec.expression.to_string(), "2d6");
    assert_eq!(reason, "原因");

    for input in ["4d6k5", "d20dh1", "4d6kh0", "4d6k5 原因"] {
        assert!(
            parse_roll_spec_compact_prefix(input).is_none(),
            "非法取骰后缀不能降级为原因：{input}"
        );
    }

    for input in ["20 days", "2 dogs"] {
        assert!(
            parse_roll_spec_prefix(input).is_none(),
            "纯数字不能作为骰式前缀：{input}"
        );
    }
    for input in ["battle", "Please pass", "difficult"] {
        assert!(
            parse_roll_spec_compact_prefix(input).is_none(),
            "ASCII 单词不能从单字母骰式处拆开：{input}"
        );
    }
}

#[test]
fn rolls_simple_and_arithmetic_expressions() {
    let result = expression("1d8+1d6+4")
        .roll(&mut |sides| match sides {
            8 => 3,
            6 => 5,
            _ => panic!("unexpected sides {sides}"),
        })
        .unwrap();
    assert_eq!(result.rolls.len(), 2);
    assert_eq!(result.rolls[0].value, 3);
    assert!(result.rolls[0].kept);
    assert_eq!(result.modifier, 4);
    assert_eq!(result.total, 12);
    assert_eq!(result.calculation(), "3 + 5 + 4");

    let result = expression("100+3*2").roll(&mut |_| 1).unwrap();
    assert_eq!(result.total, 106);
    assert_eq!(result.calculation(), "100 + 3 * 2");
}

#[test]
fn keeps_highest_and_lowest_without_aggregating_dropped_dice() {
    let mut values = [1, 6, 3, 4].into_iter();
    let high = expression("4d6k3")
        .roll(&mut |_| values.next().unwrap())
        .unwrap();
    assert_eq!(high.total, 13);
    assert_eq!(high.rolls.iter().filter(|roll| roll.kept).count(), 3);

    let mut values = [19, 7].into_iter();
    let low = expression("2d20q1")
        .roll(&mut |_| values.next().unwrap())
        .unwrap();
    assert_eq!(low.total, 7);
    assert_eq!(low.rolls.iter().filter(|roll| roll.kept).count(), 1);
}

#[test]
fn bonus_and_penalty_dice_choose_the_expected_percentile_result() {
    let mut values = [6, 9, 4, 3].into_iter();
    let bonus = expression("b2")
        .roll(&mut |_| values.next().unwrap())
        .unwrap();
    assert_eq!(bonus.total, 36);

    let mut values = [7, 2, 8, 5].into_iter();
    let penalty = expression("p2")
        .roll(&mut |_| values.next().unwrap())
        .unwrap();
    assert_eq!(penalty.total, 87);
}

#[test]
fn percentile_special_dice_maps_ten_faces_to_zero() {
    let mut values = [8, 10, 6].into_iter();
    let penalty = expression("p")
        .roll(&mut |_| {
            values
                .next()
                .expect("percentile dice should roll three d10s")
        })
        .unwrap();

    assert_eq!(penalty.total, 68);
    assert_eq!(penalty.calculation(), "D100=68（惩罚 6）");
}

#[test]
fn rejects_out_of_range_injected_rolls() {
    let error = expression("d20").roll(&mut |_| 21).unwrap_err();
    assert_eq!(
        error,
        DiceRollError::OutOfRange {
            sides: 20,
            value: 21
        }
    );
}

#[test]
fn canonical_expression_contains_operators_and_suffixes() {
    assert_eq!(expression("1d8 + 1d6 - 4").to_string(), "1d8+1d6-4");
    assert_eq!(expression("4d6k3").to_string(), "4d6k3");
    assert_eq!(expression("d20优势").to_string(), "2d20k1");
    assert_eq!(expression("100 + 3 * 2").to_string(), "100+3*2");
}

#[test]
fn canonical_power_formatting_preserves_parse_semantics() {
    let left_nested = expression("(d6**2)**3");
    let left_canonical = left_nested.to_string();
    assert_eq!(left_canonical, "(1d6**2)**3");
    assert_eq!(expression(&left_canonical), left_nested);

    let right_nested = expression("d2**(2**3)");
    assert_eq!(expression(&right_nested.to_string()), right_nested);
}

#[test]
fn calculates_total_ranges_without_rolling() {
    for (input, expected) in [
        ("2d20", (2, 40)),
        ("1d20+3", (4, 23)),
        ("1d6", (1, 6)),
        ("d100", (1, 100)),
        ("1d8+1d6+4", (6, 18)),
        ("2d6-4", (-2, 8)),
        ("4d6k3", (3, 18)),
        ("100+3*2", (106, 106)),
    ] {
        assert_eq!(expression(input).total_range(), expected, "{input}");
    }
}

#[test]
fn power_ranges_include_interior_zero_and_block_zero_denominators() {
    assert_eq!(expression("(d3-2)**2").total_range(), (0, 1));
    assert_eq!(
        parse_expression("1/((d3-2)**2)"),
        DiceExpressionParse::Invalid(DiceExpressionError::DivisionByZero)
    );
}
