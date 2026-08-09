//! Markdown 分块诊断，只输出匿名聚合数值，不记录正文或标题内容。

use std::collections::HashMap;

use super::chunking::MarkdownChunk;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ChunkDiagnostics {
    pub file_bytes: usize,
    pub file_chars: usize,
    pub chunk_count: usize,
    pub chunk_chars_min: usize,
    pub chunk_chars_avg: f64,
    pub chunk_chars_p50: usize,
    pub chunk_chars_p95: usize,
    pub chunk_chars_max: usize,
    pub chunks_with_heading: usize,
    pub chunks_without_heading: usize,
    pub heading_section_count: usize,
    pub heading_chunks_min: usize,
    pub heading_chunks_avg: f64,
    pub heading_chunks_p50: usize,
    pub heading_chunks_p95: usize,
    pub heading_chunks_max: usize,
}

pub(super) fn summarize_chunks(content: &str, chunks: &[MarkdownChunk]) -> ChunkDiagnostics {
    let mut chunk_chars = chunks
        .iter()
        .map(|chunk| chunk.body.chars().count())
        .collect::<Vec<_>>();
    let chunks_with_heading = chunks
        .iter()
        .filter(|chunk| chunk.heading_path.is_some())
        .count();
    let mut heading_counts = HashMap::<&str, usize>::new();
    for heading in chunks
        .iter()
        .filter_map(|chunk| chunk.heading_path.as_deref())
    {
        *heading_counts.entry(heading).or_default() += 1;
    }
    let mut heading_chunks = heading_counts.into_values().collect::<Vec<_>>();
    let chunk_distribution = Distribution::from_values(&mut chunk_chars);
    let heading_distribution = Distribution::from_values(&mut heading_chunks);

    ChunkDiagnostics {
        file_bytes: content.len(),
        file_chars: content.chars().count(),
        chunk_count: chunks.len(),
        chunk_chars_min: chunk_distribution.min,
        chunk_chars_avg: chunk_distribution.avg,
        chunk_chars_p50: chunk_distribution.p50,
        chunk_chars_p95: chunk_distribution.p95,
        chunk_chars_max: chunk_distribution.max,
        chunks_with_heading,
        chunks_without_heading: chunks.len().saturating_sub(chunks_with_heading),
        heading_section_count: heading_chunks.len(),
        heading_chunks_min: heading_distribution.min,
        heading_chunks_avg: heading_distribution.avg,
        heading_chunks_p50: heading_distribution.p50,
        heading_chunks_p95: heading_distribution.p95,
        heading_chunks_max: heading_distribution.max,
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Distribution {
    min: usize,
    avg: f64,
    p50: usize,
    p95: usize,
    max: usize,
}

impl Distribution {
    fn from_values(values: &mut [usize]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        values.sort_unstable();
        Self {
            min: values[0],
            avg: values.iter().sum::<usize>() as f64 / values.len() as f64,
            p50: percentile(values, 50),
            p95: percentile(values, 95),
            max: values[values.len() - 1],
        }
    }
}

fn percentile(sorted: &[usize], percentile: usize) -> usize {
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100).max(1);
    sorted[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tools::knowledge::index::chunking::chunk_markdown;

    #[test]
    fn large_markdown_reports_anonymous_chunk_statistics() {
        let mut content = String::from("# 大型分块诊断\n\n");
        for index in 0..1_200 {
            content.push_str(&format!(
                "## 章节 {index}\n\n第 {index} 节包含稳定的知识诊断文本，用于验证大型 Markdown 分块统计。\n\n"
            ));
        }

        let chunks = chunk_markdown("generated-large.md", &content);
        let diagnostics = summarize_chunks(&content, &chunks);
        tracing::debug!(?diagnostics, "大尺寸 Markdown 分块诊断");

        assert_eq!(diagnostics.file_bytes, content.len());
        assert_eq!(diagnostics.file_chars, content.chars().count());
        assert_eq!(diagnostics.chunk_count, chunks.len());
        assert!(diagnostics.chunk_count >= 1_200);
        assert_eq!(diagnostics.chunks_with_heading, diagnostics.chunk_count);
        assert_eq!(diagnostics.chunks_without_heading, 0);
        assert_eq!(diagnostics.heading_section_count, 1_200);
        assert!(diagnostics.chunk_chars_min <= diagnostics.chunk_chars_p50);
        assert!(diagnostics.chunk_chars_p50 <= diagnostics.chunk_chars_p95);
        assert!(diagnostics.chunk_chars_p95 <= diagnostics.chunk_chars_max);
        assert_eq!(diagnostics.heading_chunks_min, 1);
        assert_eq!(diagnostics.heading_chunks_max, 1);
    }
}
