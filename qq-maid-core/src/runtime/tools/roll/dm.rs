//! AI DM 判定方案生成与本地校验。

use std::collections::HashMap;

use qq_maid_llm::provider::{
    DynLlmProvider,
    types::{ChatMessage, ChatRequest, ReasoningEffort},
};
use serde::Deserialize;

use crate::error::LlmError;

const DM_SYSTEM_PROMPT: &str = r#"你是轻量跑团 DM，只负责为用户问题制定一次 D20 判定方案。
你看不到、不能决定也不得猜测实际骰值；不要自行掷骰，不要声称行动已经成功或失败。
日常二选一、运气和娱乐选择优先使用 fortune，通常选择 easy；只有问题明确很难时才提高难度。
潜行、说服、观察等实际行动使用 ability。check_name 是简短的完整检定名称。
不使用角色卡、属性值、熟练、装备或任何加值。difficulty 只能取允许的枚举。
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
    pub(super) const fn dc(self) -> u8 {
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
    success_meaning: String,
    failure_meaning: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DmCheckPlan {
    pub(super) check_type: CheckType,
    pub(super) check_name: String,
    pub(super) difficulty: Difficulty,
    pub(super) success_meaning: String,
    pub(super) failure_meaning: String,
}

pub(super) async fn prepare_dm_check(
    provider: &DynLlmProvider,
    model: Option<String>,
    query: &str,
) -> Result<DmCheckPlan, LlmError> {
    let request = ChatRequest {
        // 这是 provider 关联键，不对应也不会创建 conversation session。
        session_id: "roll-dm".to_owned(),
        model,
        messages: vec![
            ChatMessage::system(DM_SYSTEM_PROMPT),
            ChatMessage::user(query.to_owned()),
        ],
        context_budget: None,
        max_output_tokens: Some(DM_MAX_OUTPUT_TOKENS),
        reasoning_effort: Some(ReasoningEffort::Low),
        metadata: HashMap::from([
            ("purpose".to_owned(), "roll_dm_check".to_owned()),
            ("query_chars".to_owned(), query.chars().count().to_string()),
        ]),
    };
    let outcome = provider.chat(request).await?;
    parse_dm_check_plan(&outcome.reply)
}

fn parse_dm_check_plan(raw: &str) -> Result<DmCheckPlan, LlmError> {
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
    Ok(DmCheckPlan {
        check_type: raw.check_type,
        check_name: validate_text_field(raw.check_name, "check_name", CHECK_NAME_MAX_CHARS)?,
        difficulty: raw.difficulty,
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
    fn parses_allowed_schema_and_maps_difficulty_to_fixed_dc() {
        let plan = parse_dm_check_plan(
            r#"{"type":"ability","check_name":"敏捷（隐匿）","difficulty":"medium","success_meaning":"成功绕过守卫","failure_meaning":"被守卫发现"}"#,
        )
        .unwrap();
        assert_eq!(plan.check_type, CheckType::Ability);
        assert_eq!(plan.difficulty.dc(), 15);
        assert_eq!(plan.difficulty.display_name(), "中等");
    }

    #[test]
    fn accepts_a_single_outer_json_code_fence() {
        for raw in [
            "```json\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"success_meaning\":\"喝咖啡\",\"failure_meaning\":\"不喝咖啡\"}\n```",
            "```JSON\r\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"success_meaning\":\"喝咖啡\",\"failure_meaning\":\"不喝咖啡\"}\r\n```",
            "```\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"success_meaning\":\"喝咖啡\",\"failure_meaning\":\"不喝咖啡\"}\n```",
        ] {
            let plan = parse_dm_check_plan(raw).unwrap();
            assert_eq!(plan.check_type, CheckType::Fortune);
            assert_eq!(plan.difficulty, Difficulty::Easy);
        }
    }

    #[test]
    fn rejects_json_wrapped_in_explanation_or_an_unrelated_code_fence() {
        for raw in [
            "判定方案：\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"success_meaning\":\"成功\",\"failure_meaning\":\"失败\"}",
            "```text\n{\"type\":\"fortune\",\"check_name\":\"命运检定\",\"difficulty\":\"easy\",\"success_meaning\":\"成功\",\"failure_meaning\":\"失败\"}\n```",
        ] {
            let error = parse_dm_check_plan(raw).unwrap_err();
            assert_eq!(error.code, "roll_dm_invalid_output");
        }
    }

    #[test]
    fn every_difficulty_has_a_fixed_dc() {
        let cases = [
            (Difficulty::VeryEasy, "很容易", 5),
            (Difficulty::Easy, "容易", 10),
            (Difficulty::Medium, "中等", 15),
            (Difficulty::Hard, "困难", 20),
            (Difficulty::VeryHard, "很困难", 25),
            (Difficulty::NearlyImpossible, "近乎不可能", 30),
        ];
        for (difficulty, display, dc) in cases {
            assert_eq!(difficulty.display_name(), display);
            assert_eq!(difficulty.dc(), dc);
        }
    }

    #[test]
    fn rejects_invalid_json_unknown_type_and_unknown_difficulty() {
        for raw in [
            "not-json",
            r#"{"type":"skill","check_name":"检定","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"dc17","success_meaning":"成功","failure_meaning":"失败"}"#,
        ] {
            let error = parse_dm_check_plan(raw).unwrap_err();
            assert_eq!(error.code, "roll_dm_invalid_output");
        }
    }

    #[test]
    fn rejects_missing_empty_and_unknown_fields() {
        for raw in [
            r#"{"type":"fortune","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":" ","difficulty":"easy","success_meaning":"成功","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"","failure_meaning":"失败"}"#,
            r#"{"type":"fortune","check_name":"命运检定","difficulty":"easy","success_meaning":"成功","failure_meaning":"","dc":10}"#,
        ] {
            let error = parse_dm_check_plan(raw).unwrap_err();
            assert_eq!(error.code, "roll_dm_invalid_output");
        }
    }
}
