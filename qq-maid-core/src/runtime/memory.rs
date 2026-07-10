//! 记忆操作的重导出。
//!
//! 将 `storage::memory` 模块中的全部公开类型和函数重新导出到运行时层，
//! 供 runtime 模块内的其他子模块统一使用。

pub(crate) mod route {
    //! 记忆普通消息 Agent Chat 路由判断。
    //!
    //! 长期记忆仍必须走明确记忆意图和用户确认流程；这里仅决定普通消息是否可进入
    //! 受控 Tool Loop，真实草稿/确认/写入规则由 memory_flow 与 Memory Tool 负责。

    pub(crate) fn has_memory_intent(text: &str, lower: &str) -> bool {
        lower.contains("memory")
            || contains_any(text, &["记忆"])
            || contains_any(text, &["记一下", "记住", "帮我记", "记录一下", "保存一下"])
    }

    fn contains_any(text: &str, needles: &[&str]) -> bool {
        needles.iter().any(|needle| text.contains(needle))
    }
}

pub use crate::storage::memory::*;
