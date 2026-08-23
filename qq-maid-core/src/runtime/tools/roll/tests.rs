//! Roll domain 执行与命令兼容回归测试。

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
    r#"{"type":"fortune","check_name":"命运检定","difficulty":"medium","success_meaning":"今晚适合出门","failure_meaning":"今晚适合宅家"}"#
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
    assert!(matches!(
        parse_roll_command("/r 2d20 我能否说服守卫"),
        Some(RollCommand::DiceBatch {
            expression,
            repetitions: 1,
            reason: Some(reason),
        }) if expression == dice_expression("2d20") && reason == "我能否说服守卫"
    ));
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
        ("/roll 2d20+4", "2d20+4"),
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
    for input in ["/r测试", ".r测试"] {
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
    assert!(matches!(
        parse_roll_command("/r 2#d20"),
        Some(RollCommand::DiceBatch {
            expression,
            repetitions: 2,
            reason: None,
        }) if expression == dice_expression("d20")
    ));
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
async fn compact_reason_and_repeated_rolls_stay_local_and_keep_dice_icon() {
    let provider = Arc::new(MockProvider::failing(LlmError::provider(
        "must not call",
        "test",
    ))) as DynLlmProvider;
    let mut values = [2, 5].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        RollCommand::DiceBatch {
            expression: dice_expression("d6"),
            repetitions: 2,
            reason: Some("测试".to_owned()),
        },
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 6);
            values.next().expect("two independent rounds")
        },
    )
    .await;
    assert!(reply.reply.starts_with("🎲"));
    assert!(reply.reply.contains("第1轮：掷出了 2 / 6"));
    assert!(reply.reply.contains("第2轮：掷出了 5 / 6"));
    assert!(reply.reply.contains("“测试”"));
}

#[tokio::test]
async fn compact_penalty_alias_uses_valid_percentile_roll_template() {
    let provider = Arc::new(MockProvider::failing(LlmError::provider(
        "must not call",
        "test",
    ))) as DynLlmProvider;
    let command = parse_roll_command(".rap 测试").expect("compact penalty alias should parse");
    let mut values = [8, 10, 6].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        command,
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 10);
            values
                .next()
                .expect("penalty expression should roll three d10s")
        },
    )
    .await;

    assert_eq!(reply.reply, "🎲 “测试” p：D100=68（惩罚 6） = 68");
    assert!(reply.diagnostics()["roll_execution_kind"] == "local");
}

#[tokio::test]
async fn sealdice_default_advantage_expression_uses_roll_template_without_provider() {
    let provider = MockProvider::failing(LlmError::provider(
        "must not call for compact advantage",
        "test",
    ));
    let events = provider.events.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let command = parse_roll_command(".rd优势+6+1d4").expect("compact advantage should parse");
    let mut values = [20, 17, 4].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        command,
        Duration::from_secs(1),
        |sides| {
            assert!(matches!(sides, 20 | 4));
            values
                .next()
                .expect("advantage expression should roll three dice")
        },
    )
    .await;

    assert!(reply.reply.starts_with("🎲 2d20k1+6+1d4："));
    assert!(reply.reply.ends_with("= 30"));
    assert_eq!(reply.diagnostics()["roll_execution_kind"], "local");
    assert!(events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn slash_r_reason_is_local_but_roll_reason_keeps_explicit_ai_dm_path() {
    let provider = MockProvider::failing(LlmError::provider("must not call for /r reason", "test"));
    let events = provider.events.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let command = parse_roll_command("/r d50 开锁").expect("/r reason should parse locally");
    assert!(matches!(command, RollCommand::DiceBatch { .. }));
    let reply = execute_roll_command_with_roller(
        &provider,
        None,
        command,
        Duration::from_secs(1),
        |sides| {
            assert_eq!(sides, 50);
            17
        },
    )
    .await;
    assert!(reply.reply.starts_with("🎲"));
    assert!(reply.reply.contains("“开锁”"));
    assert_eq!(reply.diagnostics()["roll_execution_kind"], "local");
    assert!(events.lock().unwrap().is_empty());

    assert_eq!(
        parse_roll_command("/roll d50 开锁"),
        Some(RollCommand::DmCheck {
            expression: Some(dice_expression("d50")),
            query: "开锁".to_owned(),
        })
    );
}

#[tokio::test]
async fn named_roll_reply_uses_manual_display_name_argument() {
    let provider = Arc::new(MockProvider::failing(LlmError::provider(
        "must not call",
        "test",
    ))) as DynLlmProvider;
    let reply = execute_roll_command_with_display_name(
        &provider,
        None,
        RollCommand::DiceBatch {
            expression: dice_expression("p"),
            repetitions: 1,
            reason: Some("是惩罚骰".to_owned()),
        },
        Duration::from_secs(1),
        Some("人妻".to_owned()),
    )
    .await;
    assert!(reply.reply.starts_with("🎲 <人妻> 的“是惩罚骰”"));
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
async fn default_d20_entertainment_context_precedes_roll_and_request_contains_no_roll_result() {
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
    assert!(reply.reply.contains("难度：中等（DC 11）"));
    assert!(reply.reply.contains("投掷：14"));
    assert!(reply.reply.contains("✅ 成功"));
    assert!(reply.reply.contains("今晚适合出门。"));
    assert_eq!(reply.metrics.provider, "mock");
    assert_eq!(reply.metrics.model, "mock:dm");
    assert_eq!(reply.diagnostics()["roll_execution_kind"], "ai_dm_success");
    assert_eq!(reply.diagnostics()["dice_expression"], "1d20");
    assert_eq!(reply.diagnostics()["dice_minimum"], 1);
    assert_eq!(reply.diagnostics()["dice_maximum"], 20);
    assert_eq!(reply.diagnostics()["difficulty"], "medium");
    assert_eq!(reply.diagnostics()["computed_dc"], 11);
    assert_eq!(reply.diagnostics()["dc_strategy"], "entertainment_range");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].metadata["purpose"], "roll_dm_check");
    assert_eq!(requests[0].metadata["dice_expression"], "1d20");
    assert_eq!(requests[0].metadata["dice_minimum"], "1");
    assert_eq!(requests[0].metadata["dice_maximum"], "20");
    let user_context = &requests[0].messages.last().unwrap().content;
    assert!(user_context.contains("用户问题：晚上要不要出门"));
    assert!(user_context.contains("骰式：1d20"));
    assert!(user_context.contains("最小总值：1"));
    assert!(user_context.contains("最大总值：20"));
    let serialized = serde_json::to_string(&requests[0]).unwrap();
    assert!(!serialized.contains("\"roll\""));
    assert!(!serialized.contains("投掷：14"));
}

#[tokio::test]
async fn custom_2d20_plus_modifier_uses_core_entertainment_dc_before_roll() {
    let provider = MockProvider::replying(
        r#"{"type":"ability","check_name":"说服检定","difficulty":"easy","success_meaning":"守卫同意放行","failure_meaning":"守卫拒绝放行"}"#,
    );
    let requests = provider.requests.clone();
    let events = provider.events.clone();
    let provider = Arc::new(provider) as DynLlmProvider;
    let roller_events = events.clone();
    let mut values = [14, 18].into_iter();
    let reply = execute_roll_command_with_roller(
        &provider,
        Some("mock:dm".to_owned()),
        RollCommand::DmCheck {
            expression: Some(dice_expression("2d20+4")),
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
    assert!(reply.reply.contains("难度：容易（DC 20）"));
    assert!(reply.reply.contains("投掷：2d20+4：14 + 18 + 4 = 36"));
    assert!(reply.reply.contains("✅ 成功"));
    assert_eq!(reply.diagnostics()["dice_expression"], "2d20+4");
    assert_eq!(reply.diagnostics()["dice_minimum"], 6);
    assert_eq!(reply.diagnostics()["dice_maximum"], 44);
    assert_eq!(reply.diagnostics()["difficulty"], "easy");
    assert_eq!(reply.diagnostics()["computed_dc"], 20);
    assert_eq!(reply.diagnostics()["dc_strategy"], "entertainment_range");
    assert!(values.next().is_none());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let serialized = serde_json::to_string(&requests[0]).unwrap();
    assert!(serialized.contains("我能否说服守卫"));
    assert!(serialized.contains("2d20+4"));
    assert!(serialized.contains("最小总值：6"));
    assert!(serialized.contains("最大总值：44"));
    for actual_roll_value in ["14", "18", "36"] {
        assert!(
            !serialized.contains(actual_roll_value),
            "request leaked actual roll value {actual_roll_value}: {serialized}"
        );
    }
}

#[tokio::test]
async fn dm_receives_canonical_expression_and_range_for_each_supported_shape() {
    for (input, canonical, minimum, maximum, dc) in [
        ("2d20", "2d20", 2, 40, 21),
        ("2d20+4", "2d20+4", 6, 44, 25),
        ("d100", "1d100", 1, 100, 51),
        ("1d20+3", "1d20+3", 4, 23, 14),
        ("1d8+1d6+4", "1d8+1d6+4", 6, 18, 12),
    ] {
        let provider = MockProvider::replying(
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"medium","success_meaning":"成功","failure_meaning":"失败"}"#,
        );
        let requests = provider.requests.clone();
        let provider = Arc::new(provider) as DynLlmProvider;
        let result = execute_roll_command_with_roller(
            &provider,
            None,
            RollCommand::DmCheck {
                expression: Some(dice_expression(input)),
                query: "测试范围".to_owned(),
            },
            Duration::from_secs(1),
            |sides| sides,
        )
        .await;

        assert_eq!(result.diagnostics()["roll_execution_kind"], "ai_dm_success");
        assert!(result.reply.contains(&format!("DC {dc}")), "{input}");
        let requests = requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.metadata["dice_expression"], canonical, "{input}");
        assert_eq!(
            request.metadata["dice_minimum"],
            minimum.to_string(),
            "{input}"
        );
        assert_eq!(
            request.metadata["dice_maximum"],
            maximum.to_string(),
            "{input}"
        );
        let context = &request.messages.last().unwrap().content;
        assert!(context.contains(&format!("骰式：{canonical}")), "{input}");
        assert!(context.contains(&format!("最小总值：{minimum}")), "{input}");
        assert!(context.contains(&format!("最大总值：{maximum}")), "{input}");
    }
}

#[tokio::test]
async fn invalid_or_result_bearing_ai_outputs_use_explicit_fallback() {
    for invalid_reply in [
        r#"{"type":"fortune","check_name":"命运检定","success_meaning":"出门","failure_meaning":"宅家"}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":"10","success_meaning":"出门","failure_meaning":"宅家"}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":2147483648,"success_meaning":"出门","failure_meaning":"宅家"}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":1,"success_meaning":"出门","failure_meaning":"宅家"}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"出门","failure_meaning":"宅家","roll":20}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"出门","failure_meaning":"宅家","total":20}"#,
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"出门","failure_meaning":"宅家","success":true}"#,
    ] {
        let provider = Arc::new(MockProvider::replying(invalid_reply)) as DynLlmProvider;
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
