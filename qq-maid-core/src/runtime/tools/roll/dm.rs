//! Entertainment DM 判定方案生成与本地校验。

use std::collections::HashMap;

use qq_maid_llm::provider::{
    DynLlmProvider,
    types::{ChatMessage, ChatRequest, ReasoningEffort, TokenUsage},
};
use serde::Deserialize;

use crate::{error::LlmError, util::metrics::LlmMetrics};

use super::dice::DiceExpression;

const DM_SYSTEM_PROMPT: &str = r#"你是 Entertainment DM（娱乐判定 DM），只负责为用户问题制定一次娱乐性骰子判定方案。
当前只处理日常二选一、运气判断和无人物卡的轻量行动检定；不要识别、猜测或选择正式规则系统。
骰式由用户命令或 Core 的娱乐规则决定；你看不到、不能决定也不得猜测实际骰值；不要自行掷骰，不要声称行动已经成功或失败。
日常二选一、运气和娱乐选择优先使用 fortune。
当问题没有明显有利或不利倾向时，fortune 默认选择 medium，使娱乐判定保持接近五五开。
存在明显正向倾向时可以降低难度；存在明显负向倾向时可以提高难度。
不要因为事情日常、简单或常见就机械选择 easy。
潜行、说服、观察等实际行动使用 ability，并根据行动本身的实际难度选择 difficulty，不要套用 fortune 的默认规则。
check_name 是简短的完整检定名称。
不使用角色卡、属性值、熟练、装备或任何加值。difficulty 只能取允许的枚举；你只选择 difficulty，不提供 dc。
实际 DC 由 Core 根据当前骰式（包括默认 1d20）的理论范围和固定的娱乐模式难度刻度计算；当前不是 DND5E 或正式 TRPG 规则。不要输出 dc 字段。
只输出一个 JSON 对象，不要 Markdown、解释或额外字段：
{"type":"ability|fortune","check_name":"...","difficulty":"very_easy|easy|medium|hard|very_hard|nearly_impossible","success_meaning":"...","failure_meaning":"..."}"#;

const CHECK_NAME_MAX_CHARS: usize = 40;
const MEANING_MAX_CHARS: usize = 120;
const DM_MAX_OUTPUT_TOKENS: u64 = 256;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum CheckType {
    Ability,
    Fortune,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Difficulty {
    VeryEasy,
    Easy,
    Medium,
    Hard,
    VeryHard,
    NearlyImpossible,
}

impl Difficulty {
    /// 所有骰式共用的娱乐模式区间位置，使用有理数避免浮点取整漂移。
    const fn entertainment_position(self) -> (i32, i32) {
        match self {
            Self::VeryEasy => (1, 5),
            Self::Easy => (7, 20),
            Self::Medium => (1, 2),
            Self::Hard => (13, 20),
            Self::VeryHard => (4, 5),
            Self::NearlyImpossible => (19, 20),
        }
    }

    pub(super) const fn key(self) -> &'static str {
        match self {
            Self::VeryEasy => "very_easy",
            Self::Easy => "easy",
            Self::Medium => "medium",
            Self::Hard => "hard",
            Self::VeryHard => "very_hard",
            Self::NearlyImpossible => "nearly_impossible",
        }
    }

    pub(super) const fn display_name(self) -> &'static str {
        match self {
            Self::VeryEasy => "很容易",
            Self::Easy => "容易",
            Self::Medium => "中等",
            Self::Hard => "困难",
            Self::VeryHard => "很困难",
            Self::NearlyImpossible => "近乎不可能",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DcStrategy {
    EntertainmentRange,
}

impl DcStrategy {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::EntertainmentRange => "entertainment_range",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ComputedDc {
    pub(super) value: i32,
    pub(super) strategy: DcStrategy,
}

/// 根据骰式理论范围和娱乐难度刻度计算 Core 实际使用的 DC。
///
/// 所有表达式都按区间位置计算，并统一向上取整，避免浮点误差或截断导致难度意外降低。
/// 宽度为零时不存在区间内部阈值，公式结果为唯一可表示的理论范围端点。
pub(super) fn compute_dc(expression: &DiceExpression, difficulty: Difficulty) -> ComputedDc {
    let (minimum, maximum) = expression.total_range();
    let width = i64::from(maximum) - i64::from(minimum);
    let (numerator, denominator) = difficulty.entertainment_position();
    let offset =
        (width * i64::from(numerator) + i64::from(denominator) - 1) / i64::from(denominator);
    let value = i64::from(minimum) + offset;
    debug_assert!(value >= i64::from(i32::MIN) && value <= i64::from(i32::MAX));
    ComputedDc {
        value: value as i32,
        strategy: DcStrategy::EntertainmentRange,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDmCheckPlan {
    #[serde(rename = "type")]
    check_type: CheckType,
    check_name: String,
    difficulty: Difficulty,
    success_meaning: String,
    failure_meaning: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DmCheckPlan {
    pub(super) check_type: CheckType,
    pub(super) check_name: String,
    pub(super) difficulty: Difficulty,
    pub(super) dc: i32,
    pub(super) success_meaning: String,
    pub(super) failure_meaning: String,
}

pub(super) struct PreparedDmCheck {
    pub(super) plan: DmCheckPlan,
    pub(super) metrics: LlmMetrics,
    pub(super) usage: Option<TokenUsage>,
    pub(super) provider_fallback_used: bool,
}

pub(super) struct DmCheckFailure {
    pub(super) error: LlmError,
    /// Provider 已返回但结构化结果无效时仍保留真实调用指标；传输层失败时为空。
    pub(super) metrics: Option<LlmMetrics>,
    pub(super) usage: Option<TokenUsage>,
    pub(super) provider_fallback_used: Option<bool>,
}

pub(super) async fn prepare_dm_check(
    provider: &DynLlmProvider,
    model: Option<String>,
    query: &str,
    expression: &DiceExpression,
) -> Result<PreparedDmCheck, Box<DmCheckFailure>> {
    let (minimum, maximum) = expression.total_range();
    let dm_context =
        format!("用户问题：{query}\n骰式：{expression}\n最小总值：{minimum}\n最大总值：{maximum}");
    let request = ChatRequest {
        // 这是 provider 关联键，不对应也不会创建 conversation session。
        session_id: "roll-dm".to_owned(),
        model,
        messages: vec![
            ChatMessage::system(DM_SYSTEM_PROMPT),
            ChatMessage::user(dm_context),
        ],
        context_budget: None,
        max_output_tokens: Some(DM_MAX_OUTPUT_TOKENS),
        reasoning_effort: Some(ReasoningEffort::Low),
        metadata: HashMap::from([
            ("purpose".to_owned(), "roll_dm_check".to_owned()),
            ("query_chars".to_owned(), query.chars().count().to_string()),
            ("dice_expression".to_owned(), expression.to_string()),
            ("dice_minimum".to_owned(), minimum.to_string()),
            ("dice_maximum".to_owned(), maximum.to_string()),
        ]),
    };
    let outcome = provider.chat(request).await.map_err(|error| {
        Box::new(DmCheckFailure {
            error,
            metrics: None,
            usage: None,
            provider_fallback_used: None,
        })
    })?;
    let plan = parse_dm_check_plan(&outcome.reply, expression).map_err(|error| {
        Box::new(DmCheckFailure {
            error,
            metrics: Some(outcome.metrics.clone()),
            usage: outcome.usage.clone(),
            provider_fallback_used: Some(outcome.fallback_used),
        })
    })?;
    Ok(PreparedDmCheck {
        plan,
        metrics: outcome.metrics,
        usage: outcome.usage,
        provider_fallback_used: outcome.fallback_used,
    })
}

fn parse_dm_check_plan(raw: &str, expression: &DiceExpression) -> Result<DmCheckPlan, LlmError> {
    let raw = raw.trim();
    // 部分模型即使被要求只返回 JSON，仍会把完整对象包在 Markdown 代码块中。
    // 这里只兼容单个完整外层代码块，不从解释文本中截取对象，避免放宽结构化输出边界。
    let json = strip_outer_json_fence(raw).unwrap_or(raw);
    let raw: RawDmCheckPlan = serde_json::from_str(json).map_err(|_| {
        LlmError::new(
            "roll_dm_invalid_output",
            "AI DM returned invalid structured output",
            "roll_dm",
        )
    })?;
    let computed_dc = compute_dc(expression, raw.difficulty);
    Ok(DmCheckPlan {
        check_type: raw.check_type,
        check_name: validate_text_field(raw.check_name, "check_name", CHECK_NAME_MAX_CHARS)?,
        difficulty: raw.difficulty,
        dc: computed_dc.value,
        success_meaning: validate_text_field(
            raw.success_meaning,
            "success_meaning",
            MEANING_MAX_CHARS,
        )?,
        failure_meaning: validate_text_field(
            raw.failure_meaning,
            "failure_meaning",
            MEANING_MAX_CHARS,
        )?,
    })
}

fn strip_outer_json_fence(text: &str) -> Option<&str> {
    let body = text.strip_prefix("```")?.strip_suffix("```")?;
    let (language, json) = body.split_once('\n')?;
    let language = language.trim();
    (language.is_empty() || language.eq_ignore_ascii_case("json")).then(|| json.trim())
}

fn validate_text_field(
    value: String,
    field: &'static str,
    max_chars: usize,
) -> Result<String, LlmError> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(|ch| ch.is_control())
    {
        return Err(LlmError::new(
            "roll_dm_invalid_output",
            format!("AI DM returned invalid `{field}`"),
            "roll_dm",
        ));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_is_scoped_to_entertainment_checks() {
        assert!(DM_SYSTEM_PROMPT.contains("Entertainment DM"));
        assert!(!DM_SYSTEM_PROMPT.contains("轻量跑团 DM"));
        assert!(DM_SYSTEM_PROMPT.contains("无人物卡的轻量行动检定"));
        assert!(!DM_SYSTEM_PROMPT.contains("通常选择 easy"));
        assert!(DM_SYSTEM_PROMPT.contains("fortune 默认选择 medium"));
        assert!(DM_SYSTEM_PROMPT.contains("明显正向倾向时可以降低难度"));
        assert!(DM_SYSTEM_PROMPT.contains("明显负向倾向时可以提高难度"));
        assert!(DM_SYSTEM_PROMPT.contains("根据行动本身的实际难度选择 difficulty"));
        assert!(DM_SYSTEM_PROMPT.contains("不要套用 fortune 的默认规则"));
    }

    fn expression(input: &str) -> DiceExpression {
        match super::super::dice::parse_expression(input) {
            super::super::dice::DiceExpressionParse::Parsed(expression) => expression,
            other => panic!("expected dice expression {input}, got {other:?}"),
        }
    }

    #[test]
    fn parses_difficulty_only_schema_and_computes_dc_in_core() {
        let plan = parse_dm_check_plan(
            r#"{"type":"ability","check_name":"说服检定","difficulty":"easy","success_meaning":"成功说服守卫","failure_meaning":"守卫拒绝放行"}"#,
            &expression("2d20+4"),
        )
        .unwrap();
        assert_eq!(plan.check_type, CheckType::Ability);
        assert_eq!(plan.difficulty, Difficulty::Easy);
        assert_eq!(plan.dc, 20);
    }

    #[test]
    fn accepts_a_single_outer_json_code_fence() {
        for raw in [
            "```json\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"success_meaning\":\"喝咖啡\",\"failure_meaning\":\"不喝咖啡\"}\n```",
            "```JSON\r\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"success_meaning\":\"喝咖啡\",\"failure_meaning\":\"不喝咖啡\"}\r\n```",
            "```\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"success_meaning\":\"喝咖啡\",\"failure_meaning\":\"不喝咖啡\"}\n```",
        ] {
            let plan = parse_dm_check_plan(raw, &DiceExpression::default_d20()).unwrap();
            assert_eq!(plan.check_type, CheckType::Fortune);
            assert_eq!(plan.difficulty, Difficulty::Easy);
            assert_eq!(plan.dc, 8);
        }
    }

    #[test]
    fn rejects_json_wrapped_in_explanation_or_an_unrelated_code_fence() {
        for raw in [
            "判定方案：\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"dc\":10,\"success_meaning\":\"成功\",\"failure_meaning\":\"失败\"}",
            "```text\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"dc\":10,\"success_meaning\":\"成功\",\"failure_meaning\":\"失败\"}\n```",
        ] {
            let error = parse_dm_check_plan(raw, &DiceExpression::default_d20()).unwrap_err();
            assert_eq!(error.code, "roll_dm_invalid_output");
        }
    }

    #[test]
    fn default_d20_uses_entertainment_range_difficulty_positions() {
        let cases = [
            (Difficulty::VeryEasy, 5),
            (Difficulty::Easy, 8),
            (Difficulty::Medium, 11),
            (Difficulty::Hard, 14),
            (Difficulty::VeryHard, 17),
            (Difficulty::NearlyImpossible, 20),
        ];
        for (difficulty, dc) in cases {
            let computed = compute_dc(&DiceExpression::default_d20(), difficulty);
            assert_eq!(computed.value, dc);
            assert_eq!(computed.strategy, DcStrategy::EntertainmentRange);
        }
    }

    #[test]
    fn custom_expression_dc_uses_monotonic_entertainment_range_positions() {
        let difficulties = [
            Difficulty::VeryEasy,
            Difficulty::Easy,
            Difficulty::Medium,
            Difficulty::Hard,
            Difficulty::VeryHard,
            Difficulty::NearlyImpossible,
        ];
        for input in ["2d20", "2d20+4", "d100", "1d20+3", "1d8+1d6+4"] {
            let expression = expression(input);
            let (minimum, maximum) = expression.total_range();
            let values = difficulties
                .iter()
                .map(|difficulty| {
                    let computed = compute_dc(&expression, *difficulty);
                    assert_eq!(computed.strategy, DcStrategy::EntertainmentRange, "{input}");
                    assert!(
                        computed.value >= minimum && computed.value <= maximum,
                        "DC {} outside {minimum}..={maximum} for {input}",
                        computed.value
                    );
                    computed.value
                })
                .collect::<Vec<_>>();
            assert!(
                values.windows(2).all(|window| window[0] <= window[1]),
                "{input}"
            );
        }
    }

    #[test]
    fn custom_expression_dc_rounds_up_without_lowering_difficulty() {
        let expression = expression("2d20+4");
        let computed = compute_dc(&expression, Difficulty::Easy);
        assert_eq!(computed.value, 20);
        assert_eq!(computed.strategy, DcStrategy::EntertainmentRange);
    }

    #[test]
    fn rejects_invalid_json_unknown_type_and_unknown_difficulty() {
        for raw in [
            "not-json",
            r#"{"type":"skill","check_name":"检定","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"dc17","success_meaning":"成功","failure_meaning":"失败"}"#,
        ] {
            let error = parse_dm_check_plan(raw, &DiceExpression::default_d20()).unwrap_err();
            assert_eq!(error.code, "roll_dm_invalid_output");
        }
    }

    #[test]
    fn rejects_missing_invalid_and_forbidden_result_fields() {
        for raw in [
            r#"{"type":"fortune","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":" ","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"成功","failure_meaning":""}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败","dc":10}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败","roll":10}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败","total":10}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败","success":true}"#,
        ] {
            let error = parse_dm_check_plan(raw, &DiceExpression::default_d20()).unwrap_err();
            assert_eq!(error.code, "roll_dm_invalid_output");
        }
    }
}
