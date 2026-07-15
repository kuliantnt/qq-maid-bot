//! Memory 领域操作门面。
//!
//! 本模块集中维护 personal、群内画像与群组公共记忆的授权、可见性和多步操作语义；
//! storage 仅执行精确查询与原子事务。

pub(crate) mod agent_turn;
mod draft;
mod flow;
mod ops;
mod pending;
mod receipt;
pub(crate) mod route;
mod save;
pub mod storage;
pub(crate) mod tool_policy;
mod types;

pub(crate) use draft::{
    contains_sensitive_text, normalize_explicit_memory_content, parse_valid_memory_draft_content,
    prepare_memory_draft,
};
pub use ops::MemoryOperations;
pub(crate) use pending::{
    MEMORY_PENDING_DOMAIN, MemoryPendingPayload, PreparedMemoryDraft, draft_confirmation_text,
    memory_lexicon,
};
pub(crate) use receipt::{format_memory_saved_reply, memory_kind_label, memory_write_error_reply};
pub(crate) use route::infer_group_memory_kind;
pub use save::SaveMemoryTool;
pub use storage::*;
pub(crate) use types::MemoryRecall;
pub use types::{
    MemoryActor, MemoryMutationResult, MemoryWriteResult, ProfilePreferenceResult,
    ReplaceScopedMemoryRequest, SaveMemoryRequest,
};

pub const SAVE_MEMORY_TOOL_NAME: &str = "save_memory";

#[cfg(test)]
mod tests;
