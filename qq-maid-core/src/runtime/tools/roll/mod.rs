//! 骰子 Slash 命令领域逻辑。
//!
//! 随机数只在 Core 进程内生成，命令分派层只负责确定性收口和响应投影。

use crate::runtime::command::parse_slash_command;

const DEFAULT_DIE_SIDES: u8 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RollCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RollResult {
    value: u8,
    sides: u8,
}

/// 仅注册本次明确支持的无参数 `/roll`；其它骰子表达式继续走统一未知命令兜底。
pub(crate) fn parse_roll_command(text: &str) -> Option<RollCommand> {
    let command = parse_slash_command(text)?;
    (command.action == "roll" && command.argument.is_empty()).then_some(RollCommand)
}

/// 执行默认 D20，并生成由命令层直接返回的简短回执。
pub(crate) fn roll_default_reply() -> String {
    let mut rng = fastrand::Rng::new();
    let result = roll_default_with_rng(&mut rng);
    format!("🎲 掷出了 {} / {}", result.value, result.sides)
}

fn roll_default_with_rng(rng: &mut fastrand::Rng) -> RollResult {
    RollResult {
        value: rng.u8(1..=DEFAULT_DIE_SIDES),
        sides: DEFAULT_DIE_SIDES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_argument_roll_is_recognized_without_claiming_other_commands() {
        assert_eq!(parse_roll_command("/roll"), Some(RollCommand));
        assert_eq!(parse_roll_command("  /ROLL  "), Some(RollCommand));
        assert_eq!(parse_roll_command("/roll 100"), None);
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
            assert!(result.value >= 1, "roll must not be less than 1");
            assert!(result.value <= 20, "roll must not be greater than 20");
        }
    }
}
