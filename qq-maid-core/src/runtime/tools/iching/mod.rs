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

/// 带问题文本时不把问题公开给起卦流程，引导用户把问题留在心里后重新正式起卦。
pub(crate) const ICHING_ARGUMENT_HINT: &str =
    "所问之事藏在心里就好。默念三遍后，请直接发送 /算卦。";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IChingCommand {
    Cast,
}

/// 只接受无参数的起卦别名；带参数的同名命令仍由分派器识别并返回提示，
/// 避免继续落入天气快捷解析，也不把问题文本交给起卦流程。
pub(crate) fn parse_iching_command(text: &str) -> Option<IChingCommand> {
    let command = parse_slash_command(text)?;
    (command.action == "iching" && command.argument.is_empty()).then_some(IChingCommand::Cast)
}

/// 判断输入是否显式使用了算卦动作（包括带参数的无效形式）。
///
/// 分派器需要区分“不是算卦命令”和“算卦命令但带了参数”，否则后者会被天气
/// 的 `/城市天气` 快捷解析误认成天气查询。
pub(crate) fn is_iching_command(text: &str) -> bool {
    parse_slash_command(text).is_some_and(|command| command.action == "iching")
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
