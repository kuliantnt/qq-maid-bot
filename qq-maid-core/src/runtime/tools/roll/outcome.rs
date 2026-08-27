//! 本地骰子判定与确定性回执。

use super::{dice::RollResult, dm::DmCheckPlan};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RollOutcome {
    CriticalSuccess,
    Success,
    Failure,
    CriticalFailure,
}

impl RollOutcome {
    fn resolve(result: &RollResult, dc: i32) -> Self {
        if result.expression.is_default_d20() {
            match result.rolls[0].value {
                20 => return Self::CriticalSuccess,
                1 => return Self::CriticalFailure,
                _ => {}
            }
        }
        if result.total >= dc {
            Self::Success
        } else {
            Self::Failure
        }
    }
}

pub(super) fn render_dm_result(plan: &DmCheckPlan, result: &RollResult) -> String {
    let dc = plan.dc;
    let (dice_minimum, dice_maximum) = result.expression.total_range();
    let computed_dc = super::dm::compute_dc(&result.expression, plan.difficulty);
    let outcome = RollOutcome::resolve(result, dc);
    let check_name = display_check_name(&plan.check_name);
    let check_type = match plan.check_type {
        super::dm::CheckType::Ability => "ability",
        super::dm::CheckType::Fortune => "fortune",
    };
    tracing::debug!(
        check_type,
        difficulty = plan.difficulty.key(),
        dc,
        dice_minimum,
        dice_maximum,
        computed_dc = computed_dc.value,
        dc_strategy = computed_dc.strategy.as_str(),
        roll_expression = %result.expression,
        roll_total = result.total,
        result = outcome.as_str(),
        "已完成本地骰子判定"
    );

    let (heading, meaning) = match outcome {
        RollOutcome::CriticalSuccess => ("✨ Natural 20！大成功", &plan.success_meaning),
        RollOutcome::Success => ("✅ 成功", &plan.success_meaning),
        RollOutcome::Failure => ("❌ 失败", &plan.failure_meaning),
        RollOutcome::CriticalFailure => ("💀 Natural 1！大失败", &plan.failure_meaning),
    };
    let roll_display = if result.expression.is_single_unmodified() {
        result.calculation()
    } else {
        format!(
            "{}：{} = {}",
            result.expression,
            result.calculation(),
            result.total
        )
    };
    let meaning = sentence(meaning);
    match outcome {
        RollOutcome::CriticalSuccess | RollOutcome::CriticalFailure => format!(
            "{heading}\n\n🎲 {check_name}\n难度：{}（DC {dc}）\n投掷：{roll}\n\n{meaning}",
            plan.difficulty.display_name(),
            roll = roll_display
        ),
        RollOutcome::Success | RollOutcome::Failure => format!(
            "🎲 {check_name}\n难度：{}（DC {dc}）\n投掷：{roll}\n\n{heading}\n\n{meaning}",
            plan.difficulty.display_name(),
            roll = roll_display
        ),
    }
}

impl RollOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CriticalSuccess => "critical_success",
            Self::Success => "success",
            Self::Failure => "failure",
            Self::CriticalFailure => "critical_failure",
        }
    }
}

fn display_check_name(name: &str) -> String {
    if name.ends_with("检定") {
        name.to_owned()
    } else {
        format!("{name}检定")
    }
}

fn sentence(text: &str) -> String {
    if text.ends_with(['。', '！', '？', '.', '!', '?']) {
        text.to_owned()
    } else {
        format!("{text}。")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tools::roll::{
        dice::DiceExpression,
        dm::{CheckType, Difficulty},
    };

    fn plan(difficulty: Difficulty, dc: i32) -> DmCheckPlan {
        DmCheckPlan {
            check_type: CheckType::Fortune,
            check_name: "命运检定".to_owned(),
            difficulty,
            dc,
            success_meaning: "适合行动".to_owned(),
            failure_meaning: "暂缓行动".to_owned(),
        }
    }

    fn d20_result(value: u8) -> RollResult {
        DiceExpression::default_d20()
            .roll(&mut |_| value)
            .expect("test roller should return a valid d20")
    }

    #[test]
    fn resolves_normal_success_and_failure_locally() {
        let plan = plan(Difficulty::Easy, 8);
        let success = render_dm_result(&plan, &d20_result(14));
        assert!(success.contains("✅ 成功"));
        assert!(success.contains("适合行动。"));

        let failure = render_dm_result(&plan, &d20_result(7));
        assert!(failure.contains("❌ 失败"));
        assert!(failure.contains("暂缓行动。"));
    }

    #[test]
    fn natural_twenty_and_one_override_dc() {
        let critical_success =
            render_dm_result(&plan(Difficulty::NearlyImpossible, 20), &d20_result(20));
        assert!(critical_success.starts_with("✨ Natural 20！大成功"));
        assert!(critical_success.contains("DC 20"));
        assert!(critical_success.contains("适合行动。"));

        let critical_failure = render_dm_result(&plan(Difficulty::VeryEasy, 5), &d20_result(1));
        assert!(critical_failure.starts_with("💀 Natural 1！大失败"));
        assert!(critical_failure.contains("DC 5"));
        assert!(critical_failure.contains("暂缓行动。"));
    }

    #[test]
    fn custom_expression_uses_total_without_natural_twenty_override() {
        let expression = match super::super::dice::parse_expression("2d20") {
            super::super::dice::DiceExpressionParse::Parsed(expression) => expression,
            other => panic!("expected a valid test expression, got {other:?}"),
        };
        let result = expression
            .roll(&mut |sides| {
                assert_eq!(sides, 20);
                20
            })
            .expect("test roller should return valid values");
        let rendered = render_dm_result(&plan(Difficulty::NearlyImpossible, 39), &result);
        assert!(rendered.contains("投掷：2d20：20 + 20 = 40"));
        assert!(rendered.contains("✅ 成功"));
        assert!(!rendered.contains("Natural 20"));
    }
}
