//! Dice AST 的静态范围分析与本地随机求值。

use super::ast::{BinaryOperator, SpecialDice, UnaryOperator};
use super::{
    DiceExpression, DiceExpressionError, DiceKeep, DiceNode, DiceRollError, DiceTerm, DieRoll,
    MAX_EVALUATED_ABS, MAX_POWER_EXPONENT, Roller,
};

pub(super) struct EvaluatedValue {
    pub(super) value: i64,
    pub(super) display: String,
    pub(super) precedence: u8,
    pub(super) rolls: Vec<DieRoll>,
}

pub(super) fn validate_expression(expression: &DiceExpression) -> Result<(), DiceExpressionError> {
    let (minimum, maximum) = node_range(&expression.root)?;
    if minimum.abs() > MAX_EVALUATED_ABS || maximum.abs() > MAX_EVALUATED_ABS {
        return Err(DiceExpressionError::ValueOutOfRange);
    }
    Ok(())
}

pub(super) fn node_range(node: &DiceNode) -> Result<(i64, i64), DiceExpressionError> {
    match node {
        DiceNode::Number(value) => Ok((*value, *value)),
        DiceNode::Dice(term) => {
            let count = i64::from(term.selected_count());
            Ok((count, count * i64::from(term.sides)))
        }
        DiceNode::Special(_) => Ok((1, 100)),
        DiceNode::Unary { operator, operand } => {
            let (minimum, maximum) = node_range(operand)?;
            match operator {
                UnaryOperator::Positive => Ok((minimum, maximum)),
                UnaryOperator::Negative => Ok((-maximum, -minimum)),
            }
        }
        DiceNode::Binary {
            operator,
            left,
            right,
        } => {
            let (left_minimum, left_maximum) = node_range(left)?;
            let (right_minimum, right_maximum) = node_range(right)?;
            match operator {
                BinaryOperator::Add => checked_range(
                    left_minimum.checked_add(right_minimum),
                    left_maximum.checked_add(right_maximum),
                ),
                BinaryOperator::Subtract => checked_range(
                    left_minimum.checked_sub(right_maximum),
                    left_maximum.checked_sub(right_minimum),
                ),
                BinaryOperator::Multiply => {
                    let values = [
                        left_minimum.checked_mul(right_minimum),
                        left_minimum.checked_mul(right_maximum),
                        left_maximum.checked_mul(right_minimum),
                        left_maximum.checked_mul(right_maximum),
                    ];
                    checked_range(
                        values.into_iter().flatten().min(),
                        values.into_iter().flatten().max(),
                    )
                }
                BinaryOperator::Divide => {
                    if right_minimum <= 0 && right_maximum >= 0 {
                        return Err(DiceExpressionError::DivisionByZero);
                    }
                    let values = [
                        left_minimum.checked_div(right_minimum),
                        left_minimum.checked_div(right_maximum),
                        left_maximum.checked_div(right_minimum),
                        left_maximum.checked_div(right_maximum),
                    ];
                    checked_range(
                        values.into_iter().flatten().min(),
                        values.into_iter().flatten().max(),
                    )
                }
                BinaryOperator::Power => {
                    if right_minimum < 0 || right_maximum > MAX_POWER_EXPONENT {
                        return Err(DiceExpressionError::ExponentOutOfRange);
                    }
                    let mut values = Vec::new();
                    // 幂函数的区间极值不一定在两个端点：底数跨过 0 时，偶数指数的
                    // 最小值可能来自内部的 0；负底数还要保留 -1/1 以覆盖奇偶指数
                    // 切换。候选集合足以覆盖整数区间上的极值，同时不需要枚举大区间。
                    for base in [left_minimum, left_maximum, -1, 0, 1]
                        .into_iter()
                        .filter(|base| (left_minimum..=left_maximum).contains(base))
                    {
                        for exponent in right_minimum..=right_maximum {
                            let exponent = u32::try_from(exponent)
                                .map_err(|_| DiceExpressionError::ExponentOutOfRange)?;
                            values.push(
                                base.checked_pow(exponent)
                                    .ok_or(DiceExpressionError::ValueOutOfRange)?,
                            );
                        }
                    }
                    checked_range(values.iter().copied().min(), values.iter().copied().max())
                }
            }
        }
    }
}

fn checked_range(
    minimum: Option<i64>,
    maximum: Option<i64>,
) -> Result<(i64, i64), DiceExpressionError> {
    let minimum = minimum.ok_or(DiceExpressionError::ValueOutOfRange)?;
    let maximum = maximum.ok_or(DiceExpressionError::ValueOutOfRange)?;
    if minimum.abs() > MAX_EVALUATED_ABS || maximum.abs() > MAX_EVALUATED_ABS {
        return Err(DiceExpressionError::ValueOutOfRange);
    }
    Ok((minimum, maximum))
}

pub(super) fn node_roll_count(node: &DiceNode) -> u32 {
    match node {
        DiceNode::Number(_) => 0,
        DiceNode::Dice(term) => u32::from(term.count),
        DiceNode::Special(SpecialDice::Bonus(count) | SpecialDice::Penalty(count)) => {
            2 + u32::from(*count)
        }
        DiceNode::Unary { operand, .. } => node_roll_count(operand),
        DiceNode::Binary { left, right, .. } => node_roll_count(left) + node_roll_count(right),
    }
}

pub(super) fn evaluate_node<R: Roller>(
    node: &DiceNode,
    roller: &mut R,
) -> Result<EvaluatedValue, DiceRollError> {
    match node {
        DiceNode::Number(value) => Ok(EvaluatedValue {
            value: *value,
            display: value.to_string(),
            precedence: 5,
            rolls: Vec::new(),
        }),
        DiceNode::Dice(term) => evaluate_dice_term(term, roller),
        DiceNode::Special(special) => evaluate_special_dice(*special, roller),
        DiceNode::Unary { operator, operand } => {
            let value = evaluate_node(operand, roller)?;
            let result = match operator {
                UnaryOperator::Positive => value.value,
                UnaryOperator::Negative => {
                    value.value.checked_neg().ok_or(DiceRollError::Overflow)?
                }
            };
            let display = match operator {
                UnaryOperator::Positive => format_child(&value, 4, false),
                UnaryOperator::Negative => format!("-{}", format_child(&value, 4, true)),
            };
            Ok(EvaluatedValue {
                value: result,
                display,
                precedence: 4,
                rolls: value.rolls,
            })
        }
        DiceNode::Binary {
            operator,
            left,
            right,
        } => {
            let left = evaluate_node(left, roller)?;
            let right = evaluate_node(right, roller)?;
            let value = match operator {
                BinaryOperator::Add => left.value.checked_add(right.value),
                BinaryOperator::Subtract => left.value.checked_sub(right.value),
                BinaryOperator::Multiply => left.value.checked_mul(right.value),
                BinaryOperator::Divide => {
                    if right.value == 0 {
                        return Err(DiceRollError::DivisionByZero);
                    }
                    left.value.checked_div(right.value)
                }
                BinaryOperator::Power => {
                    let exponent =
                        u32::try_from(right.value).map_err(|_| DiceRollError::Overflow)?;
                    left.value.checked_pow(exponent)
                }
            }
            .ok_or(DiceRollError::Overflow)?;
            if value.abs() > MAX_EVALUATED_ABS {
                return Err(DiceRollError::Overflow);
            }
            let precedence = binary_precedence(*operator);
            let display = format!(
                "{} {} {}",
                format_child(&left, precedence, matches!(operator, BinaryOperator::Power),),
                binary_symbol(*operator),
                format_child(
                    &right,
                    precedence,
                    // 整数乘除不可结合；乘法右侧的同优先级子树即使以乘法为根，
                    // 也可能包含先执行的除法，因此统一保留原有分组。
                    matches!(
                        operator,
                        BinaryOperator::Subtract
                            | BinaryOperator::Multiply
                            | BinaryOperator::Divide
                            | BinaryOperator::Power
                    ),
                )
            );
            let mut rolls = left.rolls;
            rolls.extend(right.rolls);
            Ok(EvaluatedValue {
                value,
                display,
                precedence,
                rolls,
            })
        }
    }
}

fn evaluate_dice_term<R: Roller>(
    term: &DiceTerm,
    roller: &mut R,
) -> Result<EvaluatedValue, DiceRollError> {
    let mut values = Vec::with_capacity(usize::from(term.count));
    for _ in 0..term.count {
        let value = roller.roll(term.sides);
        if !(1..=term.sides).contains(&value) {
            return Err(DiceRollError::OutOfRange {
                sides: term.sides,
                value,
            });
        }
        values.push(value);
    }

    let mut kept = vec![true; values.len()];
    match term.keep {
        DiceKeep::All => {}
        DiceKeep::Highest(count) => {
            mark_only_selected(&values, &mut kept, usize::from(count), true);
        }
        DiceKeep::Lowest(count) => {
            mark_only_selected(&values, &mut kept, usize::from(count), false);
        }
        DiceKeep::DropHighest(count) => {
            mark_dropped(&values, &mut kept, usize::from(count), true);
        }
        DiceKeep::DropLowest(count) => {
            mark_dropped(&values, &mut kept, usize::from(count), false);
        }
    }
    let total = values
        .iter()
        .zip(&kept)
        .filter_map(|(value, kept)| kept.then_some(i64::from(*value)))
        .sum::<i64>();
    let display_values = values
        .iter()
        .zip(&kept)
        .map(|(value, kept)| {
            if *kept {
                value.to_string()
            } else {
                format!("{value}×")
            }
        })
        .collect::<Vec<_>>();
    let display = if term.keep.is_all() {
        display_values.join(" + ")
    } else {
        format!("{{{}}}", display_values.join(" | "))
    };
    // 未取舍的多骰会展开为 `1 + 2`，其展示语义是加法而非原子值；参与乘除、幂或
    // 一元运算时必须由 format_child 补括号，避免回执展示成另一条算式。
    let precedence = if term.keep.is_all() && term.count > 1 {
        binary_precedence(BinaryOperator::Add)
    } else {
        5
    };
    let rolls = values
        .into_iter()
        .zip(kept)
        .map(|(value, kept)| DieRoll {
            sides: term.sides,
            value,
            kept,
        })
        .collect();
    Ok(EvaluatedValue {
        value: total,
        display,
        precedence,
        rolls,
    })
}

fn mark_only_selected(values: &[u8], kept: &mut [bool], count: usize, highest: bool) {
    kept.fill(false);
    let mut indices = (0..values.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| values[*index]);
    if highest {
        indices.reverse();
    }
    for index in indices.into_iter().take(count) {
        kept[index] = true;
    }
}

fn mark_dropped(values: &[u8], kept: &mut [bool], count: usize, highest: bool) {
    let mut indices = (0..values.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| values[*index]);
    if highest {
        indices.reverse();
    }
    for index in indices.into_iter().take(count) {
        kept[index] = false;
    }
}

fn evaluate_special_dice<R: Roller>(
    special: SpecialDice,
    roller: &mut R,
) -> Result<EvaluatedValue, DiceRollError> {
    let count = match special {
        SpecialDice::Bonus(count) | SpecialDice::Penalty(count) => count,
    };
    let unit = valid_roll(roller, 10)?;
    let mut tens = Vec::with_capacity(usize::from(count) + 1);
    for _ in 0..=count {
        tens.push(valid_roll(roller, 10)?);
    }
    let candidates = tens
        .iter()
        .map(|value| percentile_value(*value, unit))
        .collect::<Vec<_>>();
    let selected = match special {
        SpecialDice::Bonus(_) => candidates
            .iter()
            .enumerate()
            .min_by_key(|(_, value)| *value)
            .map(|(index, _)| index)
            .expect("bonus dice always has a base roll"),
        SpecialDice::Penalty(_) => candidates
            .iter()
            .enumerate()
            .max_by_key(|(_, value)| *value)
            .map(|(index, _)| index)
            .expect("penalty dice always has a base roll"),
    };
    let selected_value = candidates[selected];
    let marker = match special {
        SpecialDice::Bonus(_) => "奖励",
        SpecialDice::Penalty(_) => "惩罚",
    };
    let extras = tens
        .iter()
        .skip(1)
        .map(|value| percentile_digit(*value).to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let display = format!("D100={selected_value}（{marker} {extras}）");
    let mut rolls = Vec::with_capacity(tens.len() + 1);
    rolls.push(DieRoll {
        sides: 10,
        value: unit,
        kept: true,
    });
    for (index, value) in tens.into_iter().enumerate() {
        rolls.push(DieRoll {
            sides: 10,
            value,
            kept: index == selected,
        });
    }
    Ok(EvaluatedValue {
        value: selected_value,
        display,
        precedence: 5,
        rolls,
    })
}

/// 百分骰的十面骰中，`10` 表示数字 `0`；两个 `0` 组合时按惯例显示为 `100`。
/// 直接把十面骰原始值当作十位会产生 `D100=108` 之类的越界结果。
fn percentile_value(tens: u8, unit: u8) -> i64 {
    let value = i64::from(percentile_digit(tens)) * 10 + i64::from(percentile_digit(unit));
    if value == 0 { 100 } else { value }
}

fn percentile_digit(value: u8) -> u8 {
    value % 10
}

fn valid_roll<R: Roller>(roller: &mut R, sides: u8) -> Result<u8, DiceRollError> {
    let value = roller.roll(sides);
    if !(1..=sides).contains(&value) {
        return Err(DiceRollError::OutOfRange { sides, value });
    }
    Ok(value)
}

fn binary_precedence(operator: BinaryOperator) -> u8 {
    match operator {
        BinaryOperator::Add | BinaryOperator::Subtract => 1,
        BinaryOperator::Multiply | BinaryOperator::Divide => 2,
        BinaryOperator::Power => 3,
    }
}

fn binary_symbol(operator: BinaryOperator) -> &'static str {
    match operator {
        BinaryOperator::Add => "+",
        BinaryOperator::Subtract => "-",
        BinaryOperator::Multiply => "*",
        BinaryOperator::Divide => "/",
        BinaryOperator::Power => "**",
    }
}

fn format_child(
    value: &EvaluatedValue,
    parent_precedence: u8,
    same_precedence_right: bool,
) -> String {
    let needs_parentheses = value.precedence < parent_precedence
        || (same_precedence_right && value.precedence == parent_precedence);
    if needs_parentheses {
        format!("({})", value.display)
    } else {
        value.display.clone()
    }
}
