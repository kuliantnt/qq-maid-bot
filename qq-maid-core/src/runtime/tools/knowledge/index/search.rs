use std::collections::{HashMap, HashSet};

use crate::runtime::tools::knowledge::storage::{KnowledgeSearchResult, KnowledgeStore};

use super::{
    KnowledgeEvidence, KnowledgeEvidenceDiagnostics, KnowledgeEvidenceItem,
    KnowledgeEvidenceStatus, KnowledgeRecallType, KnowledgeTruncationReason,
};

use super::text::{build_search_query, hash_text};

const SEARCH_CONTEXT_LIMIT: usize = 4;
const SEARCH_TOTAL_CHAR_BUDGET: usize = 3200;
const MAX_RESULTS_PER_FILE: usize = 2;
const MAX_SEARCH_QUERY_TOKENS: usize = 64;

// 先取更大的候选集，再做按文件限流、去重和邻接补全；
// 否则单个高命中文档会把其他来源挤出 top N。
pub(super) const SEARCH_CANDIDATE_LIMIT: usize = SEARCH_CONTEXT_LIMIT * MAX_RESULTS_PER_FILE * 4;

pub(super) fn query_text(user_text: &str) -> String {
    build_search_query(user_text, MAX_SEARCH_QUERY_TOKENS)
}

pub(super) fn query_diagnostics(query: &str) -> (String, usize) {
    let fingerprint = hash_text(query).chars().take(12).collect();
    let token_count = query
        .split(" OR ")
        .filter(|token| !token.trim().is_empty())
        .count();
    (fingerprint, token_count)
}

pub(super) fn build_evidence(
    store: &KnowledgeStore,
    results: Vec<KnowledgeSearchResult>,
    mut diagnostics: KnowledgeEvidenceDiagnostics,
) -> Result<KnowledgeEvidence, crate::storage::database::DatabaseError> {
    diagnostics.fts_candidate_count = results.len();
    if results.len() >= SEARCH_CANDIDATE_LIMIT {
        diagnostics
            .truncation_reasons
            .push(KnowledgeTruncationReason::CandidateLimit);
    }

    let selection = select_results(results);
    diagnostics.selected_hit_count = selection.results.len();
    diagnostics.per_file_filtered_count = selection.per_file_filtered_count;
    diagnostics.duplicate_body_filtered_count = selection.duplicate_body_filtered_count;
    if selection.per_file_filtered_count > 0 {
        diagnostics
            .truncation_reasons
            .push(KnowledgeTruncationReason::PerFileLimit);
    }
    if selection.result_limit_reached {
        diagnostics
            .truncation_reasons
            .push(KnowledgeTruncationReason::ResultLimit);
    }

    let expanded = expand_with_adjacent_chunks(store, selection.results)?;
    diagnostics.expanded_chunk_count = expanded.len();
    let (items, character_budget_truncated) = evidence_items_within_budget(expanded);
    diagnostics.returned_chunk_count = items.len();
    diagnostics.source_count = items
        .iter()
        .map(|item| item.relative_path.as_str())
        .collect::<HashSet<_>>()
        .len();
    if character_budget_truncated {
        diagnostics
            .truncation_reasons
            .push(KnowledgeTruncationReason::CharacterBudget);
    }

    let status = if diagnostics.fts_candidate_count == 0 {
        KnowledgeEvidenceStatus::NoHit
    } else if diagnostics.truncation_reasons.is_empty() {
        KnowledgeEvidenceStatus::Ok
    } else {
        KnowledgeEvidenceStatus::Truncated
    };
    Ok(KnowledgeEvidence {
        status,
        items,
        diagnostics,
        failure: None,
    })
}

struct Selection {
    results: Vec<KnowledgeSearchResult>,
    per_file_filtered_count: usize,
    duplicate_body_filtered_count: usize,
    result_limit_reached: bool,
}

fn select_results(results: Vec<KnowledgeSearchResult>) -> Selection {
    let mut selected = Vec::new();
    let mut per_file = HashMap::<String, usize>::new();
    let mut seen_bodies = HashSet::<String>::new();
    let mut per_file_filtered_count = 0;
    let mut duplicate_body_filtered_count = 0;
    let mut result_limit_reached = false;
    for result in results {
        if selected.len() >= SEARCH_CONTEXT_LIMIT {
            result_limit_reached = true;
            break;
        }
        if per_file.get(&result.relative_path).copied().unwrap_or(0) >= MAX_RESULTS_PER_FILE {
            per_file_filtered_count += 1;
            continue;
        }
        let body_hash = hash_text(&result.body);
        if !seen_bodies.insert(body_hash) {
            duplicate_body_filtered_count += 1;
            continue;
        }
        *per_file.entry(result.relative_path.clone()).or_default() += 1;
        selected.push(result);
    }
    Selection {
        results: selected,
        per_file_filtered_count,
        duplicate_body_filtered_count,
        result_limit_reached,
    }
}

fn expand_with_adjacent_chunks(
    store: &KnowledgeStore,
    selected: Vec<KnowledgeSearchResult>,
) -> Result<Vec<KnowledgeSearchResult>, crate::storage::database::DatabaseError> {
    let mut expanded = Vec::new();
    let mut seen_chunk_ids = HashSet::<String>::new();
    for result in selected {
        let mut group = store.adjacent_chunks(result.document_id, result.chunk_index)?;
        group.push(result);
        group.sort_by_key(|item| item.chunk_index);
        for item in group {
            if seen_chunk_ids.insert(item.chunk_id.clone()) {
                expanded.push(item);
            }
        }
    }
    Ok(expanded)
}

fn evidence_items_within_budget(
    results: Vec<KnowledgeSearchResult>,
) -> (Vec<KnowledgeEvidenceItem>, bool) {
    let mut rendered_chars = super::evidence::KNOWLEDGE_CONTEXT_PREAMBLE.chars().count();
    let mut items = Vec::new();
    let mut truncated = false;
    for result in results {
        let remaining = SEARCH_TOTAL_CHAR_BUDGET.saturating_sub(rendered_chars);
        if remaining == 0 {
            truncated = true;
            break;
        }
        let mut body_excerpt = result.body.trim().to_owned();
        if body_excerpt.chars().count() > remaining {
            body_excerpt = take_chars(&body_excerpt, remaining.saturating_sub(16));
            body_excerpt.push_str("\n[片段已裁剪]");
            truncated = true;
        }
        let item = KnowledgeEvidenceItem {
            chunk_id: result.chunk_id,
            relative_path: result.relative_path,
            document_title: result.document_title,
            heading_path: result.heading_path,
            start_line: result.start_line,
            end_line: result.end_line,
            score: (!result.adjacent).then_some(result.score),
            recall_type: if result.adjacent {
                KnowledgeRecallType::Adjacent
            } else {
                KnowledgeRecallType::Lexical
            },
            body_excerpt,
        };
        rendered_chars += super::evidence::rendered_item(&item).chars().count();
        items.push(item);
    }
    (items, truncated)
}

fn take_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}
