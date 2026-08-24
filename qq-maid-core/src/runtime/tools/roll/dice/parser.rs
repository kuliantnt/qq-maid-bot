//! Dice 表达式词法边界、递归下降解析和命令级重复前缀。

use super::ast::{BinaryOperator, SpecialDice, UnaryOperator};
use super::{
    DEFAULT_DIE_SIDES, DiceExpression, DiceExpressionError, DiceExpressionParse, DiceKeep,
    DiceNode, DiceRollSpec, DiceRollSpecParse, DiceTerm, MAX_AST_NODES, MAX_DICE_COUNT_PER_TERM,
    MAX_DICE_TERMS, MAX_DIE_SIDES, MAX_EXPRESSION_CHARS, MAX_MODIFIER, MAX_NESTING_DEPTH,
    MAX_PREFIX_INPUT_CHARS, MAX_REPETITIONS, MAX_SPECIAL_DICE_COUNT, MAX_TOTAL_DICE,
    MAX_TOTAL_ROLLS_PER_COMMAND,
};

/// 解析一个单轮骰子表达式。
pub(crate) fn parse_expression(input: &str) -> DiceExpressionParse {
    let input = input.trim();
    if !looks_like_dice_expression(input) {
        return DiceExpressionParse::NotDiceExpression;
    }
    if input.chars().count() > MAX_EXPRESSION_CHARS {
        return DiceExpressionParse::Invalid(DiceExpressionError::TooLong);
    }

    let mut parser = Parser::new(input);
    match parser.parse() {
        Ok(root) => {
            let expression = DiceExpression { root };
            match super::evaluator::validate_expression(&expression) {
                Ok(()) => DiceExpressionParse::Parsed(expression),
                Err(error) => DiceExpressionParse::Invalid(error),
            }
        }
        Err(error) => DiceExpressionParse::Invalid(error),
    }
}

/// 解析可能带 N# 重复前缀的命令骰式。
pub(crate) fn parse_roll_spec(input: &str) -> DiceRollSpecParse {
    let input = input.trim();
    let (repetitions, expression_text) = match input.find('#') {
        Some(hash) => {
            let count_text = input[..hash].trim();
            let expression_text = input[hash + 1..].trim();
            (parse_repeat_count(count_text), expression_text)
        }
        None => (Ok(1), input),
    };
    let repetitions = match repetitions {
        Ok(value) => value,
        Err(error) => return DiceRollSpecParse::Invalid(error),
    };

    match parse_expression(expression_text) {
        DiceExpressionParse::NotDiceExpression => DiceRollSpecParse::NotDiceExpression,
        DiceExpressionParse::Invalid(error) => DiceRollSpecParse::Invalid(error),
        DiceExpressionParse::Parsed(expression) => {
            let total_rolls = u32::from(repetitions).saturating_mul(expression.total_dice());
            if total_rolls > MAX_TOTAL_ROLLS_PER_COMMAND {
                return DiceRollSpecParse::Invalid(DiceExpressionError::TooManyRolls);
            }
            DiceRollSpecParse::Parsed(DiceRollSpec {
                expression,
                repetitions,
            })
        }
    }
}

/// 尝试解析骰子表达式加空格和自由文本原因的前缀形式。
#[cfg(test)]
pub(crate) fn parse_expression_prefix(input: &str) -> Option<(DiceExpression, &str)> {
    parse_roll_spec_prefix(input)
        .and_then(|(spec, reason)| (spec.repetitions == 1).then_some((spec.expression, reason)))
}

/// parse_expression_prefix 的重复投掷版本。
pub(crate) fn parse_roll_spec_prefix(input: &str) -> Option<(DiceRollSpec, &str)> {
    let input = input.trim();
    let max_boundary = prefix_boundary_limit(input)?;
    for (boundary, _) in input
        .char_indices()
        .rev()
        .filter(|(boundary, character)| *boundary <= max_boundary && character.is_whitespace())
    {
        let expression_text = input[..boundary].trim_end();
        let reason = input[boundary..].trim();
        if reason.is_empty() || matches!(reason.as_bytes().first(), Some(b'+' | b'-' | b'*' | b'/'))
        {
            continue;
        }
        if let DiceRollSpecParse::Parsed(spec) = parse_roll_spec(expression_text)
            && spec.expression.total_dice() > 0
        {
            return Some((spec, reason));
        }
    }
    None
}

/// 尝试解析没有空格分隔的“骰式 + 原因”形式，例如 `2d6原因`。
///
/// SealDice 用户经常把短命令和骰式连写；表达式本身仍先按完整语法解析，只有完整解析
/// 失败时才从字符边界回退寻找合法骰式前缀，避免把 `2d6k1` 等合法后缀误当成原因。
pub(crate) fn parse_roll_spec_compact_prefix(input: &str) -> Option<(DiceRollSpec, &str)> {
    let input = input.trim();
    let max_boundary = prefix_boundary_limit(input)?;
    for (boundary, _) in input
        .char_indices()
        .rev()
        .filter(|(boundary, _)| *boundary <= max_boundary)
    {
        if boundary == 0 {
            continue;
        }
        let expression_text = input[..boundary].trim_end();
        let reason = input[boundary..].trim();
        if expression_text.chars().any(char::is_whitespace)
            || reason.is_empty()
            || reason
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
            || matches!(reason.as_bytes().first(), Some(b'+' | b'-' | b'*' | b'/'))
        {
            continue;
        }
        // `b`、`p`、`d` 等单字母本身是合法骰式，但紧邻 ASCII 单词时更可能是
        // 自然语言开头；只有显式空格分隔或后接非 ASCII 文本时才按骰式处理。
        if expression_text.chars().count() == 1
            && expression_text
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic())
            && reason
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic())
        {
            continue;
        }
        if let DiceRollSpecParse::Parsed(spec) = parse_roll_spec(expression_text)
            && spec.expression.total_dice() > 0
        {
            return Some((spec, reason));
        }
    }
    None
}

/// 返回不超过表达式字符上限的候选边界。
///
/// `parse_roll_spec_prefix` 和紧凑前缀解析都要对候选前缀重新走一次完整解析；这里先
/// 对整段输入做一次长度限制，再限制边界范围，确保重复解析的输入规模有固定上界。
fn prefix_boundary_limit(input: &str) -> Option<usize> {
    if input.chars().count() > MAX_PREFIX_INPUT_CHARS {
        return None;
    }
    Some(
        input
            .char_indices()
            .nth(MAX_EXPRESSION_CHARS)
            .map_or(input.len(), |(boundary, _)| boundary),
    )
}

fn parse_repeat_count(count_text: &str) -> Result<u8, DiceExpressionError> {
    if count_text.is_empty() || !count_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(DiceExpressionError::InvalidSyntax);
    }
    let count = count_text
        .parse::<u32>()
        .map_err(|_| DiceExpressionError::NumberTooLarge)?;
    if !(1..=MAX_REPETITIONS).contains(&count) {
        return Err(DiceExpressionError::RepeatCountOutOfRange);
    }
    Ok(count as u8)
}

fn looks_like_dice_expression(input: &str) -> bool {
    let input = input.trim();
    let mut characters = input.chars().peekable();
    while matches!(characters.peek(), Some('+' | '-')) {
        characters.next();
    }
    match characters.next() {
        Some('(') => true,
        Some('d' | 'D') => {
            skip_whitespace(&mut characters);
            characters.peek().is_some_and(|character| {
                character.is_ascii_digit() || *character == '优' || *character == '劣'
            }) || characters.peek().is_none()
        }
        Some('b' | 'B' | 'p' | 'P' | 'f' | 'F') => {
            // 特殊骰允许单字母、数字参数和后续运算；紧邻 ASCII 字母时应视为
            // 英文自然语言，避免把 battle / Please 等单词报成无效骰式。
            characters
                .peek()
                .is_none_or(|character| !character.is_ascii_alphabetic())
        }
        Some(character) if character.is_ascii_digit() => {
            while characters
                .peek()
                .is_some_and(|character| character.is_ascii_digit())
            {
                characters.next();
            }
            skip_whitespace(&mut characters);
            match characters.peek().copied() {
                Some('d' | 'D') => {
                    characters.next();
                    skip_whitespace(&mut characters);
                    characters.peek().is_some_and(|value| {
                        value.is_ascii_digit() || *value == '优' || *value == '劣'
                    }) || characters.peek().is_none()
                }
                Some(
                    '#' | '+' | '-' | '*' | '/' | '(' | ')' | 'a' | 'A' | 'c' | 'C' | 'k' | 'K'
                    | 'q' | 'Q' | 'm' | 'M',
                )
                | None => true,
                _ => false,
            }
        }
        _ => false,
    }
}

fn skip_whitespace<I>(characters: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while characters
        .peek()
        .is_some_and(|character| character.is_whitespace())
    {
        characters.next();
    }
}

struct Parser {
    characters: Vec<char>,
    cursor: usize,
    nodes: usize,
    depth: usize,
    dice_terms: usize,
    total_dice: u32,
}

impl Parser {
    fn new(input: &str) -> Self {
        Self {
            characters: input.chars().collect(),
            cursor: 0,
            nodes: 0,
            depth: 0,
            dice_terms: 0,
            total_dice: 0,
        }
    }

    fn parse(&mut self) -> Result<DiceNode, DiceExpressionError> {
        let node = self.parse_add_sub()?;
        self.skip_whitespace();
        if self.cursor != self.characters.len() {
            return Err(DiceExpressionError::InvalidSyntax);
        }
        Ok(node)
    }

    fn parse_add_sub(&mut self) -> Result<DiceNode, DiceExpressionError> {
        let mut node = self.parse_mul_div()?;
        loop {
            self.skip_whitespace();
            let operator = match self.peek() {
                Some('+') => BinaryOperator::Add,
                Some('-') => BinaryOperator::Subtract,
                _ => break,
            };
            self.cursor += 1;
            let right = self.parse_mul_div()?;
            node = self.binary(operator, node, right)?;
        }
        Ok(node)
    }

    fn parse_mul_div(&mut self) -> Result<DiceNode, DiceExpressionError> {
        let mut node = self.parse_power()?;
        loop {
            self.skip_whitespace();
            let operator = match self.peek() {
                Some('*') if self.peek_next() == Some('*') => break,
                Some('*') => BinaryOperator::Multiply,
                Some('/') => BinaryOperator::Divide,
                _ => break,
            };
            self.cursor += 1;
            let right = self.parse_power()?;
            node = self.binary(operator, node, right)?;
        }
        Ok(node)
    }

    fn parse_power(&mut self) -> Result<DiceNode, DiceExpressionError> {
        let left = self.parse_unary()?;
        self.skip_whitespace();
        if self.peek() == Some('*') && self.peek_next() == Some('*') {
            self.cursor += 2;
            let right = self.parse_power()?;
            return self.binary(BinaryOperator::Power, left, right);
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<DiceNode, DiceExpressionError> {
        self.skip_whitespace();
        let operator = match self.peek() {
            Some('+') => Some(UnaryOperator::Positive),
            Some('-') => Some(UnaryOperator::Negative),
            _ => None,
        };
        if let Some(operator) = operator {
            self.cursor += 1;
            let operand = self.parse_unary()?;
            return self.node(DiceNode::Unary {
                operator,
                operand: Box::new(operand),
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<DiceNode, DiceExpressionError> {
        self.skip_whitespace();
        match self.peek() {
            Some('(') => {
                self.cursor += 1;
                self.depth += 1;
                if self.depth > MAX_NESTING_DEPTH {
                    return Err(DiceExpressionError::TooDeep);
                }
                let node = self.parse_add_sub()?;
                self.skip_whitespace();
                if self.peek() != Some(')') {
                    return Err(DiceExpressionError::InvalidSyntax);
                }
                self.cursor += 1;
                self.depth -= 1;
                Ok(node)
            }
            Some('d' | 'D') => {
                self.cursor += 1;
                self.parse_dice_term(1)
            }
            Some('b' | 'B') => {
                self.cursor += 1;
                self.parse_special(true)
            }
            Some('p' | 'P') => {
                self.cursor += 1;
                self.parse_special(false)
            }
            Some('f' | 'F') => Err(DiceExpressionError::InvalidSyntax),
            Some(character) if character.is_ascii_digit() => {
                let number = self.parse_unsigned()?;
                let saved = self.cursor;
                self.skip_whitespace();
                if matches!(self.peek(), Some('d' | 'D')) {
                    self.cursor += 1;
                    self.parse_dice_term(number)
                } else {
                    self.cursor = saved;
                    if number > MAX_MODIFIER {
                        return Err(DiceExpressionError::ModifierOutOfRange);
                    }
                    self.node(DiceNode::Number(i64::from(number)))
                }
            }
            _ => Err(DiceExpressionError::InvalidSyntax),
        }
    }

    fn parse_dice_term(&mut self, count: u32) -> Result<DiceNode, DiceExpressionError> {
        if !(1..=MAX_DICE_COUNT_PER_TERM).contains(&count) {
            return Err(DiceExpressionError::CountOutOfRange);
        }
        self.skip_whitespace();
        let sides = if !self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            u32::from(DEFAULT_DIE_SIDES)
        } else {
            self.parse_unsigned()?
        };
        if !(1..=MAX_DIE_SIDES).contains(&sides) {
            return Err(DiceExpressionError::SidesOutOfRange);
        }
        self.dice_terms += 1;
        if self.dice_terms > MAX_DICE_TERMS {
            return Err(DiceExpressionError::TooManyTerms);
        }

        let mut term = DiceTerm::plain(count as u8, sides as u8);
        self.parse_dice_suffix(&mut term)?;
        self.total_dice = self
            .total_dice
            .checked_add(u32::from(term.count))
            .ok_or(DiceExpressionError::TooManyDice)?;
        if self.total_dice > MAX_TOTAL_DICE {
            return Err(DiceExpressionError::TooManyDice);
        }
        self.node(DiceNode::Dice(term))
    }

    fn parse_dice_suffix(&mut self, term: &mut DiceTerm) -> Result<(), DiceExpressionError> {
        self.skip_whitespace();
        if self.consume_literal("优势") {
            if term.count != 1 {
                return Err(DiceExpressionError::InvalidSyntax);
            }
            term.count = 2;
            term.keep = DiceKeep::Highest(1);
            return Ok(());
        }
        if self.consume_literal("劣势") {
            if term.count != 1 {
                return Err(DiceExpressionError::InvalidSyntax);
            }
            term.count = 2;
            term.keep = DiceKeep::Lowest(1);
            return Ok(());
        }

        let keep = if self.consume_ascii_literal("kh") {
            Some((true, false))
        } else if self.consume_ascii_literal("kl") {
            Some((false, false))
        } else if self.consume_ascii_literal("dh") {
            Some((true, true))
        } else if self.consume_ascii_literal("dl") {
            Some((false, true))
        } else if self.consume_ascii_literal("k") {
            Some((true, false))
        } else if self.consume_ascii_literal("q") {
            Some((false, false))
        } else {
            None
        };
        let Some((highest, dropping)) = keep else {
            return Ok(());
        };
        let amount = if self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            self.parse_unsigned()?
        } else {
            1
        };
        if amount == 0 || amount > u32::from(term.count) {
            return Err(DiceExpressionError::KeepCountOutOfRange);
        }
        if dropping && amount == u32::from(term.count) {
            return Err(DiceExpressionError::KeepCountOutOfRange);
        }
        let amount = amount as u8;
        term.keep = match (highest, dropping) {
            (true, false) => DiceKeep::Highest(amount),
            (false, false) => DiceKeep::Lowest(amount),
            (true, true) => DiceKeep::DropHighest(amount),
            (false, true) => DiceKeep::DropLowest(amount),
        };
        Ok(())
    }

    fn parse_special(&mut self, bonus: bool) -> Result<DiceNode, DiceExpressionError> {
        let count = if self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            self.parse_unsigned()?
        } else {
            1
        };
        if !(1..=MAX_SPECIAL_DICE_COUNT).contains(&count) {
            return Err(DiceExpressionError::CountOutOfRange);
        }
        self.total_dice = self
            .total_dice
            .checked_add(2 + count)
            .ok_or(DiceExpressionError::TooManyDice)?;
        if self.total_dice > MAX_TOTAL_DICE {
            return Err(DiceExpressionError::TooManyDice);
        }
        self.node(DiceNode::Special(if bonus {
            SpecialDice::Bonus(count as u8)
        } else {
            SpecialDice::Penalty(count as u8)
        }))
    }

    fn parse_unsigned(&mut self) -> Result<u32, DiceExpressionError> {
        let start = self.cursor;
        let mut value = 0u32;
        while let Some(character) = self.peek() {
            if !character.is_ascii_digit() {
                break;
            }
            let digit = character
                .to_digit(10)
                .expect("ASCII digit must have a value");
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(digit))
                .ok_or(DiceExpressionError::NumberTooLarge)?;
            self.cursor += 1;
        }
        if self.cursor == start {
            return Err(DiceExpressionError::InvalidSyntax);
        }
        Ok(value)
    }

    fn binary(
        &mut self,
        operator: BinaryOperator,
        left: DiceNode,
        right: DiceNode,
    ) -> Result<DiceNode, DiceExpressionError> {
        self.node(DiceNode::Binary {
            operator,
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    fn node(&mut self, node: DiceNode) -> Result<DiceNode, DiceExpressionError> {
        self.nodes += 1;
        if self.nodes > MAX_AST_NODES {
            return Err(DiceExpressionError::TooManyNodes);
        }
        Ok(node)
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            self.cursor += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.characters.get(self.cursor).copied()
    }

    fn peek_next(&self) -> Option<char> {
        self.characters.get(self.cursor + 1).copied()
    }

    fn consume_literal(&mut self, literal: &str) -> bool {
        let characters = literal.chars().collect::<Vec<_>>();
        if self
            .characters
            .get(self.cursor..self.cursor + characters.len())
            == Some(&characters)
        {
            self.cursor += characters.len();
            true
        } else {
            false
        }
    }

    fn consume_ascii_literal(&mut self, literal: &str) -> bool {
        let matches = literal.chars().enumerate().all(|(offset, expected)| {
            self.characters
                .get(self.cursor + offset)
                .is_some_and(|actual| actual.eq_ignore_ascii_case(&expected))
        });
        if matches {
            self.cursor += literal.chars().count();
        }
        matches
    }
}
