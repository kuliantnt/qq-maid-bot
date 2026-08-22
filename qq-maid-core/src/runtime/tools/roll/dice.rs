//! 通用骰子表达式引擎。
//!
//! 这里不依赖 Slash 命令或平台文案，只负责解析受限表达式、生成本地骰值并计算总和。
//! 规则系统和人物卡可以在后续阶段复用这些结构，而不需要把骰点逻辑重新复制到各个命令中。

use std::fmt;

use rand::RngExt;

/// 默认娱乐骰子的面数。
pub(crate) const DEFAULT_DIE_SIDES: u8 = 20;
/// 单个骰子段允许的最大骰子数量。
pub(crate) const MAX_DICE_COUNT_PER_TERM: u32 = 100;
/// 单个骰子段允许的最大面数。
pub(crate) const MAX_DIE_SIDES: u32 = 100;
/// 一个表达式允许的最大骰子段数量。
pub(crate) const MAX_DICE_TERMS: usize = 8;
/// 一个表达式允许实际投掷的骰子总数，避免多段表达式放大计算和回执长度。
pub(crate) const MAX_TOTAL_DICE: u32 = 100;
/// 常数修正值的绝对值上限。
pub(crate) const MAX_MODIFIER: u32 = 1_000;
/// 表达式原文的字符数上限。
pub(crate) const MAX_EXPRESSION_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiceTerm {
    pub(crate) count: u8,
    pub(crate) sides: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiceExpression {
    pub(crate) terms: Vec<DiceTerm>,
    pub(crate) modifier: i32,
}

impl DiceExpression {
    pub(crate) fn default_d20() -> Self {
        Self {
            terms: vec![DiceTerm {
                count: 1,
                sides: DEFAULT_DIE_SIDES,
            }],
            modifier: 0,
        }
    }

    pub(crate) fn is_single_unmodified(&self) -> bool {
        self.terms.len() == 1 && self.terms[0].count == 1 && self.modifier == 0
    }

    pub(crate) fn is_default_d20(&self) -> bool {
        self.is_single_unmodified() && self.terms[0].sides == DEFAULT_DIE_SIDES
    }

    /// 确定性计算表达式的理论总值范围，供 AI DM 制定 DC 和 Core 校验使用。
    ///
    /// 解析器已限制骰子数量、面数和修正值，因此这里使用 `i32` 不会溢出，也不需要
    /// 让模型重复解析或推算骰式。
    pub(crate) fn total_range(&self) -> (i32, i32) {
        let minimum = self.total_dice() as i32 + self.modifier;
        let maximum = self
            .terms
            .iter()
            .map(|term| i32::from(term.count) * i32::from(term.sides))
            .sum::<i32>()
            + self.modifier;
        (minimum, maximum)
    }

    pub(crate) fn total_dice(&self) -> u32 {
        self.terms.iter().map(|term| u32::from(term.count)).sum()
    }

    /// 使用注入的 Roller 完成一次确定性计算。
    ///
    /// Roller 返回越界值时直接返回错误，不把不可信的测试实现或未来规则实现的错误
    /// 当成成功骰点；正式路径使用下方的本地随机 Roller。
    pub(crate) fn roll<R: Roller>(&self, roller: &mut R) -> Result<RollResult, DiceRollError> {
        let mut rolls = Vec::with_capacity(self.total_dice() as usize);
        for term in &self.terms {
            for _ in 0..term.count {
                let value = roller.roll(term.sides);
                if !(1..=term.sides).contains(&value) {
                    return Err(DiceRollError::OutOfRange {
                        sides: term.sides,
                        value,
                    });
                }
                rolls.push(DieRoll {
                    sides: term.sides,
                    value,
                });
            }
        }

        let dice_total = rolls.iter().map(|roll| i32::from(roll.value)).sum::<i32>();
        Ok(RollResult {
            expression: self.clone(),
            rolls,
            modifier: self.modifier,
            total: dice_total + self.modifier,
        })
    }
}

impl fmt::Display for DiceExpression {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, term) in self.terms.iter().enumerate() {
            if index > 0 {
                formatter.write_str("+")?;
            }
            write!(formatter, "{}d{}", term.count, term.sides)?;
        }
        match self.modifier.cmp(&0) {
            std::cmp::Ordering::Greater => write!(formatter, "+{}", self.modifier),
            std::cmp::Ordering::Less => write!(formatter, "{}", self.modifier),
            std::cmp::Ordering::Equal => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DieRoll {
    pub(crate) sides: u8,
    pub(crate) value: u8,
}

/// 一次表达式计算的结构化结果；QQ / OneBot 文案由上层根据它投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RollResult {
    pub(crate) expression: DiceExpression,
    pub(crate) rolls: Vec<DieRoll>,
    pub(crate) modifier: i32,
    pub(crate) total: i32,
}

impl RollResult {
    /// 返回不含骰式名称和总和的确定性计算串，供不同命令回执复用。
    pub(crate) fn calculation(&self) -> String {
        let dice_calculation = self
            .rolls
            .iter()
            .map(|roll| roll.value.to_string())
            .collect::<Vec<_>>()
            .join(" + ");
        match self.modifier.cmp(&0) {
            std::cmp::Ordering::Greater => {
                format!("{dice_calculation} + {}", self.modifier)
            }
            std::cmp::Ordering::Less => {
                format!("{dice_calculation} - {}", self.modifier.unsigned_abs())
            }
            std::cmp::Ordering::Equal => dice_calculation,
        }
    }
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

/// 创建线程级本地随机 Roller。
///
/// Roller 在等待模型调用完成后才创建，避免把不可跨线程异步等待的随机句柄带过 await；
/// 同一条表达式只创建一次，因此该表达式内的多次投掷复用同一个本地随机源。
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiceRollError {
    OutOfRange { sides: u8, value: u8 },
}

/// 解析本阶段支持的骰子表达式。
///
/// 语法是一个或多个骰子段加一个可选常数修正，例如 `d20`、`2d6+1d8-2`。
/// 仅在输入看起来像骰子表达式时返回 Invalid；普通自然语言交给上层的 AI DM 路径。
pub(crate) fn parse_expression(input: &str) -> DiceExpressionParse {
    let input = input.trim();
    if !looks_like_dice_expression(input) {
        return DiceExpressionParse::NotDiceExpression;
    }
    if input.chars().count() > MAX_EXPRESSION_CHARS {
        return DiceExpressionParse::Invalid(DiceExpressionError::TooLong);
    }

    let compact = input.chars().filter(|character| !character.is_whitespace());
    let compact = compact.collect::<String>();
    match parse_compact_expression(compact.as_bytes()) {
        Ok(expression) => DiceExpressionParse::Parsed(expression),
        Err(error) => DiceExpressionParse::Invalid(error),
    }
}

/// 尝试解析“骰子表达式 + 空格 + 自然语言问题”的前缀形式。
///
/// 从最长的空白边界开始尝试，既支持 `1d20+3 问题`，也支持表达式内部带空格的
/// `1d20 + 3 问题`。问题部分不送入骰子解析器，因此不受骰子表达式 64 字符上限影响。
pub(crate) fn parse_expression_prefix(input: &str) -> Option<(DiceExpression, &str)> {
    let input = input.trim();
    for (boundary, _) in input
        .char_indices()
        .rev()
        .filter(|(_, character)| character.is_whitespace())
    {
        let expression_text = input[..boundary].trim_end();
        let query = input[boundary..].trim();
        if query.is_empty() || matches!(query.as_bytes().first(), Some(b'+' | b'-' | b'*' | b'/')) {
            continue;
        }
        if let DiceExpressionParse::Parsed(expression) = parse_expression(expression_text) {
            return Some((expression, query));
        }
    }
    None
}

fn looks_like_dice_expression(input: &str) -> bool {
    let mut characters = input.chars().peekable();
    match characters.next() {
        Some('d' | 'D') => {
            skip_whitespace(&mut characters);
            characters
                .next()
                .is_some_and(|character| character.is_ascii_digit())
        }
        Some(character) if character.is_ascii_digit() => {
            while characters
                .peek()
                .is_some_and(|character| character.is_ascii_digit())
            {
                characters.next();
            }
            skip_whitespace(&mut characters);
            if !matches!(characters.next(), Some('d' | 'D')) {
                return false;
            }
            skip_whitespace(&mut characters);
            characters
                .peek()
                .is_some_and(|character| character.is_ascii_digit())
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

fn parse_compact_expression(bytes: &[u8]) -> Result<DiceExpression, DiceExpressionError> {
    let mut cursor = 0;
    let first_term = parse_dice_term(bytes, &mut cursor)?;
    let mut terms = vec![first_term];
    let mut total_dice = u32::from(terms[0].count);
    let mut modifier = 0;
    let mut has_modifier = false;

    while cursor < bytes.len() {
        let operator = match bytes.get(cursor) {
            Some(b'+' | b'-') => {
                let operator = bytes[cursor];
                cursor += 1;
                operator
            }
            _ => return Err(DiceExpressionError::InvalidSyntax),
        };

        if next_component_is_dice(bytes, cursor) {
            if operator == b'-' || has_modifier {
                return Err(DiceExpressionError::InvalidSyntax);
            }
            if terms.len() >= MAX_DICE_TERMS {
                return Err(DiceExpressionError::TooManyTerms);
            }
            let term = parse_dice_term(bytes, &mut cursor)?;
            total_dice = total_dice
                .checked_add(u32::from(term.count))
                .ok_or(DiceExpressionError::TooManyDice)?;
            if total_dice > MAX_TOTAL_DICE {
                return Err(DiceExpressionError::TooManyDice);
            }
            terms.push(term);
            continue;
        }

        if has_modifier {
            return Err(DiceExpressionError::InvalidSyntax);
        }
        let value = parse_unsigned(bytes, &mut cursor)?;
        if cursor != bytes.len() {
            return Err(DiceExpressionError::InvalidSyntax);
        }
        if value > MAX_MODIFIER {
            return Err(DiceExpressionError::ModifierOutOfRange);
        }
        modifier = if operator == b'-' {
            -(value as i32)
        } else {
            value as i32
        };
        has_modifier = true;
    }

    Ok(DiceExpression { terms, modifier })
}

fn next_component_is_dice(bytes: &[u8], cursor: usize) -> bool {
    if matches!(bytes.get(cursor), Some(b'd' | b'D')) {
        return true;
    }
    let mut index = cursor;
    while bytes.get(index).is_some_and(|byte| byte.is_ascii_digit()) {
        index += 1;
    }
    index > cursor && matches!(bytes.get(index), Some(b'd' | b'D'))
}

fn parse_dice_term(bytes: &[u8], cursor: &mut usize) -> Result<DiceTerm, DiceExpressionError> {
    let count = if matches!(bytes.get(*cursor), Some(b'd' | b'D')) {
        1
    } else {
        parse_unsigned(bytes, cursor)?
    };
    if !matches!(bytes.get(*cursor), Some(b'd' | b'D')) {
        return Err(DiceExpressionError::InvalidSyntax);
    }
    *cursor += 1;
    let sides = parse_unsigned(bytes, cursor)?;
    if !(1..=MAX_DICE_COUNT_PER_TERM).contains(&count) {
        return Err(DiceExpressionError::CountOutOfRange);
    }
    if !(1..=MAX_DIE_SIDES).contains(&sides) {
        return Err(DiceExpressionError::SidesOutOfRange);
    }
    Ok(DiceTerm {
        count: count as u8,
        sides: sides as u8,
    })
}

fn parse_unsigned(bytes: &[u8], cursor: &mut usize) -> Result<u32, DiceExpressionError> {
    let start = *cursor;
    let mut value = 0u32;
    while let Some(byte) = bytes.get(*cursor).copied() {
        if !byte.is_ascii_digit() {
            break;
        }
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(byte - b'0')))
            .ok_or(DiceExpressionError::NumberTooLarge)?;
        *cursor += 1;
    }
    if *cursor == start {
        return Err(DiceExpressionError::InvalidSyntax);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_expression_shapes() {
        let cases = [
            ("d20", vec![(1, 20)], 0),
            ("2d6", vec![(2, 6)], 0),
            ("1d20+3", vec![(1, 20)], 3),
            ("2d6 + 1", vec![(2, 6)], 1),
            ("1d8+1d6+4", vec![(1, 8), (1, 6)], 4),
            ("1d20-2", vec![(1, 20)], -2),
        ];
        for (input, expected_terms, expected_modifier) in cases {
            let DiceExpressionParse::Parsed(expression) = parse_expression(input) else {
                panic!("expected expression to parse: {input}");
            };
            assert_eq!(
                expression
                    .terms
                    .iter()
                    .map(|term| (term.count, term.sides))
                    .collect::<Vec<_>>(),
                expected_terms,
                "{input}"
            );
            assert_eq!(expression.modifier, expected_modifier, "{input}");
        }
    }

    #[test]
    fn leaves_natural_language_for_the_dm_path() {
        assert_eq!(
            parse_expression("我有 2d6 个苹果吗"),
            DiceExpressionParse::NotDiceExpression
        );
        assert_eq!(
            parse_expression("我能不能通过 DC20 的门"),
            DiceExpressionParse::NotDiceExpression
        );
        assert_eq!(
            parse_expression("DC20"),
            DiceExpressionParse::NotDiceExpression
        );
        assert_eq!(
            parse_expression("20 days"),
            DiceExpressionParse::NotDiceExpression
        );
        assert_eq!(
            parse_expression("2 dogs"),
            DiceExpressionParse::NotDiceExpression
        );
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
            ("1d20-1d6", DiceExpressionError::InvalidSyntax),
            ("1d20+1+2", DiceExpressionError::InvalidSyntax),
            ("1d20+1001", DiceExpressionError::ModifierOutOfRange),
            ("1d20*2", DiceExpressionError::InvalidSyntax),
        ] {
            assert_eq!(
                parse_expression(input),
                DiceExpressionParse::Invalid(expected),
                "{input}"
            );
        }

        let too_many_terms = "1d1+1d1+1d1+1d1+1d1+1d1+1d1+1d1+1d1";
        assert_eq!(
            parse_expression(too_many_terms),
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
    fn rolls_all_dice_once_and_returns_structured_total() {
        let DiceExpressionParse::Parsed(expression) = parse_expression("1d8+1d6+4") else {
            panic!("expression should parse");
        };
        let mut values = [3, 5].into_iter();
        let result = expression
            .roll(&mut |sides| {
                assert!(matches!(sides, 8 | 6));
                values.next().expect("each die should be rolled once")
            })
            .unwrap();
        assert_eq!(result.rolls.len(), 2);
        assert_eq!(result.rolls[0].value, 3);
        assert_eq!(result.rolls[1].value, 5);
        assert_eq!(result.modifier, 4);
        assert_eq!(result.total, 12);
        assert!(values.next().is_none());
    }

    #[test]
    fn rejects_out_of_range_injected_rolls() {
        let expression = DiceExpression::default_d20();
        let error = expression.roll(&mut |_| 21).unwrap_err();
        assert_eq!(
            error,
            DiceRollError::OutOfRange {
                sides: 20,
                value: 21
            }
        );
    }

    #[test]
    fn canonical_expression_contains_modifier_and_all_terms() {
        let DiceExpressionParse::Parsed(expression) = parse_expression("1d8 + 1d6 - 4") else {
            panic!("expression should parse");
        };
        assert_eq!(expression.to_string(), "1d8+1d6-4");
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
        ] {
            let DiceExpressionParse::Parsed(expression) = parse_expression(input) else {
                panic!("expected expression to parse: {input}");
            };
            assert_eq!(expression.total_range(), expected, "{input}");
        }
    }
}
