use std::collections::HashSet;

use super::text::{build_index_text, hash_text};

// 修改分块正文语义时必须提升版本，确保 file_hash 未变的已索引知识文件也会重建。
pub(super) const CHUNKING_VERSION: i64 = 4;

const TARGET_CHUNK_CHARS: usize = 900;
const SOFT_CHUNK_CHARS: usize = 1200;
const HARD_CHUNK_CHARS: usize = 1600;
const CODE_CHUNK_CHARS: usize = 1400;
const CODE_CHUNK_LINES: usize = 48;

#[derive(Debug, Clone)]
pub(super) struct MarkdownChunk {
    pub chunk_id: String,
    pub relative_path: String,
    pub document_title: Option<String>,
    pub heading_path: Option<String>,
    pub chunk_index: usize,
    pub chunk_type: String,
    pub body: String,
    pub content_hash: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub code_language: Option<String>,
    pub search_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    List,
    Quote,
    Code,
    Table,
}

impl BlockKind {
    fn as_str(self) -> &'static str {
        match self {
            BlockKind::Text => "text",
            BlockKind::List => "list",
            BlockKind::Quote => "quote",
            BlockKind::Code => "code",
            BlockKind::Table => "table",
        }
    }
}

#[derive(Debug, Clone)]
struct MarkdownBlock {
    kind: BlockKind,
    text: String,
    start_line: usize,
    end_line: usize,
    document_title: Option<String>,
    heading_path: Option<String>,
    code_language: Option<String>,
}

pub(super) fn chunk_markdown(relative_path: &str, content: &str) -> Vec<MarkdownChunk> {
    let parsed = strip_frontmatter(content);
    ChunkEmitter::new(
        relative_path,
        parsed.metadata.search_terms,
        parsed.metadata.title,
    )
    .emit(parse_blocks(parsed.body, parsed.body_start_line))
}

fn parse_blocks(content: &str, start_line: usize) -> Vec<MarkdownBlock> {
    parse_blocks_with_title(content, start_line, None)
}

fn parse_blocks_with_title(
    content: &str,
    start_line: usize,
    document_title: Option<String>,
) -> Vec<MarkdownBlock> {
    let mut parser = BlockParser {
        document_title,
        ..BlockParser::default()
    };
    for (offset, line) in content.lines().enumerate() {
        parser.push_line(line, start_line + offset);
    }
    parser.finish()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FrontmatterMetadata {
    title: Option<String>,
    search_terms: Vec<String>,
}

#[derive(Debug, Clone)]
struct ParsedMarkdown<'a> {
    body: &'a str,
    body_start_line: usize,
    metadata: FrontmatterMetadata,
}

fn strip_frontmatter(content: &str) -> ParsedMarkdown<'_> {
    let (content, _had_bom) = content
        .strip_prefix('\u{feff}')
        .map(|rest| (rest, true))
        .unwrap_or((content, false));
    let Some(first_line_end) = line_end_index(content) else {
        return ParsedMarkdown {
            body: content,
            body_start_line: 1,
            metadata: FrontmatterMetadata::default(),
        };
    };
    if content[..first_line_end].trim_end_matches('\r') != "---" {
        return ParsedMarkdown {
            body: content,
            body_start_line: 1,
            metadata: FrontmatterMetadata::default(),
        };
    }

    let mut cursor = first_line_end + 1;
    let mut current_line = 2;
    while cursor <= content.len() {
        let line_end = line_end_index(&content[cursor..])
            .map(|index| cursor + index)
            .unwrap_or(content.len());
        let line = content[cursor..line_end].trim_end_matches('\r');
        if line == "---" {
            let body_start = if line_end < content.len() {
                line_end + 1
            } else {
                line_end
            };
            return ParsedMarkdown {
                body: &content[body_start..],
                body_start_line: current_line + 1,
                metadata: parse_frontmatter_metadata(&content[first_line_end + 1..cursor]),
            };
        }
        if line_end == content.len() {
            break;
        }
        cursor = line_end + 1;
        current_line += 1;
    }

    // 只有开头分隔符但没有闭合标记时按普通 Markdown 处理，避免误删整篇正文。
    ParsedMarkdown {
        body: content,
        body_start_line: 1,
        metadata: FrontmatterMetadata::default(),
    }
}

fn line_end_index(text: &str) -> Option<usize> {
    text.find('\n')
}

fn parse_frontmatter_metadata(yaml: &str) -> FrontmatterMetadata {
    let mut title = None;
    let mut terms = Vec::<String>::new();
    let mut active_list_key: Option<&str> = None;

    for raw_line in yaml.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(key) = active_list_key {
            if let Some(item) = parse_yaml_list_item(raw_line) {
                if matches!(key, "synonyms" | "aliases") {
                    push_metadata_term(&mut terms, &item);
                }
                continue;
            }
            active_list_key = None;
        }
        let Some((key, value)) = parse_yaml_pair(trimmed) else {
            continue;
        };
        if !matches!(key, "title" | "synonyms" | "aliases") {
            continue;
        }
        if value.is_empty() {
            active_list_key = Some(key);
            continue;
        }
        if let Some(items) = parse_inline_string_array(value) {
            for item in items {
                if key == "title" && title.is_none() {
                    title = Some(item.clone());
                }
                push_metadata_term(&mut terms, &item);
            }
            continue;
        }
        if let Some(value) = parse_yaml_string(value) {
            if key == "title" && title.is_none() {
                title = Some(value.clone());
            }
            push_metadata_term(&mut terms, &value);
        }
    }

    dedup_metadata_terms(&mut terms);
    FrontmatterMetadata {
        title: title.filter(|value| !value.trim().is_empty()),
        search_terms: terms,
    }
}

fn parse_yaml_pair(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    if key
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        Some((key, value.trim()))
    } else {
        None
    }
}

fn parse_yaml_list_item(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let value = trimmed.strip_prefix("- ")?;
    parse_yaml_string(value.trim())
}

fn parse_inline_string_array(value: &str) -> Option<Vec<String>> {
    let inner = value.strip_prefix('[')?.strip_suffix(']')?;
    let mut items = Vec::new();
    for item in inner.split(',') {
        if let Some(value) = parse_yaml_string(item.trim()) {
            items.push(value);
        }
    }
    Some(items)
}

fn parse_yaml_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || matches!(trimmed, "[]" | "{}") {
        return None;
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return None;
    }
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(trimmed)
        .trim();
    if unquoted.is_empty() || is_plain_yaml_scalar_non_string(unquoted) {
        None
    } else {
        Some(unquoted.to_owned())
    }
}

fn is_plain_yaml_scalar_non_string(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if matches!(lower.as_str(), "true" | "false" | "null" | "~") {
        return true;
    }
    value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok()
}

fn push_metadata_term(terms: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        terms.push(value.to_owned());
    }
}

fn dedup_metadata_terms(terms: &mut Vec<String>) {
    let mut seen = HashSet::<String>::new();
    terms.retain(|term| seen.insert(build_index_text(term)));
}

#[derive(Default)]
struct BlockParser {
    document_title: Option<String>,
    headings: Vec<(usize, String)>,
    current: Option<OpenBlock>,
    blocks: Vec<MarkdownBlock>,
    fence: Option<CodeFence>,
}

impl BlockParser {
    fn push_line(&mut self, line: &str, line_no: usize) {
        let trimmed = line.trim();
        if let Some(fence) = self.fence.clone() {
            self.push_current(line, line_no, BlockKind::Code, fence.language.clone());
            if fence.closes(trimmed) {
                self.flush_current();
                self.fence = None;
            }
            return;
        }
        if let Some(fence) = CodeFence::open(trimmed) {
            self.flush_current();
            self.fence = Some(fence.clone());
            self.push_current(line, line_no, BlockKind::Code, fence.language);
            return;
        }
        if is_markdown_heading(trimmed) {
            self.flush_current();
            self.set_heading(trimmed);
            return;
        }
        if trimmed.is_empty() {
            self.flush_current();
            return;
        }
        self.push_current(line, line_no, classify_line(trimmed), None);
    }

    fn push_current(
        &mut self,
        line: &str,
        line_no: usize,
        kind: BlockKind,
        code_language: Option<String>,
    ) {
        let heading_path = self.heading_path();
        let same_block = self.current.as_ref().is_some_and(|current| {
            current.kind == kind
                && current.heading_path == heading_path
                && (kind != BlockKind::Code || current.code_language == code_language)
        });
        if !same_block {
            self.flush_current();
            self.current = Some(OpenBlock {
                kind,
                text: String::new(),
                start_line: line_no,
                end_line: line_no,
                document_title: self.document_title.clone(),
                heading_path,
                code_language,
            });
        }
        let current = self.current.as_mut().expect("current block must exist");
        if !current.text.is_empty() {
            current.text.push('\n');
        }
        current.text.push_str(line);
        current.end_line = line_no;
    }

    fn flush_current(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        let text = current.text.trim().to_owned();
        if !has_retrievable_content(&text, current.kind) {
            return;
        }
        self.blocks.push(MarkdownBlock {
            kind: current.kind,
            text,
            start_line: current.start_line,
            end_line: current.end_line,
            document_title: current.document_title,
            heading_path: current.heading_path,
            code_language: current.code_language,
        });
    }

    fn set_heading(&mut self, line: &str) {
        let level = line.chars().take_while(|ch| *ch == '#').count();
        let title = line[level..].trim().trim_matches('#').trim().to_owned();
        if title.is_empty() {
            return;
        }
        if level == 1 && self.document_title.is_none() {
            self.document_title = Some(title.clone());
        }
        while self
            .headings
            .last()
            .is_some_and(|(current_level, _)| *current_level >= level)
        {
            self.headings.pop();
        }
        self.headings.push((level, title));
    }

    fn heading_path(&self) -> Option<String> {
        if self.headings.is_empty() {
            return None;
        }
        Some(
            self.headings
                .iter()
                .map(|(_, title)| title.as_str())
                .collect::<Vec<_>>()
                .join(" / "),
        )
    }

    fn finish(mut self) -> Vec<MarkdownBlock> {
        self.flush_current();
        self.blocks
    }
}

#[derive(Debug, Clone)]
struct OpenBlock {
    kind: BlockKind,
    text: String,
    start_line: usize,
    end_line: usize,
    document_title: Option<String>,
    heading_path: Option<String>,
    code_language: Option<String>,
}

#[derive(Debug, Clone)]
struct CodeFence {
    marker: String,
    language: Option<String>,
}

impl CodeFence {
    fn open(trimmed: &str) -> Option<Self> {
        let marker_char = trimmed.chars().next()?;
        if !matches!(marker_char, '`' | '~') {
            return None;
        }
        let marker_len = trimmed.chars().take_while(|ch| *ch == marker_char).count();
        if marker_len < 3 {
            return None;
        }
        let marker = marker_char.to_string().repeat(marker_len);
        let language = trimmed[marker_len..]
            .split_whitespace()
            .next()
            .filter(|item| !item.is_empty())
            .map(str::to_owned);
        Some(Self { marker, language })
    }

    fn closes(&self, trimmed: &str) -> bool {
        trimmed.starts_with(&self.marker)
    }
}

struct ChunkEmitter<'a> {
    relative_path: &'a str,
    frontmatter_terms: Vec<String>,
    frontmatter_title: Option<String>,
    chunks: Vec<MarkdownChunk>,
}

impl<'a> ChunkEmitter<'a> {
    fn new(
        relative_path: &'a str,
        frontmatter_terms: Vec<String>,
        frontmatter_title: Option<String>,
    ) -> Self {
        Self {
            relative_path,
            frontmatter_terms,
            frontmatter_title,
            chunks: Vec::new(),
        }
    }

    fn emit(mut self, blocks: Vec<MarkdownBlock>) -> Vec<MarkdownChunk> {
        let mut pending = Vec::<MarkdownBlock>::new();
        for block in blocks {
            if block.kind == BlockKind::Code && oversized_code_block(&block) {
                self.flush_pending(&mut pending);
                for split in split_code_block(&block) {
                    self.push_chunk(split.text.clone(), &split, BlockKind::Code);
                }
                continue;
            }
            if should_start_new_chunk(&pending, &block) {
                self.flush_pending(&mut pending);
            }
            pending.push(block);
        }
        self.flush_pending(&mut pending);
        self.chunks
    }

    fn flush_pending(&mut self, pending: &mut Vec<MarkdownBlock>) {
        if pending.is_empty() {
            return;
        }
        let body = pending
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let first = pending.first().expect("pending is not empty").clone();
        let last = pending.last().expect("pending is not empty");
        let kind = combined_kind(pending);
        let parts = if body.chars().count() > SOFT_CHUNK_CHARS && kind != BlockKind::Code {
            split_long_text(&body, HARD_CHUNK_CHARS)
        } else {
            vec![body]
        };
        for part in parts {
            let mut meta = first.clone();
            meta.text = part;
            meta.end_line = last.end_line;
            self.push_chunk(meta.text.clone(), &meta, kind);
        }
        pending.clear();
    }

    fn push_chunk(&mut self, body: String, block: &MarkdownBlock, kind: BlockKind) {
        let content_hash = hash_text(&body);
        let index = self.chunks.len();
        let short_hash = content_hash.chars().take(12).collect::<String>();
        let path_hash = hash_text(self.relative_path)
            .chars()
            .take(12)
            .collect::<String>();
        // chunk_id 在 storage 层有唯一约束，slug 只用于可读性；原始相对路径哈希用于避免
        // a-b.md / a/b.md 或中文文件名归一化后发生碰撞。
        let chunk_id = format!(
            "{}-{path_hash}:{index:04}:{short_hash}",
            stable_path_id(self.relative_path),
        );
        let mut searchable = String::new();
        searchable.push_str(self.relative_path);
        searchable.push('\n');
        let include_frontmatter_terms = should_attach_frontmatter_terms(block, kind);
        let document_title = block
            .document_title
            .clone()
            .or_else(|| self.frontmatter_title.clone());
        if let Some(title) = block.document_title.as_ref().or_else(|| {
            include_frontmatter_terms
                .then_some(())
                .and(self.frontmatter_title.as_ref())
        }) {
            searchable.push_str(title);
            searchable.push('\n');
        }
        if let Some(path) = &block.heading_path {
            searchable.push_str(path);
            searchable.push('\n');
        }
        if include_frontmatter_terms && !self.frontmatter_terms.is_empty() {
            // Frontmatter 只作为受控文档级检索信号进入正文 chunk；
            // 不保留 YAML 字段名、缩进和重复别名，避免元数据片段挤占 BM25 前排。
            searchable.push_str(&self.frontmatter_terms.join("\n"));
            searchable.push('\n');
        }
        searchable.push_str(&body);
        self.chunks.push(MarkdownChunk {
            chunk_id,
            relative_path: self.relative_path.to_owned(),
            document_title,
            heading_path: block.heading_path.clone(),
            chunk_index: index,
            chunk_type: kind.as_str().to_owned(),
            search_text: build_index_text(&searchable),
            body,
            content_hash,
            start_line: Some(block.start_line),
            end_line: Some(block.end_line),
            code_language: block.code_language.clone(),
        });
    }
}

fn should_attach_frontmatter_terms(block: &MarkdownBlock, kind: BlockKind) -> bool {
    block.heading_path.is_some()
        || matches!(kind, BlockKind::Text | BlockKind::List | BlockKind::Table)
}

fn should_start_new_chunk(pending: &[MarkdownBlock], next: &MarkdownBlock) -> bool {
    let Some(first) = pending.first() else {
        return false;
    };
    if first.heading_path != next.heading_path {
        return true;
    }
    let current_chars: usize = pending
        .iter()
        .map(|block| block.text.chars().count() + 2)
        .sum();
    current_chars >= TARGET_CHUNK_CHARS
        || current_chars + next.text.chars().count() > SOFT_CHUNK_CHARS
}

fn combined_kind(blocks: &[MarkdownBlock]) -> BlockKind {
    if blocks.len() == 1 {
        return blocks[0].kind;
    }
    if blocks.iter().any(|block| block.kind == BlockKind::Code) {
        BlockKind::Code
    } else if blocks.iter().any(|block| block.kind == BlockKind::Table) {
        BlockKind::Table
    } else {
        BlockKind::Text
    }
}

fn split_code_block(block: &MarkdownBlock) -> Vec<MarkdownBlock> {
    let lines = block.text.lines().collect::<Vec<_>>();
    if lines.len() <= 2 {
        return vec![block.clone()];
    }
    let opener = lines[0];
    let Some(fence) = CodeFence::open(opener.trim()) else {
        return vec![block.clone()];
    };
    let last_line = lines[lines.len() - 1];
    let closed = fence.closes(last_line.trim());
    // 未闭合 fenced code block 到 EOF 时，最后一行仍是真实代码内容；
    // 不能把它当作 closer，否则会被复制进每个切片并污染检索索引。
    let closer = if closed {
        last_line.to_owned()
    } else {
        fence.marker.clone()
    };
    let content = if closed {
        &lines[1..lines.len() - 1]
    } else {
        &lines[1..]
    };
    let mut blocks = Vec::new();
    let mut start = 0;
    while start < content.len() {
        let mut chars = opener.chars().count() + closer.chars().count() + 2;
        let mut end = start;
        while end < content.len() && end - start < CODE_CHUNK_LINES {
            let line_chars = content[end].chars().count() + 1;
            if end > start && chars + line_chars > CODE_CHUNK_CHARS {
                break;
            }
            chars += line_chars;
            end += 1;
        }
        let mut text = String::new();
        text.push_str(opener);
        text.push('\n');
        text.push_str(&content[start..end].join("\n"));
        text.push('\n');
        text.push_str(&closer);
        blocks.push(MarkdownBlock {
            text,
            start_line: block.start_line + start,
            end_line: block.start_line + end + 1,
            ..block.clone()
        });
        start = end;
    }
    blocks
}

fn oversized_code_block(block: &MarkdownBlock) -> bool {
    block.text.chars().count() > CODE_CHUNK_CHARS || block.text.lines().count() > CODE_CHUNK_LINES
}

fn split_long_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut rest = text.trim();
    while rest.chars().count() > max_chars {
        let cut = best_cut(rest, max_chars).unwrap_or_else(|| char_boundary_at(rest, max_chars));
        let (head, tail) = rest.split_at(cut);
        chunks.push(head.trim().to_owned());
        rest = tail.trim_start();
    }
    if !rest.is_empty() {
        chunks.push(rest.to_owned());
    }
    chunks
}

fn best_cut(text: &str, max_chars: usize) -> Option<usize> {
    let limit = char_boundary_at(text, max_chars);
    let search = &text[..limit];
    for pattern in [
        "\n\n", "\n- ", "\n* ", "\n1. ", "。", "？", "！", ". ", "? ", "! ", "；", "; ", "，", ", ",
    ] {
        if let Some(index) = search.rfind(pattern) {
            let cut = index + pattern.len();
            if cut > max_chars / 2 {
                return Some(cut);
            }
        }
    }
    None
}

fn char_boundary_at(text: &str, max_chars: usize) -> usize {
    text.char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

fn classify_line(trimmed: &str) -> BlockKind {
    if trimmed.starts_with('>') {
        BlockKind::Quote
    } else if trimmed.starts_with('|') && trimmed.ends_with('|') {
        BlockKind::Table
    } else if is_list_line(trimmed) {
        BlockKind::List
    } else {
        BlockKind::Text
    }
}

fn is_list_line(trimmed: &str) -> bool {
    trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("+ ")
        || trimmed
            .split_once(". ")
            .is_some_and(|(left, _)| !left.is_empty() && left.chars().all(|ch| ch.is_ascii_digit()))
}

fn has_retrievable_content(text: &str, kind: BlockKind) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if matches!(kind, BlockKind::Code | BlockKind::List | BlockKind::Table) {
        return true;
    }
    if trimmed.chars().count() >= 8 {
        return true;
    }
    // 短配置项、命令、路径和错误码可能是关键知识，不能只按长度丢弃。
    trimmed.contains('/')
        || trimmed.contains('=')
        || trimmed.contains('.')
        || trimmed.contains('_')
        || trimmed.chars().any(|ch| ch.is_ascii_digit())
}

fn is_markdown_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    (1..=6).contains(&hashes) && line.chars().nth(hashes).is_some_and(char::is_whitespace)
}

fn stable_path_id(path: &str) -> String {
    let slug = path
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if slug.is_empty() {
        "doc".to_owned()
    } else {
        slug
    }
}
