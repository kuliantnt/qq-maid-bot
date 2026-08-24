//! 骰子 Slash 命令领域门面。
//!
//! 无参数命令保持本地 D20 快速路径，通用骰子表达式由 Core 本地结算；带问题的命令
//! 先让模型生成并校验判定方案，再本地生成骰值和结算结果。命令分派层只负责路由与响应投影。
//! 当前仓库尚无 Campaign / Rule Context，因此带问题路径固定使用 Entertainment DM；未来应由
//! active campaign 的 `rule_system` 在调用方确定性选择对应 Rule System，不能让模型猜测模式。
//! 明确的纯骰子表达式在这里始终直接进入 Dice Engine，不因未来存在正式跑团上下文而改变含义。

use std::time::{Duration, Instant};

use qq_maid_llm::provider::{DynLlmProvider, types::TokenUsage};
use serde_json::{Value, json};

use crate::{
    runtime::command::{ParsedCommand, parse_slash_command},
    util::metrics::LlmMetrics,
};

mod dice;
mod dm;
mod outcome;

use dice::{DiceExpression, DiceExpressionParse, DiceRollSpec, RollResult, Roller};
use dm::{DmCheckPlan, prepare_dm_check};
use outcome::render_dm_result;

/// AI DM 问题的字符数上限；超过上限时拒绝请求，不做静默截断。
pub(crate) const DM_QUERY_MAX_CHARS: usize = 500;
/// AI DM 是娱乐命令的轻量独立调用，超时后立即回退本地骰子表达式。
const DM_CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// 必须早于 Core 整轮超时结束 AI 调用，给日志、随机数生成和响应投影保留收口时间。
const DM_FALLBACK_RESERVE: Duration = Duration::from_millis(250);

const DM_FALLBACK_PREFIX: &str = "AI DM 暂时无法判断本次检定难度，本次仅进行普通 D20 投掷。";
const DM_EXPRESSION_FALLBACK_PREFIX: &str =
    "AI DM 暂时无法判断本次检定难度，本次仅进行指定骰子表达式投掷。";
const INVALID_DICE_EXPRESSION_REPLY: &str = "骰子表达式无效。示例：d20、2d6、1d20+3、1d8+1d6+4；单段骰子数量和面数均为 1–100，总骰子数不超过 100，最多 8 段，表达式不超过 64 个字符，修正值范围为 -1000 到 +1000。";
const REPEATED_DM_CHECK_REPLY: &str = "Entertainment DM 暂不支持带问题的重复骰点。请使用 `/roll d20 问题` 进行单次 AI 判定，或使用 `/r 2#d20 原因` 进行本地多轮骰点。";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RollCommand {
    Default,
    DiceExpression {
        expression: DiceExpression,
    },
    /// `/r`、`/rd` 的多轮或带原因本地投掷；原因只用于结果展示，不进入 AI DM。
    DiceBatch {
        expression: DiceExpression,
        repetitions: u8,
        reason: Option<String>,
    },
    DmCheck {
        expression: Option<DiceExpression>,
        query: String,
    },
    RepeatedDmCheckUnsupported,
    InvalidDiceExpression,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RollExecutionKind {
    Local,
    AiDmSuccess,
    AiDmFallback,
}

impl RollExecutionKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::AiDmSuccess => "ai_dm_success",
            Self::AiDmFallback => "ai_dm_fallback",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RollDcDiagnostics {
    dice_expression: String,
    dice_minimum: i32,
    dice_maximum: i32,
    difficulty: &'static str,
    computed_dc: i32,
    dc_strategy: &'static str,
}

impl RollDcDiagnostics {
    fn from_plan(expression: &DiceExpression, plan: &DmCheckPlan) -> Self {
        let (dice_minimum, dice_maximum) = expression.total_range();
        let computed = dm::compute_dc(expression, plan.difficulty);
        debug_assert_eq!(computed.value, plan.dc);
        Self {
            dice_expression: expression.to_string(),
            dice_minimum,
            dice_maximum,
            difficulty: plan.difficulty.key(),
            computed_dc: computed.value,
            dc_strategy: computed.strategy.as_str(),
        }
    }

    fn add_to(&self, diagnostics: &mut Value) {
        diagnostics["dice_expression"] = json!(self.dice_expression);
        diagnostics["dice_minimum"] = json!(self.dice_minimum);
        diagnostics["dice_maximum"] = json!(self.dice_maximum);
        diagnostics["difficulty"] = json!(self.difficulty);
        diagnostics["computed_dc"] = json!(self.computed_dc);
        diagnostics["dc_strategy"] = json!(self.dc_strategy);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RollFallbackReason {
    code: String,
    stage: String,
}

/// roll 领域的结构化执行结果；命令分派层只负责投影，不重新判断业务状态。
pub(crate) struct RollExecutionResult {
    pub(crate) reply: String,
    pub(crate) metrics: LlmMetrics,
    pub(crate) usage: Option<TokenUsage>,
    execution_kind: RollExecutionKind,
    provider_fallback_used: Option<bool>,
    fallback_reason: Option<RollFallbackReason>,
    dc_diagnostics: Option<RollDcDiagnostics>,
}

impl RollExecutionResult {
    pub(crate) fn diagnostics(&self) -> Value {
        let mut diagnostics = json!({
            "backend": if matches!(self.execution_kind, RollExecutionKind::Local) {
                "rust"
            } else {
                "llm"
            },
            "session_backend": "rust",
            "used_memory": false,
            "used_search": false,
            "roll_execution_kind": self.execution_kind.as_str(),
            "roll_provider": self.metrics.provider,
            "roll_model": self.metrics.model,
            "roll_total_latency_ms": self.metrics.total_latency_ms,
            "roll_fallback_used": self.fallback_reason.is_some(),
        });
        if let Some(provider_fallback_used) = self.provider_fallback_used {
            diagnostics["roll_provider_fallback_used"] = json!(provider_fallback_used);
        }
        if let Some(reason) = &self.fallback_reason {
            diagnostics["roll_fallback_reason"] = json!(reason.code);
            diagnostics["roll_fallback_stage"] = json!(reason.stage);
        }
        if let Some(dc_diagnostics) = &self.dc_diagnostics {
            dc_diagnostics.add_to(&mut diagnostics);
        }
        diagnostics
    }
}

/// `/roll`（以及 `/r`）支持无参数本地 D20、通用骰子表达式，以及一个非空自然语言问题。
///
/// `/r`、`/rd` 的表达式尾随文本是本地骰点原因，不会调用模型；保留 `/roll` 的表达式加
/// 文本作为显式 Entertainment DM 语法，避免归一化命令时丢失用户原始入口语义。
pub(crate) fn parse_roll_command(text: &str) -> Option<RollCommand> {
    let command = parse_slash_command(text)
        .or_else(|| parse_compact_roll_command(text))
        .or_else(|| parse_dot_roll_command(text))?;
    if command.action != "roll" {
        return None;
    }

    if matches!(command.raw_command.as_str(), "rap" | "rab") {
        return parse_bonus_penalty_alias(&command);
    }
    if command.argument.is_empty() {
        Some(RollCommand::Default)
    } else {
        let local_reason_syntax =
            matches!(command.raw_command.as_str(), "r" | "rd" | "rap" | "rab");
        if let Some((spec, reason)) = parse_local_roll_argument(&command.argument) {
            if local_reason_syntax {
                return Some(local_roll_command(spec, reason));
            }
            if let Some(reason) = reason {
                // Entertainment DM 当前只接受一次表达式；重复骰点带问题不能静默丢掉
                // 问题并退化为本地骰点，否则用户会误以为模型已完成判定。
                if spec.repetitions != 1 {
                    return Some(RollCommand::RepeatedDmCheckUnsupported);
                }
                return Some(RollCommand::DmCheck {
                    expression: Some(spec.expression),
                    query: reason,
                });
            }
            return Some(local_roll_command(spec, None));
        }
        match dice::parse_roll_spec(&command.argument) {
            dice::DiceRollSpecParse::Parsed(spec) => Some(local_roll_command(spec, None)),
            dice::DiceRollSpecParse::NotDiceExpression => {
                if matches!(command.raw_command.as_str(), "r" | "rd") {
                    // SealDice 的 `/r 原因`、`/rd 原因` 与无空格写法语义一致：使用
                    // 默认 d20 在本地投掷，尾随文本只作为展示原因，不进入 AI DM。
                    return Some(local_roll_command(
                        DiceRollSpec {
                            expression: DiceExpression::default_d20(),
                            repetitions: 1,
                        },
                        Some(command.argument),
                    ));
                }
                Some(RollCommand::DmCheck {
                    expression: None,
                    query: command.argument,
                })
            }
            dice::DiceRollSpecParse::Invalid(_) => Some(RollCommand::InvalidDiceExpression),
        }
    }
}

fn parse_local_roll_argument(argument: &str) -> Option<(DiceRollSpec, Option<String>)> {
    if let dice::DiceRollSpecParse::Parsed(spec) = dice::parse_roll_spec(argument) {
        return Some((spec, None));
    }
    if let Some((spec, reason)) = dice::parse_roll_spec_prefix(argument) {
        return Some((spec, Some(reason.to_owned())));
    }
    if let Some((spec, reason)) = dice::parse_roll_spec_compact_prefix(argument) {
        return Some((spec, Some(reason.to_owned())));
    }
    None
}

fn local_roll_command(spec: DiceRollSpec, reason: Option<String>) -> RollCommand {
    if spec.repetitions == 1 && reason.is_none() {
        RollCommand::DiceExpression {
            expression: spec.expression,
        }
    } else {
        RollCommand::DiceBatch {
            expression: spec.expression,
            repetitions: spec.repetitions,
            reason,
        }
    }
}

fn parse_bonus_penalty_alias(command: &ParsedCommand) -> Option<RollCommand> {
    let expression = match dice::parse_expression(if command.raw_command == "rab" {
        "b"
    } else {
        "p"
    }) {
        DiceExpressionParse::Parsed(expression) => expression,
        _ => return None,
    };
    if command.argument.is_empty() {
        Some(RollCommand::DiceExpression { expression })
    } else {
        Some(RollCommand::DiceBatch {
            expression,
            repetitions: 1,
            reason: Some(command.argument.clone()),
        })
    }
}

/// 解析 `/r2d6`、`/rd20` 等 SealDice 常见的无空格命令形式。
///
/// 只把看起来确实是骰点后缀的短命令交给 Roll domain；`/rss`、`/rename` 等其他命令
/// 不会因为共享首字母而被误接管。命令前缀本身仍由上层 `CommandPrefix` 统一规范化。
fn parse_compact_roll_command(text: &str) -> Option<ParsedCommand> {
    let text = text.trim();
    let body = text.strip_prefix('/')?;
    let mut parts = body.splitn(2, char::is_whitespace);
    let token = parts.next()?.trim();
    let remainder = parts.next().unwrap_or("").trim();
    let lowercase = token.to_ascii_lowercase();

    for alias in ["rap", "rab"] {
        if lowercase.starts_with(alias) && lowercase.len() > alias.len() {
            // 无空格的紧凑别名只接受中文原因；ASCII 后缀必须走普通 Slash 解析，
            // 避免 `/rapid`、`/rabbit` 等未知命令被误接管成骰点。
            let suffix = &token[alias.len()..];
            if !is_cjk_reason_start(suffix) {
                continue;
            }
            let argument = join_compact_argument(&token[alias.len()..], remainder);
            return Some(ParsedCommand {
                action: "roll".to_owned(),
                argument,
                raw_command: alias.to_owned(),
            });
        }
    }

    let (raw_command, suffix) = if lowercase.starts_with("rd") && lowercase.len() > 2 {
        ("rd", &token[2..])
    } else if lowercase.starts_with('r') && lowercase.len() > 1 {
        ("r", &token[1..])
    } else {
        return None;
    };
    let local_reason_suffix = raw_command == "r" && is_cjk_reason_start(suffix);
    if !looks_like_compact_roll_suffix(suffix) && !local_reason_suffix {
        return None;
    }
    let expression = if local_reason_suffix {
        "d20".to_owned()
    } else if raw_command == "rd" {
        compact_rd_expression(suffix)
    } else {
        suffix.to_owned()
    };
    let argument = if local_reason_suffix {
        let reason = join_compact_argument(suffix, remainder);
        join_compact_argument("d20", &reason)
    } else {
        join_compact_argument(&expression, remainder)
    };
    Some(ParsedCommand {
        action: "roll".to_owned(),
        argument,
        raw_command: raw_command.to_owned(),
    })
}

fn is_cjk_reason_start(suffix: &str) -> bool {
    suffix.chars().next().is_some_and(
        |character| matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}'),
    )
}

/// 允许 Roll domain 单独处理原生 SealDice 点号入口；正常 Core 路由仍先由命令前缀统一规范化。
fn parse_dot_roll_command(text: &str) -> Option<ParsedCommand> {
    let text = text.trim();
    let remainder = text.strip_prefix('.').or_else(|| text.strip_prefix('。'))?;
    parse_compact_roll_command(&format!("/{remainder}"))
        .or_else(|| parse_slash_command(&format!("/{remainder}")))
}

fn join_compact_argument(expression: &str, remainder: &str) -> String {
    if remainder.is_empty() {
        expression.to_owned()
    } else {
        format!("{expression} {remainder}")
    }
}

fn looks_like_compact_roll_suffix(suffix: &str) -> bool {
    suffix.chars().next().is_some_and(|character| {
        character.is_ascii_digit()
            || matches!(
                character,
                'd' | 'D'
                    | 'b'
                    | 'B'
                    | 'p'
                    | 'P'
                    | 'f'
                    | 'F'
                    | 'k'
                    | 'K'
                    | 'q'
                    | 'Q'
                    | '('
                    | '+'
                    | '-'
                    | '#'
                    | '优'
                    | '劣'
            )
    })
}

fn compact_rd_expression(suffix: &str) -> String {
    // SealDice 的 `d` 是默认骰表达式，`d优势`/`d劣势` 会先展开为双 D20 取高/取低；
    // 后续的 `+6`、`+1d4` 等仍属于同一条表达式，不能把“优势”当作普通原因截断。
    if suffix.starts_with("优势") || suffix.starts_with("劣势") {
        return format!("d20{suffix}");
    }
    let starts_with_digit = suffix
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit());
    let contains_dice_separator = suffix
        .chars()
        .any(|character| matches!(character, 'd' | 'D'));
    if starts_with_digit && contains_dice_separator {
        suffix.to_owned()
    } else if starts_with_digit {
        format!("d{suffix}")
    } else if suffix
        .chars()
        .next()
        .is_some_and(|character| matches!(character, 'd' | 'D'))
    {
        suffix.to_owned()
    } else {
        format!("d20{suffix}")
    }
}

/// 执行骰点命令，并把已有 `/set 昵称` 展示名仅用于本地结果投影。
///
/// `prepare_dm_check` 的 API 不接收骰值；只有它返回已校验的不可变方案后，才会调用
/// 本地 roller。这个调用顺序是防止模型看到骰值后调整 DC 的核心安全边界。
pub(crate) async fn execute_roll_command_with_display_name(
    provider: &DynLlmProvider,
    model: Option<String>,
    command: RollCommand,
    request_budget: Duration,
    display_name: Option<String>,
) -> RollExecutionResult {
    execute_roll_command_with_roller_factory(
        provider,
        model,
        command,
        dm_timeout_for_request(request_budget),
        thread_roller,
        display_name,
    )
    .await
}

fn dm_timeout_for_request(request_budget: Duration) -> Duration {
    DM_CHECK_TIMEOUT.min(request_budget.saturating_sub(DM_FALLBACK_RESERVE))
}

/// 按需创建线程级 RNG，避免不可 `Send` 的句柄跨越 AI DM 的异步模型调用。
/// 同一条 NdM 命令只创建一次 roller，因此连续投掷会复用同一个 RNG 句柄。
fn thread_roller() -> impl Roller {
    dice::csprng_roller()
}

#[cfg(test)]
async fn execute_roll_command_with_roller<F>(
    provider: &DynLlmProvider,
    model: Option<String>,
    command: RollCommand,
    dm_timeout: Duration,
    roller: F,
) -> RollExecutionResult
where
    F: Roller,
{
    execute_roll_command_with_roller_factory(provider, model, command, dm_timeout, || roller, None)
        .await
}

async fn execute_roll_command_with_roller_factory<RF, F>(
    provider: &DynLlmProvider,
    model: Option<String>,
    command: RollCommand,
    dm_timeout: Duration,
    roller_factory: RF,
    display_name: Option<String>,
) -> RollExecutionResult
where
    RF: FnOnce() -> F,
    F: Roller,
{
    let (query, expression) = match command {
        RollCommand::Default => {
            let mut roller = roller_factory();
            let (result, _) = roll_expression_result(None, &mut roller);
            return local_result(roll_dice_expression_reply_with_context(
                &result,
                display_name.as_deref(),
                None,
            ));
        }
        RollCommand::DiceExpression { expression } => {
            let mut roller = roller_factory();
            let (result, _) = roll_expression_result(Some(expression), &mut roller);
            return local_result(roll_dice_expression_reply_with_context(
                &result,
                display_name.as_deref(),
                None,
            ));
        }
        RollCommand::DiceBatch {
            expression,
            repetitions,
            reason,
        } => {
            let mut roller = roller_factory();
            let results = roll_expression_results(expression, repetitions, &mut roller);
            return local_result(roll_dice_results_reply(
                &results,
                display_name.as_deref(),
                reason.as_deref(),
            ));
        }
        RollCommand::InvalidDiceExpression => {
            return local_result(INVALID_DICE_EXPRESSION_REPLY.to_owned());
        }
        RollCommand::RepeatedDmCheckUnsupported => {
            return local_result(REPEATED_DM_CHECK_REPLY.to_owned());
        }
        RollCommand::DmCheck { expression, query } => (query, expression),
    };

    let query_chars = query.chars().count();
    if query_chars > DM_QUERY_MAX_CHARS {
        return local_result(format!(
            "判定问题过长，请控制在 {DM_QUERY_MAX_CHARS} 个字符以内。"
        ));
    }

    let requested_expression = expression.is_some();
    let expression = expression.unwrap_or_else(DiceExpression::default_d20);
    let started_at = Instant::now();
    let prepared = tokio::time::timeout(
        dm_timeout,
        prepare_dm_check(provider, model.clone(), query.as_str(), &expression),
    )
    .await;
    match prepared {
        Ok(Ok(prepared)) => {
            // 此处是本轮第一次生成实际骰值；上面的模型 future 已完成且方案已通过校验。
            let mut roller = roller_factory();
            let roll = expression
                .roll(&mut roller)
                .expect("本地 Roller 必须返回有效骰值");
            RollExecutionResult {
                reply: render_dm_result(&prepared.plan, &roll),
                metrics: prepared.metrics,
                usage: prepared.usage,
                execution_kind: RollExecutionKind::AiDmSuccess,
                provider_fallback_used: Some(prepared.provider_fallback_used),
                fallback_reason: None,
                dc_diagnostics: Some(RollDcDiagnostics::from_plan(&expression, &prepared.plan)),
            }
        }
        Ok(Err(failure)) => {
            tracing::warn!(
                error_code = failure.error.code,
                error_stage = failure.error.stage,
                query_chars,
                "AI DM 判定方案生成失败，降级为本地骰子投掷"
            );
            let mut roller = roller_factory();
            let metrics = failure.metrics.unwrap_or_else(|| {
                requested_model_metrics(provider, model.as_deref(), started_at.elapsed())
            });
            let roll = expression
                .roll(&mut roller)
                .expect("本地 Roller 必须返回有效骰值");
            RollExecutionResult {
                reply: roll_fallback_reply_from_result(
                    &roll,
                    requested_expression,
                    display_name.as_deref(),
                ),
                metrics,
                usage: failure.usage,
                execution_kind: RollExecutionKind::AiDmFallback,
                provider_fallback_used: failure.provider_fallback_used,
                fallback_reason: Some(RollFallbackReason {
                    code: failure.error.code.to_owned(),
                    stage: failure.error.stage.to_owned(),
                }),
                dc_diagnostics: None,
            }
        }
        Err(_) => {
            tracing::warn!(
                error_code = "timeout",
                error_stage = "roll_dm",
                query_chars,
                "AI DM 判定方案生成超时，降级为本地骰子投掷"
            );
            let mut roller = roller_factory();
            let roll = expression
                .roll(&mut roller)
                .expect("本地 Roller 必须返回有效骰值");
            RollExecutionResult {
                reply: roll_fallback_reply_from_result(
                    &roll,
                    requested_expression,
                    display_name.as_deref(),
                ),
                metrics: requested_model_metrics(provider, model.as_deref(), started_at.elapsed()),
                usage: None,
                execution_kind: RollExecutionKind::AiDmFallback,
                provider_fallback_used: None,
                fallback_reason: Some(RollFallbackReason {
                    code: "timeout".to_owned(),
                    stage: "roll_dm".to_owned(),
                }),
                dc_diagnostics: None,
            }
        }
    }
}

fn local_result(reply: String) -> RollExecutionResult {
    RollExecutionResult {
        reply,
        metrics: LlmMetrics {
            provider: "rust".to_owned(),
            model: "roll-local".to_owned(),
            stream: false,
            ttfe_ms: None,
            ttft_ms: None,
            total_latency_ms: 0,
        },
        usage: None,
        execution_kind: RollExecutionKind::Local,
        provider_fallback_used: None,
        fallback_reason: None,
        dc_diagnostics: None,
    }
}

fn requested_model_metrics(
    provider: &DynLlmProvider,
    model: Option<&str>,
    elapsed: Duration,
) -> LlmMetrics {
    LlmMetrics {
        provider: provider.name().to_owned(),
        model: model.unwrap_or_else(|| provider.model()).to_owned(),
        stream: false,
        ttfe_ms: None,
        ttft_ms: None,
        total_latency_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
    }
}

fn roll_expression_result<R>(
    expression: Option<DiceExpression>,
    roller: &mut R,
) -> (RollResult, bool)
where
    R: Roller,
{
    let requested_expression = expression.is_some();
    let expression = expression.unwrap_or_else(DiceExpression::default_d20);
    let result = expression
        .roll(roller)
        .expect("本地 Roller 必须返回有效骰值");
    (result, requested_expression)
}

fn roll_expression_results<R>(
    expression: DiceExpression,
    repetitions: u8,
    roller: &mut R,
) -> Vec<RollResult>
where
    R: Roller,
{
    (0..repetitions)
        .map(|_| {
            expression
                .roll(roller)
                .expect("本地 Roller 必须返回有效骰值")
        })
        .collect()
}

fn roll_dice_expression_reply_with_context(
    result: &RollResult,
    display_name: Option<&str>,
    reason: Option<&str>,
) -> String {
    let prefix = roll_context_prefix(display_name, reason);
    format!("🎲 {prefix}{}", roll_detail(result))
}

fn roll_dice_results_reply(
    results: &[RollResult],
    display_name: Option<&str>,
    reason: Option<&str>,
) -> String {
    let prefix = roll_context_prefix(display_name, reason);
    if results.len() == 1 {
        return format!("🎲 {prefix}{}", roll_detail(&results[0]));
    }

    let rounds = results
        .iter()
        .enumerate()
        .map(|(index, result)| format!("第{}轮：{}", index + 1, roll_detail(result)))
        .collect::<Vec<_>>()
        .join("\n");
    format!("🎲 {prefix}多轮投掷\n{rounds}")
}

fn roll_context_prefix(display_name: Option<&str>, reason: Option<&str>) -> String {
    match (display_name, reason) {
        (Some(display_name), Some(reason)) => format!("<{display_name}> 的“{reason}” "),
        (Some(display_name), None) => format!("<{display_name}> "),
        (None, Some(reason)) => format!("“{reason}” "),
        (None, None) => String::new(),
    }
}

fn roll_detail(result: &RollResult) -> String {
    if result.expression.is_single_unmodified() {
        let roll = result.rolls.first().expect("单骰表达式必须产生一个骰值");
        return format!("掷出了 {} / {}", roll.value, roll.sides);
    }

    let calculation = result.calculation();
    format!("{}：{calculation} = {}", result.expression, result.total)
}

fn roll_fallback_reply_from_result(
    result: &RollResult,
    requested_expression: bool,
    display_name: Option<&str>,
) -> String {
    let prefix = if requested_expression {
        DM_EXPRESSION_FALLBACK_PREFIX
    } else {
        DM_FALLBACK_PREFIX
    };
    format!(
        "{prefix}\n\n{}",
        roll_dice_expression_reply_with_context(result, display_name, None)
    )
}

#[cfg(test)]
mod tests;
