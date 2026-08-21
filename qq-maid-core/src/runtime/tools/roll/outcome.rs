//! D20 本地结算与确定性回执。

use super::dm::DmCheckPlan;

pub(super) const DEFAULT_DIE_SIDES: u8 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RollResult {
    pub(super) value: u8,
    pub(super) sides: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RollOutcome {
    CriticalSuccess,
    Success,
    Failure,
    CriticalFailure,
}

impl RollOutcome {
    fn resolve(roll: u8, dc: u8) -> Self {
        match roll {
            20 => Self::CriticalSuccess,
            1 => Self::CriticalFailure,
            value if value >= dc => Self::Success,
            _ => Self::Failure,
        }
    }
}

pub(super) fn render_dm_result(plan: &DmCheckPlan, roll: u8) -> String {
    debug_assert!((1..=DEFAULT_DIE_SIDES).contains(&roll));
    let dc = plan.difficulty.dc();
    let outcome = RollOutcome::resolve(roll, dc);
    let check_name = display_check_name(&plan.check_name);
    let check_type = match plan.check_type {
        super::dm::CheckType::Ability => "ability",
        super::dm::CheckType::Fortune => "fortune",
    };
    tracing::debug!(
        check_type,
        difficulty = plan.difficulty.display_name(),
        dc,
        roll,
        result = outcome.as_str(),
        "已完成本地 D20 判定"
    );

    let (heading, meaning) = match outcome {
        RollOutcome::CriticalSuccess => ("✨ Natural 20！大成功", &plan.success_meaning),
        RollOutcome::Success => ("✅ 成功", &plan.success_meaning),
        RollOutcome::Failure => ("❌ 失败", &plan.failure_meaning),
        RollOutcome::CriticalFailure => ("💀 Natural 1！大失败", &plan.failure_meaning),
    };
    let meaning = sentence(meaning);
    match outcome {
        RollOutcome::CriticalSuccess | RollOutcome::CriticalFailure => format!(
            "{heading}\n\n🎲 {check_name}\n难度：{}（DC {dc}）\n投掷：{roll}\n\n{meaning}",
            plan.difficulty.display_name()
        ),
        RollOutcome::Success | RollOutcome::Failure => format!(
            "🎲 {check_name}\n难度：{}（DC {dc}）\n投掷：{roll}\n\n{heading}\n\n{meaning}",
            plan.difficulty.display_name()
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
    use crate::runtime::tools::roll::dm::{CheckType, Difficulty};

    fn plan(difficulty: Difficulty) -> DmCheckPlan {
        DmCheckPlan {
            check_type: CheckType::Fortune,
            check_name: "命运检定".to_owned(),
            difficulty,
            success_meaning: "适合行动".to_owned(),
            failure_meaning: "暂缓行动".to_owned(),
        }
    }

    #[test]
    fn resolves_normal_success_and_failure_locally() {
        let plan = plan(Difficulty::Easy);
        let success = render_dm_result(&plan, 14);
        assert!(success.contains("✅ 成功"));
        assert!(success.contains("适合行动。"));

        let failure = render_dm_result(&plan, 7);
        assert!(failure.contains("❌ 失败"));
        assert!(failure.contains("暂缓行动。"));
    }

    #[test]
    fn natural_twenty_and_one_override_dc() {
        let critical_success = render_dm_result(&plan(Difficulty::NearlyImpossible), 20);
        assert!(critical_success.starts_with("✨ Natural 20！大成功"));
        assert!(critical_success.contains("DC 30"));
        assert!(critical_success.contains("适合行动。"));

        let critical_failure = render_dm_result(&plan(Difficulty::VeryEasy), 1);
        assert!(critical_failure.starts_with("💀 Natural 1！大失败"));
        assert!(critical_failure.contains("DC 5"));
        assert!(critical_failure.contains("暂缓行动。"));
    }
}
