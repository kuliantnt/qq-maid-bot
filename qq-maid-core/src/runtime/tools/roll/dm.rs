//! AI DM 判定方案生成与本地校验。

use std::collections::HashMap;

use qq_maid_llm::provider::{
    DynLlmProvider,
    types::{ChatMessage, ChatRequest, ReasoningEffort, TokenUsage},
};
use serde::Deserialize;

use crate::{error::LlmError, util::metrics::LlmMetrics};

use super::dice::DiceExpression;

const DM_SYSTEM_PROMPT: &str = r#"你是轻量跑团 DM，只负责为用户问题制定一次骰子判定方案。
骰式由用户命令或 Core 规则引擎决定；你看不到、不能决定也不得猜测实际骰值；不要自行掷骰，不要声称行动已经成功或失败。
日常二选一、运气和娱乐选择优先使用 fortune，通常选择 easy；只有问题明确很难时才提高难度。
潜行、说服、观察等实际行动使用 ability。check_name 是简短的完整检定名称。
不使用角色卡、属性值、熟练、装备或任何加值。difficulty 只能取允许的枚举，dc 是 Core 最终使用的实际整数阈值。
通常必须选择“最小总值 < dc <= 最大总值”，避免必成功或必失败。默认 1d20 沿用常用 DC：very_easy=5、easy=10、medium=15、hard=20、very_hard=25、nearly_impossible=30；后两档由 Core 的 Natural 20 规则保留成功机会。
只输出一个 JSON 对象，不要 Markdown、解释或额外字段：
{"type":"ability|fortune","check_name":"...","difficulty":"very_easy|easy|medium|hard|very_hard|nearly_impossible","dc":15,"success_meaning":"...","failure_meaning":"..."}"#;

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
    /// 默认 D20 的历史难度习惯；通用骰式不得直接套用这个映射。
    const fn conventional_d20_dc(self) -> i32 {
        match self {
            Self::VeryEasy => 5,
            Self::Easy => 10,
            Self::Medium => 15,
            Self::Hard => 20,
            Self::VeryHard => 25,
            Self::NearlyImpossible => 30,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDmCheckPlan {
    #[serde(rename = "type")]
    check_type: CheckType,
    check_name: String,
    difficulty: Difficulty,
    dc: i32,
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
    validate_dc(raw.dc, raw.difficulty, expression)?;
    Ok(DmCheckPlan {
        check_type: raw.check_type,
        check_name: validate_text_field(raw.check_name, "check_name", CHECK_NAME_MAX_CHARS)?,
        difficulty: raw.difficulty,
        dc: raw.dc,
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

fn validate_dc(
    dc: i32,
    difficulty: Difficulty,
    expression: &DiceExpression,
) -> Result<(), LlmError> {
    // 默认无修正 D20 是历史兼容路径，difficulty 与 DC 必须一一对应；very_hard=25
    // 和 nearly_impossible=30 虽超出理论最大值，仍由 Natural 20 特殊规则保留成功机会。
    let is_valid = if expression.is_default_d20() {
        dc == difficulty.conventional_d20_dc()
    } else {
        // 自定义骰式不套用 D20 固定表，只接受有实际成功机会且不会必定成功的阈值。
        let (minimum, maximum) = expression.total_range();
        dc > minimum && dc <= maximum
    };
    if is_valid {
        return Ok(());
    }
    Err(LlmError::new(
        "roll_dm_invalid_output",
        "AI DM returned invalid `dc` for dice range",
        "roll_dm",
    ))
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

    fn expression(input: &str) -> DiceExpression {
        match super::super::dice::parse_expression(input) {
            super::super::dice::DiceExpressionParse::Parsed(expression) => expression,
            other => panic!("expected dice expression {input}, got {other:?}"),
        }
    }

    #[test]
    fn parses_allowed_schema_with_actual_dc() {
        let plan = parse_dm_check_plan(
            r#"{"type":"ability","check_name":"说服检定","difficulty":"hard","dc":26,"success_meaning":"成功说服守卫","failure_meaning":"守卫拒绝放行"}"#,
            &expression("2d20"),
        )
        .unwrap();
        assert_eq!(plan.check_type, CheckType::Ability);
        assert_eq!(plan.difficulty.display_name(), "困难");
        assert_eq!(plan.dc, 26);
    }

    #[test]
    fn accepts_a_single_outer_json_code_fence() {
        for raw in [
            "```json\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"dc\":10,\"success_meaning\":\"喝咖啡\",\"failure_meaning\":\"不喝咖啡\"}\n```",
            "```JSON\r\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"dc\":10,\"success_meaning\":\"喝咖啡\",\"failure_meaning\":\"不喝咖啡\"}\r\n```",
            "```\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"dc\":10,\"success_meaning\":\"喝咖啡\",\"failure_meaning\":\"不喝咖啡\"}\n```",
        ] {
            let plan = parse_dm_check_plan(raw, &DiceExpression::default_d20()).unwrap();
            assert_eq!(plan.check_type, CheckType::Fortune);
            assert_eq!(plan.difficulty, Difficulty::Easy);
            assert_eq!(plan.dc, 10);
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
    fn default_d20_requires_the_conventional_difficulty_dc_pair() {
        let cases = [
            (Difficulty::VeryEasy, 5),
            (Difficulty::Easy, 10),
            (Difficulty::Medium, 15),
            (Difficulty::Hard, 20),
            (Difficulty::VeryHard, 25),
            (Difficulty::NearlyImpossible, 30),
        ];
        for (difficulty, dc) in cases {
            validate_dc(dc, difficulty, &DiceExpression::default_d20()).unwrap();
        }
        for (difficulty, dc) in [
            (Difficulty::Easy, 15),
            (Difficulty::Hard, 10),
            (Difficulty::Medium, 20),
            (Difficulty::NearlyImpossible, 29),
        ] {
            assert!(
                validate_dc(dc, difficulty, &DiceExpression::default_d20()).is_err(),
                "mismatched default D20 pair should be rejected: {difficulty:?} + DC {dc}"
            );
        }
    }

    #[test]
    fn custom_expression_dc_must_be_a_meaningful_reachable_threshold() {
        let custom = expression("2d20");
        validate_dc(26, Difficulty::Hard, &custom).unwrap();
        validate_dc(55, Difficulty::Medium, &expression("d100")).unwrap();
        for invalid in [i32::MIN, 2, 41, i32::MAX] {
            let error = validate_dc(invalid, Difficulty::NearlyImpossible, &custom).unwrap_err();
            assert_eq!(error.code, "roll_dm_invalid_output");
        }
    }

    #[test]
    fn rejects_invalid_json_unknown_type_and_unknown_difficulty() {
        for raw in [
            "not-json",
            r#"{"type":"skill","check_name":"检定","difficulty":"easy","dc":10,"success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"dc17","dc":17,"success_meaning":"成功","failure_meaning":"失败"}"#,
        ] {
            let error = parse_dm_check_plan(raw, &DiceExpression::default_d20()).unwrap_err();
            assert_eq!(error.code, "roll_dm_invalid_output");
        }
    }

    #[test]
    fn rejects_missing_wrong_extreme_and_forbidden_result_fields() {
        for raw in [
            r#"{"type":"fortune","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":" ","difficulty":"easy","dc":10,"success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"成功","failure_meaning":""}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":"10","success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10.5,"success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":2147483648,"success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"成功","failure_meaning":"失败","roll":10}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"成功","failure_meaning":"失败","total":10}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","dc":10,"success_meaning":"成功","failure_meaning":"失败","success":true}"#,
        ] {
            let error = parse_dm_check_plan(raw, &DiceExpression::default_d20()).unwrap_err();
            assert_eq!(error.code, "roll_dm_invalid_output");
        }
    }
}
