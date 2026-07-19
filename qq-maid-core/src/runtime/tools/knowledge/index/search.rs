use std::collections::{HashMap, HashSet};

use crate::{
    error::LlmError,
    runtime::tools::knowledge::storage::{KnowledgeSearchResult, KnowledgeStore},
};

use super::{
    KnowledgeEvidence, KnowledgeEvidenceDiagnostics, KnowledgeEvidenceItem,
    KnowledgeEvidenceStatus, KnowledgeInjectionDecision, KnowledgeInjectionReason,
    KnowledgeRecallType, KnowledgeTruncationReason,
    embedding::SemanticRuntime,
    text::{build_search_query, hash_text, identifier_terms, relevance_terms},
};

const SEARCH_CONTEXT_LIMIT: usize = 4;
pub(super) const SEARCH_TOTAL_CHAR_BUDGET: usize = 3200;
const PREFLIGHT_CONTEXT_LIMIT: usize = 1;
const PREFLIGHT_TOTAL_CHAR_BUDGET: usize = 1200;
const MAX_RESULTS_PER_FILE: usize = 2;
const MAX_SEARCH_QUERY_TOKENS: usize = 64;
const RRF_K: f64 = 60.0;
const THRESHOLD_VERSION: &str = "knowledge-preflight-v1";

const LEXICAL_HIGH_COVERAGE: f64 = 0.55;
const SEMANTIC_HIGH_SIMILARITY: f64 = 0.60;
const HYBRID_MIN_COVERAGE: f64 = 0.28;
const HYBRID_MIN_SIMILARITY: f64 = 0.62;
const TOOL_MIN_COVERAGE: f64 = 0.14;
const TOOL_MIN_SEMANTIC_SIMILARITY: f64 = 0.30;

// 先取更大的候选集，再做融合、按文件限流、章节去重和扩展。
pub(super) const SEARCH_CANDIDATE_LIMIT: usize = SEARCH_CONTEXT_LIMIT * MAX_RESULTS_PER_FILE * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KnowledgeSearchProfile {
    Tool,
    Preflight,
    AutoFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpansionMode {
    None,
    Section,
    Adjacent,
}

#[derive(Debug, Clone, Copy)]
struct SearchOptions {
    result_limit: usize,
    total_char_budget: usize,
    max_results_per_file: usize,
    expansion: ExpansionMode,
    use_semantic: bool,
    filter_low_relevance: bool,
    high_confidence_only: bool,
}

impl SearchOptions {
    fn for_profile(profile: KnowledgeSearchProfile) -> Self {
        match profile {
            KnowledgeSearchProfile::Tool => Self {
                result_limit: SEARCH_CONTEXT_LIMIT,
                total_char_budget: SEARCH_TOTAL_CHAR_BUDGET,
                max_results_per_file: MAX_RESULTS_PER_FILE,
                expansion: ExpansionMode::Section,
                use_semantic: true,
                filter_low_relevance: true,
                high_confidence_only: false,
            },
            KnowledgeSearchProfile::Preflight => Self {
                result_limit: PREFLIGHT_CONTEXT_LIMIT,
                total_char_budget: PREFLIGHT_TOTAL_CHAR_BUDGET,
                max_results_per_file: 1,
                expansion: ExpansionMode::None,
                use_semantic: true,
                filter_low_relevance: true,
                high_confidence_only: true,
            },
            KnowledgeSearchProfile::AutoFallback => Self {
                result_limit: SEARCH_CONTEXT_LIMIT,
                total_char_budget: SEARCH_TOTAL_CHAR_BUDGET,
                max_results_per_file: MAX_RESULTS_PER_FILE,
                expansion: ExpansionMode::Adjacent,
                use_semantic: false,
                filter_low_relevance: false,
                high_confidence_only: false,
            },
        }
    }
}

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
    semantic: Option<&SemanticRuntime>,
    queries: &[String],
    profile: KnowledgeSearchProfile,
    mut diagnostics: KnowledgeEvidenceDiagnostics,
) -> Result<KnowledgeEvidence, LlmError> {
    let options = SearchOptions::for_profile(profile);
    diagnostics.query_count = queries.len();
    let fused = retrieve_candidates(store, semantic, queries, options, &mut diagnostics)?;
    let ranked = rank_candidates(fused, profile, &mut diagnostics);
    let injection = injection_decision(&ranked);
    let selection = select_results(ranked, options, &injection);
    apply_selection_diagnostics(&selection, &mut diagnostics);
    finish_evidence(store, selection, options, diagnostics, injection)
}

fn retrieve_candidates(
    store: &KnowledgeStore,
    semantic: Option<&SemanticRuntime>,
    queries: &[String],
    options: SearchOptions,
    diagnostics: &mut KnowledgeEvidenceDiagnostics,
) -> Result<HashMap<String, RankedCandidate>, LlmError> {
    let mut fused = HashMap::<String, RankedCandidate>::new();
    for query in queries {
        let fts_query = query_text(query);
        if fts_query.is_empty() {
            continue;
        }
        let lexical = store
            .search(&fts_query, SEARCH_CANDIDATE_LIMIT)
            .map_err(search_db_error)?;
        diagnostics.fts_candidate_count += lexical.len();
        if lexical.len() >= SEARCH_CANDIDATE_LIMIT {
            push_reason_once(
                &mut diagnostics.truncation_reasons,
                KnowledgeTruncationReason::CandidateLimit,
            );
        }
        add_ranked_results(&mut fused, query, lexical, RecallChannel::Lexical);

        if options.use_semantic
            && let Some(semantic) = semantic
        {
            let semantic_results = semantic.search(store, query, SEARCH_CANDIDATE_LIMIT)?;
            diagnostics.semantic_candidate_count += semantic_results.len();
            add_ranked_results(&mut fused, query, semantic_results, RecallChannel::Semantic);
        }
    }
    Ok(fused)
}

fn rank_candidates(
    fused: HashMap<String, RankedCandidate>,
    profile: KnowledgeSearchProfile,
    diagnostics: &mut KnowledgeEvidenceDiagnostics,
) -> Vec<RankedCandidate> {
    diagnostics.fused_candidate_count = fused.len();
    let mut ranked = fused.into_values().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .rrf_score
            .partial_cmp(&left.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.result.chunk_id.cmp(&right.result.chunk_id))
    });
    diagnostics.top_lexical_coverage = ranked
        .iter()
        .map(|candidate| candidate.lexical_coverage)
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    diagnostics.top_semantic_similarity = ranked
        .iter()
        .filter_map(|candidate| candidate.semantic_similarity)
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));

    if profile == KnowledgeSearchProfile::Preflight {
        ranked.sort_by(|left, right| {
            preflight_confidence(right)
                .partial_cmp(&preflight_confidence(left))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.result.chunk_id.cmp(&right.result.chunk_id))
        });
    }
    ranked
}

fn apply_selection_diagnostics(
    selection: &Selection,
    diagnostics: &mut KnowledgeEvidenceDiagnostics,
) {
    diagnostics.selected_hit_count = selection.results.len();
    diagnostics.per_file_filtered_count = selection.per_file_filtered_count;
    diagnostics.duplicate_body_filtered_count = selection.duplicate_body_filtered_count;
    diagnostics.duplicate_section_filtered_count = selection.duplicate_section_filtered_count;
    diagnostics.low_relevance_filtered_count = selection.low_relevance_filtered_count;
    if selection.per_file_filtered_count > 0 {
        push_reason_once(
            &mut diagnostics.truncation_reasons,
            KnowledgeTruncationReason::PerFileLimit,
        );
    }
    if selection.result_limit_reached {
        push_reason_once(
            &mut diagnostics.truncation_reasons,
            KnowledgeTruncationReason::ResultLimit,
        );
    }
}

fn finish_evidence(
    store: &KnowledgeStore,
    selection: Selection,
    options: SearchOptions,
    mut diagnostics: KnowledgeEvidenceDiagnostics,
    injection: KnowledgeInjectionDecision,
) -> Result<KnowledgeEvidence, LlmError> {
    let expanded = expand_results(store, selection.results, options.expansion)?;
    diagnostics.expanded_chunk_count = expanded.len();
    diagnostics.section_expanded_count = expanded
        .iter()
        .filter(|result| result.recall_type == KnowledgeRecallType::Section)
        .count();
    let (items, character_budget_truncated) =
        evidence_items_within_budget(expanded, options.total_char_budget);
    diagnostics.returned_chunk_count = items.len();
    diagnostics.source_count = items
        .iter()
        .map(|item| item.relative_path.as_str())
        .collect::<HashSet<_>>()
        .len();
    if character_budget_truncated {
        push_reason_once(
            &mut diagnostics.truncation_reasons,
            KnowledgeTruncationReason::CharacterBudget,
        );
    }

    let status = if diagnostics.fused_candidate_count == 0 {
        KnowledgeEvidenceStatus::NoHit
    } else if items.is_empty() {
        KnowledgeEvidenceStatus::LowRelevance
    } else if diagnostics.truncation_reasons.is_empty() {
        KnowledgeEvidenceStatus::Ok
    } else {
        KnowledgeEvidenceStatus::Truncated
    };
    Ok(KnowledgeEvidence {
        status,
        items,
        diagnostics,
        injection,
        failure: None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecallChannel {
    Lexical,
    Semantic,
}

#[derive(Debug, Clone)]
struct RankedCandidate {
    result: KnowledgeSearchResult,
    rrf_score: f64,
    lexical_coverage: f64,
    lexical_match_count: usize,
    exact_identifier_match: bool,
    semantic_similarity: Option<f64>,
    lexical_seen: bool,
    semantic_seen: bool,
}

fn add_ranked_results(
    fused: &mut HashMap<String, RankedCandidate>,
    query: &str,
    results: Vec<KnowledgeSearchResult>,
    channel: RecallChannel,
) {
    for (index, result) in results.into_iter().enumerate() {
        let rank_score = 1.0 / (RRF_K + index as f64 + 1.0);
        let candidate_text = candidate_text(&result);
        let (coverage, match_count) = lexical_coverage(query, &candidate_text);
        let exact_identifier_match = has_exact_identifier(query, &candidate_text);
        let semantic_similarity = (channel == RecallChannel::Semantic).then_some(result.score);
        let entry = fused
            .entry(result.chunk_id.clone())
            .or_insert_with(|| RankedCandidate {
                result: result.clone(),
                rrf_score: 0.0,
                lexical_coverage: 0.0,
                lexical_match_count: 0,
                exact_identifier_match: false,
                semantic_similarity: None,
                lexical_seen: false,
                semantic_seen: false,
            });
        entry.rrf_score += rank_score;
        if channel == RecallChannel::Lexical {
            entry.lexical_seen = true;
            entry.result = result;
        } else {
            entry.semantic_seen = true;
            if !entry.lexical_seen {
                entry.result = result;
            }
        }
        if coverage > entry.lexical_coverage {
            entry.lexical_coverage = coverage;
            entry.lexical_match_count = match_count;
        }
        entry.exact_identifier_match |= exact_identifier_match;
        entry.semantic_similarity = max_option(entry.semantic_similarity, semantic_similarity);
    }
}

fn candidate_text(result: &KnowledgeSearchResult) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        result.document_title.as_deref().unwrap_or_default(),
        result.heading_path.as_deref().unwrap_or_default(),
        result.body,
        result.search_text,
    )
}

fn lexical_coverage(query: &str, candidate: &str) -> (f64, usize) {
    let terms = relevance_terms(query);
    if terms.is_empty() {
        return (0.0, 0);
    }
    let candidate_terms = relevance_terms(candidate)
        .into_iter()
        .collect::<HashSet<_>>();
    let mut matched_weight = 0.0;
    let mut total_weight = 0.0;
    let mut matched = 0;
    for term in terms {
        let weight = if term.is_ascii() {
            2.0
        } else if term.chars().count() >= 3 {
            1.5
        } else {
            1.0
        };
        total_weight += weight;
        if candidate_terms.contains(&term) {
            matched_weight += weight;
            matched += 1;
        }
    }
    (matched_weight / total_weight, matched)
}

fn has_exact_identifier(query: &str, candidate: &str) -> bool {
    let candidate = candidate.to_ascii_lowercase();
    identifier_terms(query)
        .into_iter()
        .any(|term| candidate.contains(&term))
}

fn injection_decision(ranked: &[RankedCandidate]) -> KnowledgeInjectionDecision {
    if ranked.is_empty() {
        return decision(false, KnowledgeInjectionReason::NoHit);
    }
    for candidate in ranked {
        if let Some(reason) = high_confidence_reason(candidate) {
            return decision(true, reason);
        }
    }
    decision(false, KnowledgeInjectionReason::BelowThreshold)
}

fn high_confidence_reason(candidate: &RankedCandidate) -> Option<KnowledgeInjectionReason> {
    let semantic = candidate.semantic_similarity.unwrap_or(f64::NEG_INFINITY);
    if candidate.lexical_seen
        && candidate.semantic_seen
        && candidate.lexical_coverage >= HYBRID_MIN_COVERAGE
        && semantic >= HYBRID_MIN_SIMILARITY
    {
        return Some(KnowledgeInjectionReason::HybridAgreement);
    }
    if candidate.lexical_seen
        && (candidate.exact_identifier_match
            || (candidate.lexical_coverage >= LEXICAL_HIGH_COVERAGE
                && candidate.lexical_match_count >= 3))
    {
        return Some(KnowledgeInjectionReason::LexicalHighConfidence);
    }
    (candidate.semantic_seen && semantic >= SEMANTIC_HIGH_SIMILARITY)
        .then_some(KnowledgeInjectionReason::SemanticHighConfidence)
}

fn preflight_confidence(candidate: &RankedCandidate) -> f64 {
    if candidate.exact_identifier_match {
        return 3.0;
    }
    if candidate.lexical_seen && candidate.lexical_coverage >= LEXICAL_HIGH_COVERAGE {
        return 2.0 + candidate.lexical_coverage;
    }
    if candidate.lexical_seen
        && candidate.semantic_seen
        && candidate.lexical_coverage >= HYBRID_MIN_COVERAGE
    {
        return 1.0 + candidate.semantic_similarity.unwrap_or_default();
    }
    candidate.semantic_similarity.unwrap_or_default()
}

fn decision(allow: bool, reason: KnowledgeInjectionReason) -> KnowledgeInjectionDecision {
    KnowledgeInjectionDecision {
        allow_injection: allow,
        reason,
        threshold_version: THRESHOLD_VERSION.to_owned(),
    }
}

struct Selection {
    results: Vec<RankedCandidate>,
    per_file_filtered_count: usize,
    duplicate_body_filtered_count: usize,
    duplicate_section_filtered_count: usize,
    low_relevance_filtered_count: usize,
    result_limit_reached: bool,
}

fn select_results(
    results: Vec<RankedCandidate>,
    options: SearchOptions,
    injection: &KnowledgeInjectionDecision,
) -> Selection {
    let mut selected = Vec::new();
    let mut per_file = HashMap::<String, usize>::new();
    let mut seen_bodies = HashSet::<String>::new();
    let mut seen_sections = HashSet::<(String, Option<String>)>::new();
    let mut per_file_filtered_count = 0;
    let mut duplicate_body_filtered_count = 0;
    let mut duplicate_section_filtered_count = 0;
    let mut low_relevance_filtered_count = 0;
    let mut result_limit_reached = false;
    for result in results {
        if selected.len() >= options.result_limit {
            result_limit_reached = true;
            break;
        }
        if options.high_confidence_only && high_confidence_reason(&result).is_none() {
            low_relevance_filtered_count += 1;
            continue;
        }
        if options.high_confidence_only && !injection.allow_injection {
            low_relevance_filtered_count += 1;
            continue;
        }
        if options.filter_low_relevance && !has_tool_relevance(&result) {
            low_relevance_filtered_count += 1;
            continue;
        }
        if per_file
            .get(&result.result.relative_path)
            .copied()
            .unwrap_or(0)
            >= options.max_results_per_file
        {
            per_file_filtered_count += 1;
            continue;
        }
        let body_hash = hash_text(&result.result.body);
        if !seen_bodies.insert(body_hash) {
            duplicate_body_filtered_count += 1;
            continue;
        }
        let section = (
            result.result.relative_path.clone(),
            result.result.heading_path.clone(),
        );
        if options.expansion == ExpansionMode::Section && !seen_sections.insert(section) {
            duplicate_section_filtered_count += 1;
            continue;
        }
        *per_file
            .entry(result.result.relative_path.clone())
            .or_default() += 1;
        selected.push(result);
    }
    Selection {
        results: selected,
        per_file_filtered_count,
        duplicate_body_filtered_count,
        duplicate_section_filtered_count,
        low_relevance_filtered_count,
        result_limit_reached,
    }
}

fn has_tool_relevance(candidate: &RankedCandidate) -> bool {
    candidate.exact_identifier_match
        || candidate.lexical_seen
        || (candidate.lexical_coverage >= TOOL_MIN_COVERAGE && candidate.lexical_match_count >= 2)
        || candidate
            .semantic_similarity
            .is_some_and(|score| score >= TOOL_MIN_SEMANTIC_SIMILARITY)
}

#[derive(Debug)]
struct ExpandedResult {
    result: KnowledgeSearchResult,
    recall_type: KnowledgeRecallType,
    score: Option<f64>,
}

fn expand_results(
    store: &KnowledgeStore,
    selected: Vec<RankedCandidate>,
    mode: ExpansionMode,
) -> Result<Vec<ExpandedResult>, LlmError> {
    let mut primary_ids = HashSet::new();
    let mut output = Vec::new();
    for candidate in &selected {
        primary_ids.insert(candidate.result.chunk_id.clone());
        output.push(ExpandedResult {
            result: candidate.result.clone(),
            recall_type: recall_type(candidate),
            score: Some(candidate.rrf_score),
        });
    }
    if mode == ExpansionMode::None {
        return Ok(output);
    }
    let mut seen_expanded = HashSet::new();
    for candidate in &selected {
        let chunks = match mode {
            ExpansionMode::Section => store.section_chunks(
                candidate.result.document_id,
                candidate.result.heading_path.as_deref(),
            ),
            ExpansionMode::Adjacent => {
                store.adjacent_chunks(candidate.result.document_id, candidate.result.chunk_index)
            }
            ExpansionMode::None => unreachable!(),
        }
        .map_err(search_db_error)?;
        for chunk in chunks {
            if !primary_ids.contains(&chunk.chunk_id)
                && seen_expanded.insert(chunk.chunk_id.clone())
            {
                output.push(ExpandedResult {
                    result: chunk,
                    recall_type: KnowledgeRecallType::Section,
                    score: None,
                });
            }
        }
    }
    Ok(output)
}

fn recall_type(candidate: &RankedCandidate) -> KnowledgeRecallType {
    match (candidate.lexical_seen, candidate.semantic_seen) {
        (true, true) => KnowledgeRecallType::Hybrid,
        (true, false) => KnowledgeRecallType::Lexical,
        (false, true) => KnowledgeRecallType::Semantic,
        (false, false) => KnowledgeRecallType::Lexical,
    }
}

fn evidence_items_within_budget(
    results: Vec<ExpandedResult>,
    total_char_budget: usize,
) -> (Vec<KnowledgeEvidenceItem>, bool) {
    let mut rendered_chars = super::evidence::KNOWLEDGE_CONTEXT_PREAMBLE.chars().count();
    let mut items = Vec::new();
    let mut truncated = false;
    for expanded in results {
        let remaining = total_char_budget.saturating_sub(rendered_chars);
        if remaining == 0 {
            truncated = true;
            break;
        }
        let result = expanded.result;
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
            score: expanded.score,
            recall_type: expanded.recall_type,
            body_excerpt,
        };
        rendered_chars += super::evidence::rendered_item(&item).chars().count();
        items.push(item);
    }
    (items, truncated)
}

fn max_option(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn push_reason_once(
    reasons: &mut Vec<KnowledgeTruncationReason>,
    reason: KnowledgeTruncationReason,
) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn take_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn search_db_error(error: crate::storage::database::DatabaseError) -> LlmError {
    LlmError::new(
        "knowledge_db_error",
        format!("knowledge search database error: {}", error.message()),
        "knowledge",
    )
}
