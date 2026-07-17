//! Memory 召回排序与多样性控制。
//!
//! SQL 层先按完整 target 和可见性取候选，本模块只在已经授权的单层候选内排序。
//! 用户明确保存的长期事实不做时间衰减；只有系统派生记录会随时间降低优先级。

use std::{cmp::Ordering, collections::HashSet};

use chrono::{DateTime, Utc};

use super::{
    storage::{MemoryRecord, MemorySourceType},
    types::MemoryRecall,
};

const PRIVATE_RECORD_LIMIT: usize = 12;
const GROUP_LAYER_RECORD_LIMIT: usize = 4;
const SYSTEM_DERIVED_HALF_LIFE_DAYS: f64 = 30.0;
const MMR_LAMBDA: f64 = 0.75;

pub(super) fn rerank_recall(
    mut recall: MemoryRecall,
    query: &str,
    shared_conversation: bool,
) -> MemoryRecall {
    let now = Utc::now();
    let personal_limit = if shared_conversation {
        GROUP_LAYER_RECORD_LIMIT
    } else {
        PRIVATE_RECORD_LIMIT
    };
    recall.personal = rank_layer(recall.personal, query, personal_limit, now);
    recall.group_profile = rank_layer(recall.group_profile, query, GROUP_LAYER_RECORD_LIMIT, now);
    recall.group = rank_layer(recall.group, query, GROUP_LAYER_RECORD_LIMIT, now);
    recall
}

fn rank_layer(
    records: Vec<MemoryRecord>,
    query: &str,
    limit: usize,
    now: DateTime<Utc>,
) -> Vec<MemoryRecord> {
    let query_features = text_features(query);
    let mut seen = HashSet::new();
    let mut candidates = records
        .into_iter()
        .filter(|record| seen.insert(dedup_key(record)))
        .map(|record| {
            let features = text_features(&record.content);
            let score = relevance_score(&record, &query_features, &features, now);
            RankedMemory {
                record,
                features,
                score,
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| right.record.created_at.cmp(&left.record.created_at))
    });
    // 本轮问题与任一候选都无词面关联时维持原有“置顶/来源/近期”顺序；
    // MMR 只用于相关候选，避免泛化寒暄无故打乱最近记忆和字符预算边界。
    if !candidates
        .iter()
        .any(|candidate| meaningful_query_match(&query_features, &candidate.features))
    {
        return candidates
            .into_iter()
            .take(limit)
            .map(|candidate| candidate.record)
            .collect();
    }
    mmr_select(candidates, limit)
}

fn meaningful_query_match(query: &HashSet<String>, content: &HashSet<String>) -> bool {
    if query.is_empty() {
        return false;
    }
    let required = query.len().min(2);
    query.intersection(content).take(required).count() >= required
}

#[derive(Debug)]
struct RankedMemory {
    record: MemoryRecord,
    features: HashSet<String>,
    score: f64,
}

fn relevance_score(
    record: &MemoryRecord,
    query_features: &HashSet<String>,
    content_features: &HashSet<String>,
    now: DateTime<Utc>,
) -> f64 {
    let overlap = if query_features.is_empty() {
        0.0
    } else {
        query_features.intersection(content_features).count() as f64 / query_features.len() as f64
    };
    let source_weight = match record.source_type {
        MemorySourceType::UserConfirmed => 1.20,
        MemorySourceType::ManualImport => 1.10,
        MemorySourceType::SystemDerived => 0.90,
        MemorySourceType::Legacy => 0.80,
    };
    let pinned_boost = if record.pinned { 0.60 } else { 0.0 };
    let base = source_weight + pinned_boost + overlap * 2.0;
    base * temporal_multiplier(record, now)
}

fn temporal_multiplier(record: &MemoryRecord, now: DateTime<Utc>) -> f64 {
    if record.source_type != MemorySourceType::SystemDerived {
        return 1.0;
    }
    let timestamp = record
        .updated_at
        .as_deref()
        .or(record.last_confirmed_at.as_deref())
        .unwrap_or(&record.created_at);
    let Ok(created_at) = DateTime::parse_from_rfc3339(timestamp) else {
        // 历史数据格式无法证明时不衰减，避免误伤长期事实。
        return 1.0;
    };
    let age_days = now
        .signed_duration_since(created_at.with_timezone(&Utc))
        .num_seconds()
        .max(0) as f64
        / 86_400.0;
    2.0_f64.powf(-age_days / SYSTEM_DERIVED_HALF_LIFE_DAYS)
}

fn mmr_select(mut candidates: Vec<RankedMemory>, limit: usize) -> Vec<MemoryRecord> {
    if candidates.len() <= 1 {
        return candidates
            .into_iter()
            .take(limit)
            .map(|candidate| candidate.record)
            .collect();
    }
    let max_score = candidates
        .iter()
        .map(|candidate| candidate.score)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_score = candidates
        .iter()
        .map(|candidate| candidate.score)
        .fold(f64::INFINITY, f64::min);
    let score_range = (max_score - min_score).max(f64::EPSILON);
    let mut selected = Vec::<RankedMemory>::with_capacity(limit.min(candidates.len()));
    while !candidates.is_empty() && selected.len() < limit {
        let (best_index, _) = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let relevance = (candidate.score - min_score) / score_range;
                let redundancy = selected
                    .iter()
                    .map(|chosen| jaccard(&candidate.features, &chosen.features))
                    .fold(0.0_f64, f64::max);
                let score = MMR_LAMBDA * relevance - (1.0 - MMR_LAMBDA) * redundancy;
                (index, score)
            })
            .max_by(|left, right| left.1.partial_cmp(&right.1).unwrap_or(Ordering::Equal))
            .expect("non-empty candidates");
        selected.push(candidates.remove(best_index));
    }
    selected
        .into_iter()
        .map(|candidate| candidate.record)
        .collect()
}

fn dedup_key(record: &MemoryRecord) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        normalize_text(&record.content),
        record.memory_type,
        record.visibility.as_str(),
        record.attribute_key.as_deref().unwrap_or(""),
        record.relation_subject_id.as_deref().unwrap_or(""),
        record.relation_object_id.as_deref().unwrap_or("")
    )
}

pub(super) fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn text_features(text: &str) -> HashSet<String> {
    let normalized = normalize_text(text);
    let mut features = HashSet::new();
    let mut ascii_word = String::new();
    let mut non_ascii = Vec::new();
    for character in normalized.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            ascii_word.push(character);
            continue;
        }
        if !ascii_word.is_empty() {
            features.insert(std::mem::take(&mut ascii_word));
        }
        if character.is_alphanumeric() {
            non_ascii.push(character);
            features.insert(character.to_string());
        } else {
            non_ascii.clear();
        }
        if non_ascii.len() >= 2 {
            let pair = non_ascii[non_ascii.len() - 2..].iter().collect::<String>();
            features.insert(pair);
        }
    }
    if !ascii_word.is_empty() {
        features.insert(ascii_word);
    }
    features
}

fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    let union = left.len() + right.len() - intersection;
    intersection as f64 / union as f64
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, SecondsFormat};

    use super::*;
    use crate::runtime::tools::memory::storage::{MemoryKind, MemoryStatus, MemoryVisibility};

    fn record(id: &str, content: &str, source_type: MemorySourceType) -> MemoryRecord {
        MemoryRecord {
            id: id.to_owned(),
            ts: "2026-07-17T10:00:00+08:00".to_owned(),
            created_at: "2026-07-17T10:00:00+08:00".to_owned(),
            updated_at: None,
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
            scope_type: "personal".to_owned(),
            scope_id: Some("scope-a".to_owned()),
            created_by_user_id: Some("scope-a".to_owned()),
            memory_kind: MemoryKind::Personal,
            subject_id: None,
            relation_subject_id: None,
            relation_object_id: None,
            visibility: MemoryVisibility::Private,
            source_type,
            source_ref: None,
            last_confirmed_at: None,
            status: MemoryStatus::Active,
            pinned: false,
            attribute_key: None,
            user_id: None,
            group_id: None,
            content: content.to_owned(),
            source_text: String::new(),
        }
    }

    #[test]
    fn query_relevance_promotes_matching_memory() {
        let ranked = rank_layer(
            vec![
                record("new", "用户喜欢喝咖啡", MemorySourceType::UserConfirmed),
                record(
                    "match",
                    "Rust 项目使用 SQLite",
                    MemorySourceType::UserConfirmed,
                ),
            ],
            "这个 Rust 项目用什么数据库",
            2,
            Utc::now(),
        );
        assert_eq!(ranked[0].id, "match");
    }

    #[test]
    fn exact_duplicate_is_removed_but_distinct_relation_subjects_remain() {
        let mut duplicate = record("duplicate", "喜欢 Rust", MemorySourceType::UserConfirmed);
        duplicate.relation_subject_id = Some("actor-a".to_owned());
        let mut same_subject = duplicate.clone();
        same_subject.id = "same-subject".to_owned();
        let mut other_subject = duplicate.clone();
        other_subject.id = "other-subject".to_owned();
        other_subject.relation_subject_id = Some("actor-b".to_owned());

        let ranked = rank_layer(
            vec![duplicate, same_subject, other_subject],
            "Rust",
            10,
            Utc::now(),
        );
        assert_eq!(ranked.len(), 2);
        assert!(
            ranked
                .iter()
                .any(|record| { record.relation_subject_id.as_deref() == Some("actor-a") })
        );
        assert!(
            ranked
                .iter()
                .any(|record| { record.relation_subject_id.as_deref() == Some("actor-b") })
        );
    }

    #[test]
    fn only_system_derived_memory_decays() {
        let now = Utc::now();
        let old = now - Duration::days(60);
        let mut derived = record("derived", "相同主题", MemorySourceType::SystemDerived);
        derived.created_at = old.to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut confirmed = record("confirmed", "相同主题", MemorySourceType::UserConfirmed);
        confirmed.created_at = old.to_rfc3339_opts(SecondsFormat::Secs, true);

        assert!(temporal_multiplier(&derived, now) < 0.3);
        assert_eq!(temporal_multiplier(&confirmed, now), 1.0);
    }
}
