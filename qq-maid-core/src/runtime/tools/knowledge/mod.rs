//! 只读知识检索 Tool。

mod index;
mod storage;
mod tool;

#[cfg(test)]
mod agent_tests;

pub use index::{
    KnowledgeEvidence, KnowledgeEvidenceDiagnostics, KnowledgeEvidenceFailure,
    KnowledgeEvidenceItem, KnowledgeEvidenceStatus, KnowledgeIndex, KnowledgeRecallType,
    KnowledgeSyncSummary, KnowledgeTruncationReason, eval, render_context,
};
pub use storage::{
    KNOWLEDGE_MIGRATIONS, KNOWLEDGE_SCHEMA_V1, KNOWLEDGE_SCHEMA_V2, KnowledgeChunkDraft,
    KnowledgeStore,
};
pub use tool::{KNOWLEDGE_SEARCH_TOOL_NAME, KnowledgeSearchTool};
