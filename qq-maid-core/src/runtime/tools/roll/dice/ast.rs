//! Dice AST、后缀规则和规范化展示。

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiceTerm {
    pub(crate) count: u8,
    pub(crate) sides: u8,
    pub(crate) keep: DiceKeep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiceKeep {
    All,
    Highest(u8),
    Lowest(u8),
    DropHighest(u8),
    DropLowest(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpecialDice {
    Bonus(u8),
    Penalty(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnaryOperator {
    Positive,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DiceNode {
    Number(i64),
    Dice(DiceTerm),
    Special(SpecialDice),
    Unary {
        operator: UnaryOperator,
        operand: Box<DiceNode>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<DiceNode>,
        right: Box<DiceNode>,
    },
}

impl DiceTerm {
    pub(super) fn plain(count: u8, sides: u8) -> Self {
        Self {
            count,
            sides,
            keep: DiceKeep::All,
        }
    }

    pub(super) fn selected_count(&self) -> u32 {
        match self.keep {
            DiceKeep::All => u32::from(self.count),
            // SealDice 允许保留数量超过骰池，此时实际仍只能保留已有的全部骰子。
            // 范围分析必须同步取较小值，不能按不存在的骰子扩大理论上下界。
            DiceKeep::Highest(count) | DiceKeep::Lowest(count) => u32::from(count.min(self.count)),
            DiceKeep::DropHighest(count) | DiceKeep::DropLowest(count) => {
                u32::from(self.count - count)
            }
        }
    }
}

impl DiceKeep {
    pub(super) fn is_all(self) -> bool {
        matches!(self, Self::All)
    }
}

pub(super) fn format_node(node: &DiceNode, parent_precedence: u8, right_child: bool) -> String {
    let (display, precedence) = match node {
        DiceNode::Number(value) => (value.to_string(), 5),
        DiceNode::Dice(term) => (format_term(term), 5),
        DiceNode::Special(SpecialDice::Bonus(count)) => (format_special('b', *count), 5),
        DiceNode::Special(SpecialDice::Penalty(count)) => (format_special('p', *count), 5),
        DiceNode::Unary { operator, operand } => {
            let symbol = match operator {
                UnaryOperator::Positive => "+",
                UnaryOperator::Negative => "-",
            };
            (format!("{symbol}{}", format_node(operand, 4, true)), 4)
        }
        DiceNode::Binary {
            operator,
            left,
            right,
        } => {
            let precedence = binary_precedence(*operator);
            (
                format!(
                    "{}{}{}",
                    // Power 是右结合的；左侧同优先级子树必须保留括号，否则
                    // `(d6**2)**3` 会被回执或诊断重新解析成 `d6**(2**3)`。
                    format_node(left, precedence, matches!(operator, BinaryOperator::Power),),
                    binary_symbol(*operator),
                    format_node(right, precedence, true),
                ),
                precedence,
            )
        }
    };
    if precedence < parent_precedence || (right_child && precedence == parent_precedence) {
        format!("({display})")
    } else {
        display
    }
}

pub(super) fn format_term(term: &DiceTerm) -> String {
    let mut text = format!("{}d{}", term.count, term.sides);
    match term.keep {
        DiceKeep::All => {}
        DiceKeep::Highest(count) => text.push_str(&format!("k{count}")),
        DiceKeep::Lowest(count) => text.push_str(&format!("q{count}")),
        DiceKeep::DropHighest(count) => text.push_str(&format!("dh{count}")),
        DiceKeep::DropLowest(count) => text.push_str(&format!("dl{count}")),
    }
    text
}

fn format_special(kind: char, count: u8) -> String {
    if count == 1 {
        kind.to_string()
    } else {
        format!("{kind}{count}")
    }
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

/// 保留 Phase 1 结果结构中的修正值字段；复杂运算以求值 trace 为准。
pub(super) fn legacy_modifier(node: &DiceNode) -> Option<i32> {
    fn collect(node: &DiceNode, sign: i64, terms: &mut Vec<DiceTerm>, modifier: &mut i64) -> bool {
        match node {
            DiceNode::Dice(term) if term.keep.is_all() => {
                terms.push(term.clone());
                true
            }
            DiceNode::Number(value) => {
                *modifier += sign * value;
                true
            }
            DiceNode::Binary {
                operator: BinaryOperator::Add,
                left,
                right,
            } => collect(left, sign, terms, modifier) && collect(right, sign, terms, modifier),
            DiceNode::Binary {
                operator: BinaryOperator::Subtract,
                left,
                right,
            } => collect(left, sign, terms, modifier) && collect(right, -sign, terms, modifier),
            _ => false,
        }
    }

    let mut terms = Vec::new();
    let mut modifier = 0_i64;
    if !collect(node, 1, &mut terms, &mut modifier)
        || terms.is_empty()
        || !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&modifier)
    {
        return None;
    }
    Some(modifier as i32)
}
