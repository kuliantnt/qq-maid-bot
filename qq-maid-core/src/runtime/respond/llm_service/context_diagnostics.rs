//! Tool / Agent Loop 上下文尺寸与阶段性内存诊断。
//!
//! 本模块只输出“尺寸、计数与进程内存读数”，绝不输出聊天正文、知识正文、
//! 搜索正文、API Key 或 Authorization。所有字段都在请求上下文基础上做聚合，
//! 便于在 `before_route` / `before_build_llm_messages` /
//! `after_build_llm_messages` / `before_knowledge_search` /
//! `after_knowledge_search` / `before_llm_request` / `after_llm_request` /
//! `after_tool_result` / `request_end` 等阶段观察上下文是否台阶式放大。

use qq_maid_common::{input_part::MessageInputPart, process_mem::process_memory_sample};
use sha2::{Digest, Sha256};

use super::super::{RespondPurpose, RespondRequest};
use crate::service::VisibleEntitySnapshot;

/// 大上下文告警阈值：估算请求字符数超过该值时输出 warn。
///
/// 与 Issue #361 建议一致：`estimated_request_chars > 100_000` 或 Tool Loop
/// `input_tokens > 8000` 时告警；阈值只用于诊断，不改变任何预算或行为。
pub(super) const LARGE_CONTEXT_WARN_CHARS: usize = 100_000;

/// 单条请求的分项字符统计；全部为诊断计数，不包含正文内容。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RequestSizeStats {
    /// 进入模型的历史消息条数。
    pub history_message_count: usize,
    /// 历史消息正文总字符数。
    pub history_chars: usize,
    /// 系统提示词总字符数。
    pub system_chars: usize,
    /// 已持久化会话摘要锚点字符数。
    pub summary_chars: usize,
    /// 知识证据上下文字符数。
    pub knowledge_evidence_chars: usize,
    /// 长期记忆上下文字符数。
    pub memory_chars: usize,
    /// 会话状态上下文字符数。
    pub session_chars: usize,
    /// 当前用户指令字符数。
    pub user_chars: usize,
    /// 当前消息输入块数量。
    pub input_part_count: usize,
    /// 引用消息（ref index 回填）文本字符数。
    pub quoted_chars: usize,
    /// 请求级可见实体快照条目数。
    pub visible_snapshot_count: usize,
    /// 请求级可见实体快照字符数。
    pub visible_snapshot_chars: usize,
    /// 请求级 Todo 可见快照条目数。
    pub todo_snapshot_count: usize,
    /// 请求级 Todo 可见快照字符数。
    pub todo_snapshot_chars: usize,
}

impl RequestSizeStats {
    /// 从 `RespondRequest` 聚合分项统计；不读取任何持久化状态。
    pub(super) fn from_request(req: &RespondRequest) -> Self {
        let history_chars = req
            .history_messages
            .iter()
            .map(|message| {
                message.content.chars().count()
                    + message
                        .content_parts
                        .iter()
                        .map(|part| match part {
                            MessageInputPart::Text { text, .. } => text.chars().count(),
                            _ => 0,
                        })
                        .sum::<usize>()
            })
            .sum();
        let system_chars = req.system_prompts.iter().map(String::len).sum();
        let quoted_chars = req
            .quoted
            .as_ref()
            .map(|quoted| {
                quoted.fallback_text().chars().count() + quoted.metadata_text().chars().count()
            })
            .unwrap_or(0);
        let (visible_snapshot_count, visible_snapshot_chars) =
            visible_snapshot_stats(req.visible_entity_snapshot.as_ref());
        let (todo_snapshot_count, todo_snapshot_chars) =
            visible_snapshot_todo_stats(req.visible_entity_snapshot.as_ref());
        Self {
            history_message_count: req.history_messages.len(),
            history_chars,
            system_chars,
            summary_chars: req.history_summary.chars().count(),
            knowledge_evidence_chars: req.knowledge_context.chars().count(),
            memory_chars: req.memory_context.chars().count(),
            session_chars: req.session_context.chars().count(),
            user_chars: req.effective_user_text().chars().count(),
            input_part_count: req.effective_input_parts().len(),
            quoted_chars,
            visible_snapshot_count,
            visible_snapshot_chars,
            todo_snapshot_count,
            todo_snapshot_chars,
        }
    }

    /// 进入模型的估算请求总字符数；仅用于告警阈值判断，不替代 provider 预算。
    pub(super) fn estimated_request_chars(&self) -> usize {
        self.history_chars
            .saturating_add(self.system_chars)
            .saturating_add(self.summary_chars)
            .saturating_add(self.knowledge_evidence_chars)
            .saturating_add(self.memory_chars)
            .saturating_add(self.session_chars)
            .saturating_add(self.user_chars)
            .saturating_add(self.quoted_chars)
    }
}

fn visible_snapshot_stats(snapshot: Option<&VisibleEntitySnapshot>) -> (usize, usize) {
    let Some(snapshot) = snapshot else {
        return (0, 0);
    };
    let chars = snapshot
        .items
        .iter()
        .map(|item| {
            item.entity_id.chars().count()
                + item
                    .label
                    .as_deref()
                    .map(str::chars)
                    .map(|chars| chars.count())
                    .unwrap_or(0)
        })
        .sum();
    (snapshot.items.len(), chars)
}

fn visible_snapshot_todo_stats(snapshot: Option<&VisibleEntitySnapshot>) -> (usize, usize) {
    let Some(snapshot) = snapshot else {
        return (0, 0);
    };
    let mut count = 0usize;
    let mut chars = 0usize;
    for item in &snapshot.items {
        if item.domain != "todo" || item.entity_kind != "todo" {
            continue;
        }
        count += 1;
        chars += item.entity_id.chars().count()
            + item
                .label
                .as_deref()
                .map(str::chars)
                .map(|chars| chars.count())
                .unwrap_or(0);
    }
    (count, chars)
}

/// 一次请求入口处预计算的轻量诊断快照。
///
/// 计算过程只读取 `&RespondRequest`，不 clone 任何大字段；`request_end` 等
/// 请求对象已被 move 的阶段直接复用这份快照，避免为诊断再复制一遍上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestStageSnapshot {
    pub(super) purpose: &'static str,
    pub(super) scope_hash: String,
    pub(super) stats: RequestSizeStats,
}

/// 从 `&RespondRequest` 生成请求阶段诊断快照。
pub(crate) fn request_stage_snapshot(req: &RespondRequest) -> RequestStageSnapshot {
    RequestStageSnapshot {
        purpose: respond_purpose_name(&req.purpose),
        scope_hash: scope_key_hash(&req.scope_key),
        stats: RequestSizeStats::from_request(req),
    }
}

/// 输出单个请求阶段的脱敏诊断日志。
///
/// 只在 DEBUG 级别输出，避免生产默认日志膨胀；`smaps_rollup` 字段仅在显式
/// 开启 `QQ_MAID_MEMORY_DIAGNOSTICS` 时采样。
pub(super) fn log_request_stage(stage: &'static str, req: &RespondRequest) {
    log_request_stage_snapshot(stage, &request_stage_snapshot(req));
}

pub(crate) fn log_request_stage_snapshot(stage: &'static str, snapshot: &RequestStageSnapshot) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let stats = &snapshot.stats;
    let mem = process_memory_sample();
    tracing::debug!(
        stage,
        purpose = %snapshot.purpose,
        scope_hash = %snapshot.scope_hash,
        history_message_count = stats.history_message_count,
        history_chars = stats.history_chars,
        system_chars = stats.system_chars,
        summary_chars = stats.summary_chars,
        knowledge_evidence_chars = stats.knowledge_evidence_chars,
        memory_chars = stats.memory_chars,
        session_chars = stats.session_chars,
        user_chars = stats.user_chars,
        input_part_count = stats.input_part_count,
        quoted_chars = stats.quoted_chars,
        visible_snapshot_count = stats.visible_snapshot_count,
        visible_snapshot_chars = stats.visible_snapshot_chars,
        todo_snapshot_count = stats.todo_snapshot_count,
        todo_snapshot_chars = stats.todo_snapshot_chars,
        rss_kb = mem.rss_kb,
        vm_size_kb = mem.vm_size_kb,
        pss_kb = mem.pss_kb,
        private_dirty_kb = mem.private_dirty_kb,
        "respond request stage"
    );
}

/// 大上下文告警：估算请求字符数超过阈值时输出 warn，并给出分项统计定位。
///
/// 只输出计数与尺寸，不输出任何正文内容。
pub(super) fn warn_large_request_context(stage: &'static str, req: &RespondRequest) {
    warn_large_request_context_snapshot(stage, &request_stage_snapshot(req));
}

pub(crate) fn warn_large_request_context_snapshot(
    stage: &'static str,
    snapshot: &RequestStageSnapshot,
) {
    let stats = &snapshot.stats;
    let estimated_request_chars = stats.estimated_request_chars();
    if estimated_request_chars < LARGE_CONTEXT_WARN_CHARS {
        return;
    }
    let mem = process_memory_sample();
    tracing::warn!(
        stage,
        estimated_request_chars,
        scope_hash = %snapshot.scope_hash,
        history_message_count = stats.history_message_count,
        history_chars = stats.history_chars,
        system_chars = stats.system_chars,
        knowledge_evidence_chars = stats.knowledge_evidence_chars,
        memory_chars = stats.memory_chars,
        session_chars = stats.session_chars,
        user_chars = stats.user_chars,
        quoted_chars = stats.quoted_chars,
        visible_snapshot_count = stats.visible_snapshot_count,
        rss_kb = mem.rss_kb,
        vm_size_kb = mem.vm_size_kb,
        "large respond request context detected"
    );
}

/// 生成 `RespondRequest` 请求级估算字符数；供告警与测试复用。
pub(super) fn estimated_request_chars(req: &RespondRequest) -> usize {
    RequestSizeStats::from_request(req).estimated_request_chars()
}

/// scope_key 的短指纹，用于诊断关联但不输出完整群 ID / openid。
pub(super) fn scope_key_hash(scope_key: &str) -> String {
    let digest = Sha256::digest(scope_key.as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn respond_purpose_name(purpose: &RespondPurpose) -> &'static str {
    match purpose {
        RespondPurpose::Chat => "chat",
        RespondPurpose::MemoryDraft => "memory_draft",
        RespondPurpose::TodoParse => "todo_parse",
        RespondPurpose::Compact => "compact",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{VisibleEntityItem, VisibleEntitySnapshot};

    fn request() -> RespondRequest {
        RespondRequest {
            scope_key: "private:user-1".to_owned(),
            user_text: "今天完成第一条待办".to_owned(),
            system_prompts: vec!["系统提示词".repeat(3)],
            history_summary: "旧摘要".to_owned(),
            knowledge_context: "知识证据正文".repeat(5),
            memory_context: "记忆上下文".repeat(2),
            session_context: "会话状态".repeat(2),
            ..Default::default()
        }
    }

    #[test]
    fn request_size_stats_reports_per_item_chars() {
        let mut req = request();
        req.history_messages = vec![
            qq_maid_llm::provider::types::ChatMessage::user("你好".repeat(10)),
            qq_maid_llm::provider::types::ChatMessage {
                role: qq_maid_llm::provider::types::ChatRole::Assistant,
                content: "在的".repeat(5),
                content_parts: Vec::new(),
            },
        ];
        req.visible_entity_snapshot = Some(VisibleEntitySnapshot {
            platform: "qq_official".to_owned(),
            account_id: None,
            scope_key: req.scope_key.clone(),
            owner_key: Some("owner".to_owned()),
            created_at: "2026-07-31T00:00:00+08:00".to_owned(),
            items: vec![
                VisibleEntityItem {
                    domain: "todo".to_owned(),
                    entity_kind: "todo".to_owned(),
                    entity_id: "todo-1".to_owned(),
                    visible_number: 1,
                    label: Some("第一条待办".to_owned()),
                    status: Some("pending".to_owned()),
                },
                VisibleEntityItem {
                    domain: "todo".to_owned(),
                    entity_kind: "todo".to_owned(),
                    entity_id: "todo-2".to_owned(),
                    visible_number: 2,
                    label: Some("第二条待办".to_owned()),
                    status: Some("pending".to_owned()),
                },
            ],
        });

        let stats = RequestSizeStats::from_request(&req);

        assert_eq!(stats.history_message_count, 2);
        assert_eq!(
            stats.history_chars,
            "你好".repeat(10).chars().count() + "在的".repeat(5).chars().count()
        );
        assert_eq!(stats.system_chars, "系统提示词".repeat(3).len());
        assert_eq!(
            stats.knowledge_evidence_chars,
            "知识证据正文".repeat(5).chars().count()
        );
        assert_eq!(stats.visible_snapshot_count, 2);
        assert_eq!(stats.todo_snapshot_count, 2);
        assert_eq!(stats.todo_snapshot_chars, stats.visible_snapshot_chars);
        assert!(stats.estimated_request_chars() > 0);
    }

    #[test]
    fn non_todo_snapshot_items_are_not_counted_as_todo() {
        let mut req = request();
        req.visible_entity_snapshot = Some(VisibleEntitySnapshot {
            platform: "qq_official".to_owned(),
            account_id: None,
            scope_key: req.scope_key.clone(),
            owner_key: Some("owner".to_owned()),
            created_at: "2026-07-31T00:00:00+08:00".to_owned(),
            items: vec![VisibleEntityItem {
                domain: "memory".to_owned(),
                entity_kind: "memory".to_owned(),
                entity_id: "mem-1".to_owned(),
                visible_number: 1,
                label: None,
                status: None,
            }],
        });

        let stats = RequestSizeStats::from_request(&req);

        assert_eq!(stats.visible_snapshot_count, 1);
        assert_eq!(stats.todo_snapshot_count, 0);
    }

    #[test]
    fn large_request_triggers_warning_threshold() {
        let mut req = request();
        req.knowledge_context = "长知识证据".repeat(30_000);
        assert!(estimated_request_chars(&req) >= LARGE_CONTEXT_WARN_CHARS);
    }

    #[test]
    fn scope_key_hash_is_stable_and_truncated() {
        let hash = scope_key_hash("private:user-1");
        assert_eq!(hash.len(), 16);
        assert_eq!(hash, scope_key_hash("private:user-1"));
        assert_ne!(hash, scope_key_hash("private:user-2"));
    }
}
