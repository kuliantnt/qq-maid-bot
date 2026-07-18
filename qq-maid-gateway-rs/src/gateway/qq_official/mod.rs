//! QQ 官方机器人平台处理入口。
//!
//! 这里收纳 QQ 私聊与群聊的 Core 前置编排和平台回复发送；跨平台命令、诊断与
//! 统一入站模型继续留在 Gateway 共享模块。

pub(crate) mod c2c;
pub(crate) mod group;
