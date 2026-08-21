//! 骰子 Slash 命令领域门面。
//!
//! 无参数命令保持本地 D20 快速路径，简单骰子表达式由 Core 本地结算；带问题的命令
//! 先让模型生成并校验判定方案，再本地生成骰值和结算结果。命令分派层只负责路由与响应投影。

use std::{sync::OnceLock, time::Duration};

use qq_maid_llm::provider::DynLlmProvider;
use regex::Regex;

use crate::runtime::command::parse_slash_command;

mod dm;
mod outcome;

use dm::prepare_dm_check;
use outcome::{DEFAULT_DIE_SIDES, RollResult, render_dm_result};

/// AI DM 问题的字符数上限；超过上限时拒绝请求，不做静默截断。
pub(crate) const DM_QUERY_MAX_CHARS: usize = 500;
/// AI DM 是娱乐命令的轻量独立调用，超时后立即回退本地 D20。
const DM_CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DICE_COUNT: u8 = 100;
const MAX_DIE_SIDES: u8 = 100;

const DM_FALLBACK_PREFIX: &str = "AI DM 暂时无法判断本次检定难度，本次仅进行普通 D20 投掷。";
const UNSUPPORTED_DICE_EXPRESSION_REPLY: &str =
    "暂不支持该骰子表达式。目前支持 dM 或 NdM（骰子个数和面数均为 1–100）。";

static DICE_EXPRESSION_RE: OnceLock<Regex> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RollCommand {
    Default,
    DiceExpression { count: u8, sides: u8 },
    DmCheck { query: String },
    UnsupportedDiceExpression,
}

enum ParsedDiceExpression {
    Supported { count: u8, sides: u8 },
    Unsupported,
}

/// `/roll` 支持无参数本地 D20、简单 `NdM` 骰子表达式，以及一个非空自然语言问题。
pub(crate) fn parse_roll_command(text: &str) -> Option<RollCommand> {
    let command = parse_slash_command(text)?;
    if command.action != "roll" {
        return None;
    }
    if command.argument.is_empty() {
        Some(RollCommand::Default)
    } else {
        match parse_dice_expression(&command.argument) {
            Some(ParsedDiceExpression::Supported { count, sides }) => {
                Some(RollCommand::DiceExpression { count, sides })
            }
            Some(ParsedDiceExpression::Unsupported) => Some(RollCommand::UnsupportedDiceExpression),
            None => Some(RollCommand::DmCheck {
                query: command.argument,
            }),
        }
    }
}

/// 只匹配完整参数，避免自然语言中出现 `2d6` 或 `DC20` 时误判为骰子命令。
///
/// `+/-` 修正值仍会被识别并明确拒绝；个数与面数限制用于约束工作量和回复长度。
fn parse_dice_expression(argument: &str) -> Option<ParsedDiceExpression> {
    let captures = DICE_EXPRESSION_RE
        .get_or_init(|| {
            Regex::new(r"(?i)\A(?P<count>[0-9]+)?d(?P<sides>[0-9]+)(?P<modifier>[+-][0-9]+)?\z")
                .expect("dice expression regex must compile")
        })
        .captures(argument)?;

    if captures.name("modifier").is_some() {
        return Some(ParsedDiceExpression::Unsupported);
    }
    let count = captures
        .name("count")
        .map_or(Ok(1), |value| value.as_str().parse::<u8>());
    let sides = captures["sides"].parse::<u8>();
    match (count, sides) {
        (Ok(count @ 1..=MAX_DICE_COUNT), Ok(sides @ 1..=MAX_DIE_SIDES)) => {
            Some(ParsedDiceExpression::Supported { count, sides })
        }
        _ => Some(ParsedDiceExpression::Unsupported),
    }
}

/// 执行一次 `/roll` 命令。
///
/// `prepare_dm_check` 的 API 不接收骰值；只有它返回已校验的不可变方案后，才会调用
/// 本地 roller。这个调用顺序是防止模型看到骰值后调整 DC 的核心安全边界。
pub(crate) async fn execute_roll_command(
    provider: &DynLlmProvider,
    model: Option<String>,
    command: RollCommand,
) -> String {
    let mut rng = fastrand::Rng::new();
    execute_roll_command_with_roller(provider, model, command, DM_CHECK_TIMEOUT, move |sides| {
        if sides == DEFAULT_DIE_SIDES {
            roll_default_with_rng(&mut rng).value
        } else {
            rng.u8(1..=sides)
        }
    })
    .await
}

async fn execute_roll_command_with_roller<F>(
    provider: &DynLlmProvider,
    model: Option<String>,
    command: RollCommand,
    dm_timeout: Duration,
    mut roller: F,
) -> String
where
    F: FnMut(u8) -> u8,
{
    let query = match command {
        RollCommand::Default => return roll_default_reply_from_value(roller(DEFAULT_DIE_SIDES)),
        RollCommand::DiceExpression { count, sides } => {
            return roll_dice_expression_reply(count, sides, &mut roller);
        }
        RollCommand::UnsupportedDiceExpression => {
            return UNSUPPORTED_DICE_EXPRESSION_REPLY.to_owned();
        }
        RollCommand::DmCheck { query } => query,
    };

    let query_chars = query.chars().count();
    if query_chars > DM_QUERY_MAX_CHARS {
        return format!("判定问题过长，请控制在 {DM_QUERY_MAX_CHARS} 个字符以内。");
    }

    let prepared = tokio::time::timeout(
        dm_timeout,
        prepare_dm_check(provider, model, query.as_str()),
    )
    .await;
    match prepared {
        Ok(Ok(plan)) => {
            // 此处是本轮第一次生成实际骰值；上面的模型 future 已完成且方案已通过校验。
            render_dm_result(&plan, roller(DEFAULT_DIE_SIDES))
        }
        Ok(Err(error)) => {
            tracing::warn!(
                error_code = error.code,
                error_stage = error.stage,
                query_chars,
                "AI DM 判定方案生成失败，降级为普通 D20"
            );
            roll_fallback_reply_from_value(roller(DEFAULT_DIE_SIDES))
        }
        Err(_) => {
            tracing::warn!(
                error_code = "timeout",
                error_stage = "roll_dm",
                query_chars,
                "AI DM 判定方案生成超时，降级为普通 D20"
            );
            roll_fallback_reply_from_value(roller(DEFAULT_DIE_SIDES))
        }
    }
}

fn roll_default_reply_from_value(value: u8) -> String {
    format!("🎲 掷出了 {value} / {DEFAULT_DIE_SIDES}")
}

fn roll_dice_expression_reply<F>(count: u8, sides: u8, roller: &mut F) -> String
where
    F: FnMut(u8) -> u8,
{
    let values = (0..count).map(|_| roller(sides)).collect::<Vec<_>>();
    if count == 1 {
        return format!("🎲 掷出了 {} / {sides}", values[0]);
    }

    let total = values.iter().map(|value| u16::from(*value)).sum::<u16>();
    let calculation = values
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(" + ");
    format!("🎲 {count}d{sides}：{calculation} = {total}")
}

fn roll_fallback_reply_from_value(value: u8) -> String {
    format!(
        "{DM_FALLBACK_PREFIX}\n\n{}",
        roll_default_reply_from_value(value)
    )
}

fn roll_default_with_rng(rng: &mut fastrand::Rng) -> RollResult {
    RollResult {
        value: rng.u8(1..=DEFAULT_DIE_SIDES),
        sides: DEFAULT_DIE_SIDES,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use qq_maid_llm::provider::{
        ChatOutcome, LlmProvider,
        types::{ChatRequest, TokenUsage},
    };

    use crate::{error::LlmError, util::metrics::LlmMetrics};

    use super::*;

    #[derive(Clone)]
    struct MockProvider {
        result: Result<String, LlmError>,
        requests: Arc<Mutex<Vec<ChatRequest>>>,
        events: Arc<Mutex<Vec<&'static str>>>,
        delay: Duration,
    }

    impl MockProvider {
        fn replying(reply: &str) -> Self {
            Self {
                result: Ok(reply.to_owned()),
                requests: Arc::new(Mutex::new(Vec::new())),
                events: Arc::new(Mutex::new(Vec::new())),
                delay: Duration::ZERO,
            }
        }

        fn failing(error: LlmError) -> Self {
            Self {
                result: Err(error),
                requests: Arc::new(Mutex::new(Vec::new())),
                events: Arc::new(Mutex::new(Vec::new())),
                delay: Duration::ZERO,
            }
        }

        fn delayed(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
            self.events.lock().unwrap().push("model");
            self.requests.lock().unwrap().push(req.clone());
            tokio::time::sleep(self.delay).await;
            let reply = self.result.clone()?;
            Ok(ChatOutcome {
                reply,
                output_parts: Vec::new(),
                metrics: LlmMetrics {
                    provider: "mock".to_owned(),
                    model: req.model.unwrap_or_else(|| "mock-model".to_owned()),
                    stream: false,
                    ttfe_ms: None,
                    ttft_ms: None,
                    total_latency_ms: 1,
                },
                usage: Some(TokenUsage {
                    input_tokens: None,
                    cached_input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                }),
                fallback_used: false,
                agent: Default::default(),
            })
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn model(&self) -> &str {
            "mock-model"
        }

        fn stream_enabled(&self) -> bool {
            false
        }
    }

    fn fortune_json() -> &'static str {
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"今晚适合出门","failure_meaning":"今晚适合宅家"}"#
    }

    #[test]
    fn parses_default_dm_supported_and_unsupported_dice_expressions() {
        assert_eq!(parse_roll_command("/roll"), Some(RollCommand::Default));
        assert_eq!(parse_roll_command("  /ROLL  "), Some(RollCommand::Default));
        assert_eq!(
            parse_roll_command(" /RoLl   晚上要不要出门  "),
            Some(RollCommand::DmCheck {
                query: "晚上要不要出门".to_owned(),
            })
        );
        for (input, count, sides) in [
            ("/roll d20", 1, 20),
            ("/roll d100", 1, 100),
            ("/roll 2d6", 2, 6),
            ("/roll 1D100", 1, 100),
        ] {
            assert_eq!(
                parse_roll_command(input),
                Some(RollCommand::DiceExpression { count, sides }),
                "{input}"
            );
        }
        for input in [
            "/roll 1d20+3",
            "/roll 2d10-1",
            "/roll 2D6-1",
            "/roll 101d6",
            "/roll d101",
            "/roll 0d6",
            "/roll d0",
        ] {
            assert_eq!(
                parse_roll_command(input),
                Some(RollCommand::UnsupportedDiceExpression),
                "{input}"
            );
        }
        for (input, query) in [
            ("/roll 我能不能通过 DC20 的门", "我能不能通过 DC20 的门"),
            ("/roll 我有 2d6 个苹果吗", "我有 2d6 个苹果吗"),
        ] {
            assert_eq!(
                parse_roll_command(input),
                Some(RollCommand::DmCheck {
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
    fn default_roll_uses_d20_and_stays_in_inclusive_range() {
        // 固定种子只验证范围不变量，不断言多次结果必须不同，避免概率性测试。
        let mut rng = fastrand::Rng::with_seed(20);
        for _ in 0..4_096 {
            let result = roll_default_with_rng(&mut rng);
            assert_eq!(result.sides, 20);
            assert!((1..=20).contains(&result.value));
        }
    }

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
        assert_eq!(reply, "🎲 掷出了 13 / 20");
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
            RollCommand::DiceExpression { count: 2, sides: 6 },
            Duration::from_secs(1),
            |sides| {
                assert_eq!(sides, 6);
                values.next().expect("2d6 should roll exactly twice")
            },
        )
        .await;

        assert_eq!(reply, "🎲 2d6：2 + 5 = 7");
        assert!(values.next().is_none());
        assert!(requests.lock().unwrap().is_empty());

        let reply = execute_roll_command_with_roller(
            &provider,
            None,
            RollCommand::DiceExpression {
                count: 1,
                sides: 100,
            },
            Duration::from_secs(1),
            |sides| {
                assert_eq!(sides, 100);
                73
            },
        )
        .await;
        assert_eq!(reply, "🎲 掷出了 73 / 100");
        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unsupported_dice_expression_never_calls_provider_or_roller() {
        let provider = MockProvider::replying(fortune_json());
        let requests = provider.requests.clone();
        let provider = Arc::new(provider) as DynLlmProvider;
        let reply = execute_roll_command_with_roller(
            &provider,
            Some("unused".to_owned()),
            RollCommand::UnsupportedDiceExpression,
            Duration::from_secs(1),
            |_| panic!("unsupported dice expression must not roll"),
        )
        .await;

        assert_eq!(reply, UNSUPPORTED_DICE_EXPRESSION_REPLY);
        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dm_plan_is_fixed_before_local_roll_and_request_contains_no_roll() {
        let provider = MockProvider::replying(fortune_json());
        let events = provider.events.clone();
        let requests = provider.requests.clone();
        let provider = Arc::new(provider) as DynLlmProvider;
        let roller_events = events.clone();
        let reply = execute_roll_command_with_roller(
            &provider,
            Some("mock:dm".to_owned()),
            RollCommand::DmCheck {
                query: "晚上要不要出门".to_owned(),
            },
            Duration::from_secs(1),
            move |sides| {
                assert_eq!(sides, 20);
                roller_events.lock().unwrap().push("rng");
                14
            },
        )
        .await;

        assert_eq!(*events.lock().unwrap(), ["model", "rng"]);
        assert!(reply.contains("难度：容易（DC 10）"));
        assert!(reply.contains("投掷：14"));
        assert!(reply.contains("✅ 成功"));
        assert!(reply.contains("今晚适合出门。"));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].metadata["purpose"], "roll_dm_check");
        let serialized = serde_json::to_string(&requests[0]).unwrap();
        assert!(!serialized.contains("\"roll\""));
        assert!(!serialized.contains("投掷：14"));
    }

    #[tokio::test]
    async fn provider_cannot_supply_roll_or_success_and_invalid_output_falls_back() {
        let reply_with_forbidden_fields = r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"出门","failure_meaning":"宅家","roll":20,"success":true}"#;
        let provider =
            Arc::new(MockProvider::replying(reply_with_forbidden_fields)) as DynLlmProvider;
        let reply = execute_roll_command_with_roller(
            &provider,
            None,
            RollCommand::DmCheck {
                query: "出门吗".to_owned(),
            },
            Duration::from_secs(1),
            |_| 7,
        )
        .await;
        assert_eq!(
            reply,
            "AI DM 暂时无法判断本次检定难度，本次仅进行普通 D20 投掷。\n\n🎲 掷出了 7 / 20"
        );
    }

    #[tokio::test]
    async fn provider_error_and_timeout_fall_back_to_local_d20() {
        let failed =
            Arc::new(MockProvider::failing(LlmError::provider("boom", "test"))) as DynLlmProvider;
        let failed_reply = execute_roll_command_with_roller(
            &failed,
            None,
            RollCommand::DmCheck {
                query: "出门吗".to_owned(),
            },
            Duration::from_secs(1),
            |_| 9,
        )
        .await;
        assert!(failed_reply.ends_with("🎲 掷出了 9 / 20"));

        let delayed =
            Arc::new(MockProvider::replying(fortune_json()).delayed(Duration::from_millis(50)))
                as DynLlmProvider;
        let timeout_reply = execute_roll_command_with_roller(
            &delayed,
            None,
            RollCommand::DmCheck {
                query: "出门吗".to_owned(),
            },
            Duration::from_millis(1),
            |_| 11,
        )
        .await;
        assert!(timeout_reply.ends_with("🎲 掷出了 11 / 20"));
    }

    #[tokio::test]
    async fn oversized_query_is_rejected_without_model_or_roll() {
        let provider = MockProvider::replying(fortune_json());
        let events = provider.events.clone();
        let provider = Arc::new(provider) as DynLlmProvider;
        let reply = execute_roll_command_with_roller(
            &provider,
            None,
            RollCommand::DmCheck {
                query: "问".repeat(DM_QUERY_MAX_CHARS + 1),
            },
            Duration::from_secs(1),
            |_| panic!("oversized query must not roll"),
        )
        .await;
        assert_eq!(
            reply,
            format!("判定问题过长，请控制在 {DM_QUERY_MAX_CHARS} 个字符以内。")
        );
        assert!(events.lock().unwrap().is_empty());
    }
}
