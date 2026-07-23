//! Memory 领域操作门面。
//!
//! 本模块集中维护 personal、群内画像与群组公共记忆的授权、可见性和多步操作语义；
//! storage 仅执行精确查询与原子事务。

pub(crate) mod agent_turn;
mod consolidation;
mod draft;
mod dream;
mod flow;
mod ops;
mod pending;
mod recall;
mod receipt;
pub(crate) mod route;
mod save;
pub(crate) mod status;
pub mod storage;
mod types;

pub use consolidation::{
    MemoryConsolidationConfig, MemoryConsolidationRunStats, MemoryConsolidationWorker,
};
pub(crate) use draft::{
    contains_sensitive_text, normalize_explicit_memory_content, parse_valid_memory_draft_content,
    prepare_memory_draft,
};
pub use dream::MemoryDreamConfig;
pub(crate) use dream::{MemoryDreamContext, MemoryDreamWorker};
pub use ops::MemoryOperations;
pub(crate) use pending::{
    MEMORY_PENDING_DOMAIN, MemoryPendingPayload, PreparedMemoryDraft, draft_confirmation_text,
    memory_lexicon,
};
pub(crate) use receipt::{
    GROUP_MEMORY_COMMAND_ONLY_REPLY, format_memory_saved_reply, memory_kind_label,
    memory_write_error_reply,
};
pub(crate) use route::infer_group_memory_kind;
pub use save::SaveMemoryTool;
// 只暴露 Memory 领域的稳定持久化门面；storage 内部查询/行映射不应通过通配导出
// 扩散到 respond 或其他业务域。
pub use storage::{
    CreateMemoryRequest, CreateScopedMemoryRequest, ListMemoryQuery,
    MEMORY_CONSOLIDATION_SCHEMA_V4, MEMORY_DOMAIN_SCHEMA_V3, MEMORY_MIGRATIONS, MEMORY_SCHEMA_V1,
    MEMORY_SCOPE_SCHEMA_V2, MemoryCategory, MemoryDeleteResponse, MemoryError, MemoryErrorInfo,
    MemoryItemResponse, MemoryKind, MemoryListResponse, MemoryQuery, MemoryRecord, MemoryScopeType,
    MemorySourceType, MemoryStatus, MemoryStore, MemoryTarget, MemoryVisibility, ScopedMemoryQuery,
    UpdateMemoryRequest,
};
pub(crate) use types::MemoryRecall;
pub use types::{
    MemoryActor, MemoryMutationResult, MemoryWriteResult, ProfilePreferenceResult,
    ReplaceScopedMemoryRequest, SaveMemoryRequest,
};

pub const SAVE_MEMORY_TOOL_NAME: &str = "save_memory";

#[cfg(test)]
mod tests;
