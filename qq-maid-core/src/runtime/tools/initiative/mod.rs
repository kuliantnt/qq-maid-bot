//! 群级轻量先攻门面；不依赖人物卡、模型或 Campaign，重启后临时状态清空。
mod command;
mod ops;
#[cfg(test)]
mod tests;

pub(crate) use command::{InitiativeCommand, parse_command};
pub(crate) use ops::InitiativeService;
