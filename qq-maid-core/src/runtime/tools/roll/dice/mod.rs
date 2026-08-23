//! 通用骰子表达式领域门面。
//!
//! AST、解析和求值分别位于职责明确的子模块；本文件只保留对 Roll domain 的稳定门面、
//! 结构化结果和安全限制。解析器只接受无状态、可验证的骰点子集，未知语法不能静默交给
//! LLM 猜测，也不能让模型参与确定性的骰值计算。

mod ast;
mod evaluator;
mod parser;
#[cfg(test)]
mod tests;

use std::fmt;

use rand::RngExt;

use ast::{DiceNode, format_node, legacy_modifier};
use evaluator::{evaluate_node, node_range, node_roll_count};

pub(crate) use ast::{DiceKeep, DiceTerm};
#[cfg(test)]
pub(crate) use parser::parse_expression_prefix;
pub(crate) use parser::{
    parse_expression, parse_roll_spec, parse_roll_spec_compact_prefix, parse_roll_spec_prefix,
};

/// 默认娱乐骰子的面数。
pub(crate) const DEFAULT_DIE_SIDES: u8 = 20;
/// 单个骰子段允许的最大骰子数量。
pub(crate) const MAX_DICE_COUNT_PER_TERM: u32 = 100;
/// 单个骰子段允许的最大面数。
pub(crate) const MAX_DIE_SIDES: u32 = 100;
/// 一个表达式允许的最大骰子段数量。AST 节点总数还会单独限制。
pub(crate) const MAX_DICE_TERMS: usize = 8;
/// 一个表达式允许实际投掷的骰子总数，避免多段表达式放大计算和回执长度。
pub(crate) const MAX_TOTAL_DICE: u32 = 100;
/// 一次命令（含多轮）允许实际投掷的骰子总数。
pub(crate) const MAX_TOTAL_ROLLS_PER_COMMAND: u32 = 200;
/// 常数修正值和整数运算字面量的绝对值上限。
pub(crate) const MAX_MODIFIER: u32 = 1_000;
/// 表达式原文的字符数上限。
pub(crate) const MAX_EXPRESSION_CHARS: usize = 64;
/// N#expr 的重复次数上限。
pub(crate) const MAX_REPETITIONS: u32 = 20;

pub(crate) const MAX_AST_NODES: usize = 96;
pub(crate) const MAX_NESTING_DEPTH: usize = 8;
pub(crate) const MAX_SPECIAL_DICE_COUNT: u32 = 20;
pub(crate) const MAX_POWER_EXPONENT: i64 = 12;
pub(crate) const MAX_EVALUATED_ABS: i64 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiceExpression {
    root: DiceNode,
}

impl DiceExpression {
    pub(crate) fn default_d20() -> Self {
        Self {
            root: DiceNode::Dice(DiceTerm::plain(1, DEFAULT_DIE_SIDES)),
        }
    }

    pub(crate) fn is_single_unmodified(&self) -> bool {
        matches!(
            &self.root,
            DiceNode::Dice(DiceTerm {
                count: 1,
                keep: DiceKeep::All,
                ..
            })
        )
    }

    pub(crate) fn is_default_d20(&self) -> bool {
        matches!(
            &self.root,
            DiceNode::Dice(DiceTerm {
                count: 1,
                sides: DEFAULT_DIE_SIDES,
                keep: DiceKeep::All,
            })
        )
    }

    /// 计算表达式的理论范围，供 AI DM 选择难度和 Core 计算 DC。
    pub(crate) fn total_range(&self) -> (i32, i32) {
        let (minimum, maximum) =
            node_range(&self.root).unwrap_or((i64::from(i32::MIN), i64::from(i32::MAX)));
        (
            minimum.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            maximum.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        )
    }

    pub(crate) fn total_dice(&self) -> u32 {
        node_roll_count(&self.root)
    }

    /// 使用注入的 Roller 完成一次确定性计算。
    ///
    /// Roller 返回越界值时直接失败；正式路径使用下方的本地随机 Roller。
    pub(crate) fn roll<R: Roller>(&self, roller: &mut R) -> Result<RollResult, DiceRollError> {
        let evaluated = evaluate_node(&self.root, roller)?;
        let total = i32::try_from(evaluated.value).map_err(|_| DiceRollError::Overflow)?;
        Ok(RollResult {
            expression: self.clone(),
            rolls: evaluated.rolls,
            modifier: legacy_modifier(&self.root).unwrap_or(0),
            total,
            calculation: evaluated.display,
        })
    }
}

impl fmt::Display for DiceExpression {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&format_node(&self.root, 0, false))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DieRoll {
    pub(crate) sides: u8,
    pub(crate) value: u8,
    pub(crate) kept: bool,
}

/// 一次表达式计算的结构化结果；平台文案由上层投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RollResult {
    pub(crate) expression: DiceExpression,
    pub(crate) rolls: Vec<DieRoll>,
    /// 对简单 NdM+修正表达式保留的兼容字段；复杂 AST 以 calculation 为准。
    pub(crate) modifier: i32,
    pub(crate) total: i32,
    calculation: String,
}

impl RollResult {
    pub(crate) fn calculation(&self) -> String {
        self.calculation.clone()
    }
}

/// 一次命令实际执行的骰式；repetitions 不改变单轮表达式语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiceRollSpec {
    pub(crate) expression: DiceExpression,
    pub(crate) repetitions: u8,
}

/// 供生产随机源和测试替身共同实现的骰值生成接口。
pub(crate) trait Roller {
    fn roll(&mut self, sides: u8) -> u8;
}

impl<F> Roller for F
where
    F: FnMut(u8) -> u8,
{
    fn roll(&mut self, sides: u8) -> u8 {
        self(sides)
    }
}

/// 创建线程级本地随机 Roller；同一命令的多轮投掷复用一个随机源。
pub(crate) fn csprng_roller() -> impl Roller {
    let mut rng = rand::rng();
    move |sides| rng.random_range(1..=sides)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiceExpressionParse {
    NotDiceExpression,
    Parsed(DiceExpression),
    Invalid(DiceExpressionError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiceRollSpecParse {
    NotDiceExpression,
    Parsed(DiceRollSpec),
    Invalid(DiceExpressionError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiceExpressionError {
    TooLong,
    InvalidSyntax,
    NumberTooLarge,
    CountOutOfRange,
    SidesOutOfRange,
    TooManyTerms,
    TooManyDice,
    ModifierOutOfRange,
    TooManyNodes,
    TooDeep,
    KeepCountOutOfRange,
    RepeatCountOutOfRange,
    TooManyRolls,
    DivisionByZero,
    ExponentOutOfRange,
    ValueOutOfRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiceRollError {
    OutOfRange { sides: u8, value: u8 },
    DivisionByZero,
    Overflow,
}
