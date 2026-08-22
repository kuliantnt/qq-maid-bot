//! 骰子 Slash 命令领域门面。
//!
//! 无参数命令保持本地 D20 快速路径，通用骰子表达式由 Core 本地结算；带问题的命令
//! 先让模型生成并校验判定方案，再本地生成骰值和结算结果。命令分派层只负责路由与响应投影。

use std::time::{Duration, Instant};

use qq_maid_llm::provider::{DynLlmProvider, types::TokenUsage};
use serde_json::{Value, json};

use crate::{runtime::command::parse_slash_command, util::metrics::LlmMetrics};

mod dice;
mod dm;
mod outcome;

use dice::{DiceExpression, DiceExpressionParse, RollResult, Roller};
use dm::prepare_dm_check;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RollCommand {
    Default,
    DiceExpression {
        expression: DiceExpression,
    },
    DmCheck {
        expression: Option<DiceExpression>,
        query: String,
    },
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
        diagnostics
    }
}

/// `/roll`（以及 `/r`）支持无参数本地 D20、通用骰子表达式，以及一个非空自然语言问题。
pub(crate) fn parse_roll_command(text: &str) -> Option<RollCommand> {
    let command = parse_slash_command(text)?;
    if command.action != "roll" {
        return None;
    }
    if command.argument.is_empty() {
        Some(RollCommand::Default)
    } else {
        if let Some((expression, query)) = dice::parse_expression_prefix(&command.argument) {
            return Some(RollCommand::DmCheck {
                expression: Some(expression),
                query: query.to_owned(),
            });
        }
        match dice::parse_expression(&command.argument) {
            DiceExpressionParse::Parsed(expression) => {
                Some(RollCommand::DiceExpression { expression })
            }
            DiceExpressionParse::Invalid(_) => Some(RollCommand::InvalidDiceExpression),
            DiceExpressionParse::NotDiceExpression => Some(RollCommand::DmCheck {
                expression: None,
                query: command.argument,
            }),
        }
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
    request_budget: Duration,
) -> RollExecutionResult {
    execute_roll_command_with_roller_factory(
        provider,
        model,
        command,
        dm_timeout_for_request(request_budget),
        thread_roller,
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
    execute_roll_command_with_roller_factory(provider, model, command, dm_timeout, || roller).await
}

async fn execute_roll_command_with_roller_factory<RF, F>(
    provider: &DynLlmProvider,
    model: Option<String>,
    command: RollCommand,
    dm_timeout: Duration,
    roller_factory: RF,
) -> RollExecutionResult
where
    RF: FnOnce() -> F,
    F: Roller,
{
    let (query, expression) = match command {
        RollCommand::Default => {
            let mut roller = roller_factory();
            let (result, _) = roll_expression_result(None, &mut roller);
            return local_result(roll_dice_expression_reply(&result));
        }
        RollCommand::DiceExpression { expression } => {
            let mut roller = roller_factory();
            let (result, _) = roll_expression_result(Some(expression), &mut roller);
            return local_result(roll_dice_expression_reply(&result));
        }
        RollCommand::InvalidDiceExpression => {
            return local_result(INVALID_DICE_EXPRESSION_REPLY.to_owned());
        }
        RollCommand::DmCheck { expression, query } => (query, expression),
    };

    let query_chars = query.chars().count();
    if query_chars > DM_QUERY_MAX_CHARS {
        return local_result(format!(
            "判定问题过长，请控制在 {DM_QUERY_MAX_CHARS} 个字符以内。"
        ));
    }

    let started_at = Instant::now();
    let prepared = tokio::time::timeout(
        dm_timeout,
        prepare_dm_check(provider, model.clone(), query.as_str()),
    )
    .await;
    match prepared {
        Ok(Ok(prepared)) => {
            // 此处是本轮第一次生成实际骰值；上面的模型 future 已完成且方案已通过校验。
            let mut roller = roller_factory();
            let (roll, _) = roll_expression_result(expression, &mut roller);
            RollExecutionResult {
                reply: render_dm_result(&prepared.plan, &roll),
                metrics: prepared.metrics,
                usage: prepared.usage,
                execution_kind: RollExecutionKind::AiDmSuccess,
                provider_fallback_used: Some(prepared.provider_fallback_used),
                fallback_reason: None,
            }
        }
        Ok(Err(failure)) => {
            tracing::warn!(
                error_code = failure.error.code,
                error_stage = failure.error.stage,
                query_chars,
                "AI DM 判定方案生成失败，降级为普通 D20"
            );
            let mut roller = roller_factory();
            let metrics = failure.metrics.unwrap_or_else(|| {
                requested_model_metrics(provider, model.as_deref(), started_at.elapsed())
            });
            let (roll, requested_expression) = roll_expression_result(expression, &mut roller);
            RollExecutionResult {
                reply: roll_fallback_reply_from_result(&roll, requested_expression),
                metrics,
                usage: failure.usage,
                execution_kind: RollExecutionKind::AiDmFallback,
                provider_fallback_used: failure.provider_fallback_used,
                fallback_reason: Some(RollFallbackReason {
                    code: failure.error.code.to_owned(),
                    stage: failure.error.stage.to_owned(),
                }),
            }
        }
        Err(_) => {
            tracing::warn!(
                error_code = "timeout",
                error_stage = "roll_dm",
                query_chars,
                "AI DM 判定方案生成超时，降级为普通 D20"
            );
            let mut roller = roller_factory();
            let (roll, requested_expression) = roll_expression_result(expression, &mut roller);
            RollExecutionResult {
                reply: roll_fallback_reply_from_result(&roll, requested_expression),
                metrics: requested_model_metrics(provider, model.as_deref(), started_at.elapsed()),
                usage: None,
                execution_kind: RollExecutionKind::AiDmFallback,
                provider_fallback_used: None,
                fallback_reason: Some(RollFallbackReason {
                    code: "timeout".to_owned(),
                    stage: "roll_dm".to_owned(),
                }),
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

fn roll_dice_expression_reply(result: &RollResult) -> String {
    if result.expression.is_single_unmodified() {
        let roll = result.rolls.first().expect("单骰表达式必须产生一个骰值");
        return format!("🎲 掷出了 {} / {}", roll.value, roll.sides);
    }

    let calculation = result.calculation();
    format!("🎲 {}：{calculation} = {}", result.expression, result.total)
}

fn roll_fallback_reply_from_result(result: &RollResult, requested_expression: bool) -> String {
    let prefix = if requested_expression {
        DM_EXPRESSION_FALLBACK_PREFIX
    } else {
        DM_FALLBACK_PREFIX
    };
    format!("{prefix}\n\n{}", roll_dice_expression_reply(result))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::{error::LlmError, util::metrics::LlmMetrics};
    use async_trait::async_trait;
    use qq_maid_llm::provider::{
        ChatOutcome, LlmProvider,
        types::{ChatRequest, TokenUsage},
    };

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

    fn dice_expression(input: &str) -> DiceExpression {
        match dice::parse_expression(input) {
            DiceExpressionParse::Parsed(expression) => expression,
            other => panic!("expected dice expression {input}, got {other:?}"),
        }
    }

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
        assert_eq!(
            parse_roll_command("/r 2d20 我能否说服守卫"),
            Some(RollCommand::DmCheck {
                expression: Some(dice_expression("2d20")),
                query: "我能否说服守卫".to_owned(),
            })
        );
        assert_eq!(
            parse_roll_command("/roll 1d20 + 3 能否通过"),
            Some(RollCommand::DmCheck {
                expression: Some(dice_expression("1d20+3")),
                query: "能否通过".to_owned(),
            })
        );
        for (input, expression) in [
            ("/roll d20", "d20"),
            ("/roll d100", "d100"),
            ("/roll 2d6", "2d6"),
            ("/roll 1D100", "1D100"),
            ("/roll 1d20+3", "1d20+3"),
            ("/roll 2d6 + 1", "2d6 + 1"),
            ("/roll 1d8+1d6+4", "1d8+1d6+4"),
        ] {
            assert!(matches!(
                parse_roll_command(input),
                Some(RollCommand::DiceExpression { expression: parsed })
                    if parsed == dice_expression(expression)
            ));
        }
        for input in [
            "/roll 101d6",
            "/roll d101",
            "/roll 0d6",
            "/roll d0",
            "/roll 1d20-1d6",
            "/roll 1d20+1001",
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
                expression: None,
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
        assert!(reply.reply.contains("难度：容易（DC 10）"));
        assert!(reply.reply.contains("投掷：14"));
        assert!(reply.reply.contains("✅ 成功"));
        assert!(reply.reply.contains("今晚适合出门。"));
        assert_eq!(reply.metrics.provider, "mock");
        assert_eq!(reply.metrics.model, "mock:dm");
        assert_eq!(reply.diagnostics()["roll_execution_kind"], "ai_dm_success");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].metadata["purpose"], "roll_dm_check");
        let serialized = serde_json::to_string(&requests[0]).unwrap();
        assert!(!serialized.contains("\"roll\""));
        assert!(!serialized.contains("投掷：14"));
    }

    #[tokio::test]
    async fn dm_check_can_use_a_user_supplied_expression_and_only_sends_question_to_model() {
        let provider = MockProvider::replying(fortune_json());
        let requests = provider.requests.clone();
        let events = provider.events.clone();
        let provider = Arc::new(provider) as DynLlmProvider;
        let roller_events = events.clone();
        let mut values = [14, 18].into_iter();
        let reply = execute_roll_command_with_roller(
            &provider,
            Some("mock:dm".to_owned()),
            RollCommand::DmCheck {
                expression: Some(dice_expression("2d20")),
                query: "我能否说服守卫".to_owned(),
            },
            Duration::from_secs(1),
            |sides| {
                assert_eq!(sides, 20);
                roller_events.lock().unwrap().push("rng");
                values.next().expect("2d20 should roll exactly twice")
            },
        )
        .await;

        assert_eq!(*events.lock().unwrap(), ["model", "rng", "rng"]);
        assert!(reply.reply.contains("投掷：2d20：14 + 18 = 32"));
        assert!(reply.reply.contains("✅ 成功"));
        assert!(values.next().is_none());
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            serde_json::to_string(&requests[0])
                .unwrap()
                .contains("我能否说服守卫")
        );
        assert!(
            !serde_json::to_string(&requests[0])
                .unwrap()
                .contains("2d20")
        );
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
                expression: None,
                query: "出门吗".to_owned(),
            },
            Duration::from_secs(1),
            |_| 7,
        )
        .await;
        assert_eq!(
            reply.reply,
            "AI DM 暂时无法判断本次检定难度，本次仅进行普通 D20 投掷。\n\n🎲 掷出了 7 / 20"
        );
        assert_eq!(
            reply.diagnostics()["roll_fallback_reason"],
            "roll_dm_invalid_output"
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
                expression: None,
                query: "出门吗".to_owned(),
            },
            Duration::from_secs(1),
            |_| 9,
        )
        .await;
        assert!(failed_reply.reply.ends_with("🎲 掷出了 9 / 20"));
        assert_eq!(
            failed_reply.diagnostics()["roll_execution_kind"],
            "ai_dm_fallback"
        );
        assert_eq!(
            failed_reply.diagnostics()["roll_fallback_reason"],
            "provider_error"
        );

        let delayed =
            Arc::new(MockProvider::replying(fortune_json()).delayed(Duration::from_millis(50)))
                as DynLlmProvider;
        let timeout_reply = execute_roll_command_with_roller(
            &delayed,
            None,
            RollCommand::DmCheck {
                expression: None,
                query: "出门吗".to_owned(),
            },
            Duration::from_millis(1),
            |_| 11,
        )
        .await;
        assert!(timeout_reply.reply.ends_with("🎲 掷出了 11 / 20"));
        assert_eq!(
            timeout_reply.diagnostics()["roll_fallback_reason"],
            "timeout"
        );
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
                expression: None,
                query: "问".repeat(DM_QUERY_MAX_CHARS + 1),
            },
            Duration::from_secs(1),
            |_| panic!("oversized query must not roll"),
        )
        .await;
        assert_eq!(
            reply.reply,
            format!("判定问题过长，请控制在 {DM_QUERY_MAX_CHARS} 个字符以内。")
        );
        assert!(events.lock().unwrap().is_empty());
    }
}
