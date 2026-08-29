//! 《周易》起卦领域。
//!
//! 本模块只负责固定骰式的起卦、六爻/变卦计算和原文投影。骰值由 `roll` 领域通过
//! 现有 `/r6#(3d2+3)` 解析与 Roller 生成；这里不维护第二套随机数或表达式实现。

mod data;
mod logic;
mod receipt;

#[cfg(test)]
mod tests;

use crate::runtime::{command::parse_slash_command, tools::roll};

pub(crate) use logic::calculate_cast;
pub(crate) use receipt::render_cast;

const ICHING_ROLL_COMMAND: &str = "/r6#(3d2+3)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IChingCommand {
    Cast,
}

/// 只接受无参数 `/算卦`；参数存在时交给统一未知命令收口，避免默默忽略用户输入。
pub(crate) fn parse_iching_command(text: &str) -> Option<IChingCommand> {
    let command = parse_slash_command(text)?;
    (command.action == "iching" && command.argument.is_empty()).then_some(IChingCommand::Cast)
}

/// 执行一次六爻起卦，骰值全部来自现有 Roll 领域。
pub(crate) fn execute_iching_command(command: IChingCommand) -> String {
    match command {
        IChingCommand::Cast => {
            let totals = roll::roll_local_command_totals(ICHING_ROLL_COMMAND)
                .expect("算卦固定骰式必须能被 Roll 领域解析");
            let values: [u8; 6] = totals
                .into_iter()
                .map(|total| u8::try_from(total).expect("算卦骰式总值必须在 u8 范围内"))
                .collect::<Vec<_>>()
                .try_into()
                .expect("算卦固定骰式必须恰好投掷六轮");
            let result = calculate_cast(values).expect("算卦骰式总值必须落在 6 到 9");
            render_cast(&result)
        }
    }
}
