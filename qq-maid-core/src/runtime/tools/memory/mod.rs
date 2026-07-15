//! Memory 领域操作门面。
//!
//! 本模块集中维护 personal、群内画像与群组公共记忆的授权、可见性和多步操作语义；
//! storage 仅执行精确查询与原子事务。

mod draft;
mod ops;
pub mod storage;
mod types;

pub(crate) use draft::{
    classify_memory, contains_sensitive_text, parse_valid_memory_draft_content,
};
pub use ops::MemoryOperations;
pub use storage::*;
pub use types::{
    MemoryActor, MemoryMutationResult, MemoryWriteResult, ProfilePreferenceResult,
    ReplaceScopedMemoryRequest, SaveMemoryRequest,
};

#[cfg(test)]
mod tests;

pub(crate) mod route {
    pub(crate) fn has_memory_intent(text: &str, lower: &str) -> bool {
        lower.contains("memory")
            || ["记忆", "记一下", "记住", "帮我记", "记录一下", "保存一下"]
                .iter()
                .any(|needle| text.contains(needle))
    }
}
