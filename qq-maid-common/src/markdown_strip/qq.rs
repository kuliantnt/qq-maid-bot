use super::{ensure_line_break, ensure_paragraph_break, push_paragraph_break};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// 将外部 Markdown 解析后重渲染为 QQ 主动消息使用的稳定子集。
///
/// 保留标题、无序/有序列表、HTTP(S) 内联链接、行内代码和围栏代码块；引用、
/// 强调、表格等 QQ 展示不稳定的结构只降级对应局部内容。解析器会解析引用式链接
/// 和合法反斜杠转义，因此不会把 `[1]: URL` 或 `\\#` 一类中间表示泄漏到消息中。
pub fn render_markdown_for_qq(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, qq_markdown_options());
    let mut renderer = QqMarkdownRenderer::default();

    for event in parser {
        renderer.push(event);
    }

    renderer.finish()
}

/// 在 Unicode 字符预算内安全渲染 QQ Markdown。
///
/// 长度限制作用于 Markdown 源片段，并在每次候选截断后重新解析，绝不直接截断
/// 已生成的链接或代码语法。优先使用解析事件边界；单个纯文本节点本身过长时，
/// 才退化到字符边界，并由 renderer 重新闭合当前结构。
pub fn render_markdown_for_qq_with_limit(markdown: &str, limit: usize) -> String {
    let rendered = render_markdown_for_qq(markdown);
    if rendered.chars().count() <= limit {
        return rendered;
    }
    if limit == 0 {
        return String::new();
    }

    let max_source_end = markdown
        .char_indices()
        .nth(limit)
        .map_or(markdown.len(), |(index, _)| index);
    let mut boundaries = Parser::new_ext(markdown, qq_markdown_options())
        .into_offset_iter()
        .map(|(_, range)| range.end)
        .filter(|&end| {
            end <= max_source_end && end < markdown.len() && markdown.is_char_boundary(end)
        })
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();

    for end in boundaries.into_iter().rev() {
        if let Some(candidate) = render_truncated_markdown(&markdown[..end], limit) {
            return candidate;
        }
    }

    let mut ends = markdown
        .char_indices()
        .map(|(index, _)| index)
        .take(limit.saturating_add(1))
        .collect::<Vec<_>>();
    ends.push(max_source_end);
    for end in ends.into_iter().rev() {
        if let Some(candidate) = render_truncated_markdown(&markdown[..end], limit) {
            return candidate;
        }
    }

    "…".to_owned()
}

pub(super) fn qq_markdown_options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH
}

fn render_truncated_markdown(prefix: &str, limit: usize) -> Option<String> {
    let prefix = prefix.trim_end();
    let source = if prefix.is_empty() {
        "…".to_owned()
    } else {
        format!("{prefix}…")
    };
    let rendered = render_markdown_for_qq(&source);
    (rendered.chars().count() <= limit).then_some(rendered)
}

#[derive(Debug, Default)]
pub(super) struct QqMarkdownRenderer {
    output: String,
    lists: Vec<Option<u64>>,
    links: Vec<LinkFrame>,
    images: Vec<String>,
    // 普通文本的行首标记可能跨 Text 事件，需等到满足 CommonMark 条件后再决定是否降级。
    line_start_prefix: String,
    in_item: usize,
    code_block: Option<CodeBlockBuffer>,
}

#[derive(Debug)]
struct LinkFrame {
    destination: Option<String>,
    output_start: usize,
}

#[derive(Debug)]
struct CodeBlockBuffer {
    language: String,
    content: String,
}

impl QqMarkdownRenderer {
    pub(super) fn push(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(code) => self.push_inline_code(&code),
            Event::SoftBreak | Event::HardBreak => {
                if let Some(code_block) = self.code_block.as_mut() {
                    code_block.content.push('\n');
                } else if let Some(image_alt) = self.images.last_mut() {
                    image_alt.push(' ');
                } else if !self.links.is_empty() {
                    self.output.push(' ');
                } else {
                    self.flush_line_start_prefix(true);
                    ensure_line_break(&mut self.output);
                }
            }
            Event::Rule => {
                self.flush_line_start_prefix(true);
                push_paragraph_break(&mut self.output);
            }
            Event::TaskListMarker(checked) => {
                self.flush_line_start_prefix(false);
                self.output.push_str(if checked { "[x] " } else { "[ ] " });
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => self.push_text(&text),
            Event::Html(_) | Event::InlineHtml(_) | Event::FootnoteReference(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if self.in_item == 0 {
                    ensure_paragraph_break(&mut self.output);
                }
            }
            Tag::Heading { level, .. } => {
                self.flush_line_start_prefix(false);
                ensure_paragraph_break(&mut self.output);
                self.output.push_str(heading_prefix(level));
                self.output.push(' ');
            }
            Tag::List(start) => {
                self.flush_line_start_prefix(false);
                if self.lists.is_empty() {
                    ensure_paragraph_break(&mut self.output);
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush_line_start_prefix(false);
                ensure_line_break(&mut self.output);
                self.output
                    .push_str(&"  ".repeat(self.lists.len().saturating_sub(1)));
                match self.lists.last_mut() {
                    Some(Some(next)) => {
                        self.output.push_str(&format!("{next}. "));
                        *next += 1;
                    }
                    _ => self.output.push_str("- "),
                }
                self.in_item += 1;
            }
            Tag::Link { dest_url, .. } => {
                let destination = safe_markdown_link(&dest_url);
                if destination.is_some() {
                    self.flush_line_start_prefix(false);
                    let output_start = self.output.len();
                    self.output.push('[');
                    self.links.push(LinkFrame {
                        destination,
                        output_start,
                    });
                } else {
                    self.links.push(LinkFrame {
                        destination,
                        output_start: self.output.len(),
                    });
                }
            }
            Tag::Image { .. } => self.images.push(String::new()),
            Tag::CodeBlock(kind) => {
                self.flush_line_start_prefix(false);
                let language = if let CodeBlockKind::Fenced(language) = kind {
                    language
                        .chars()
                        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '+'))
                        .collect::<String>()
                } else {
                    String::new()
                };
                self.code_block = Some(CodeBlockBuffer {
                    language,
                    content: String::new(),
                });
            }
            Tag::Table(_) => {
                self.flush_line_start_prefix(false);
                ensure_paragraph_break(&mut self.output);
            }
            Tag::TableHead | Tag::TableRow => {
                self.flush_line_start_prefix(false);
                ensure_line_break(&mut self.output);
            }
            Tag::TableCell => {
                self.flush_line_start_prefix(false);
                if !self.output.ends_with('\n') && !self.output.ends_with(" / ") {
                    self.output.push_str(" / ");
                }
            }
            Tag::BlockQuote(_)
            | Tag::HtmlBlock
            | Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::FootnoteDefinition(_)
            | Tag::MetadataBlock(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line_start_prefix(true);
                if self.in_item == 0 {
                    push_paragraph_break(&mut self.output);
                }
            }
            TagEnd::Heading(_) => {
                self.flush_line_start_prefix(true);
                push_paragraph_break(&mut self.output);
            }
            TagEnd::Item => {
                self.flush_line_start_prefix(true);
                self.in_item = self.in_item.saturating_sub(1);
                ensure_line_break(&mut self.output);
            }
            TagEnd::List(_) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    push_paragraph_break(&mut self.output);
                }
            }
            TagEnd::Link => {
                if let Some(LinkFrame {
                    destination: Some(destination),
                    output_start,
                }) = self.links.pop()
                {
                    if self.output.len() == output_start + 1 {
                        self.output.truncate(output_start);
                    } else {
                        self.output.push_str("](<");
                        self.output.push_str(&destination);
                        self.output.push_str(">)");
                    }
                }
            }
            TagEnd::Image => {
                if let Some(alt) = self.images.pop() {
                    // 图片只保留 alt；若位于链接内，alt 会自然成为外层链接的唯一标签。
                    let alt = alt.trim();
                    if !alt.is_empty() {
                        self.push_text(alt);
                    }
                }
            }
            TagEnd::CodeBlock => {
                if let Some(code_block) = self.code_block.take() {
                    ensure_paragraph_break(&mut self.output);
                    let fence_len = longest_backtick_run(&code_block.content)
                        .saturating_add(1)
                        .max(3);
                    let fence = "`".repeat(fence_len);
                    self.output.push_str(&fence);
                    self.output.push_str(&code_block.language);
                    self.output.push('\n');
                    self.output.push_str(&code_block.content);
                    if !self.output.ends_with('\n') {
                        self.output.push('\n');
                    }
                    self.output.push_str(&fence);
                    push_paragraph_break(&mut self.output);
                }
            }
            TagEnd::Table => {
                self.flush_line_start_prefix(true);
                push_paragraph_break(&mut self.output);
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                self.flush_line_start_prefix(true);
                ensure_line_break(&mut self.output);
            }
            TagEnd::TableCell
            | TagEnd::BlockQuote(_)
            | TagEnd::HtmlBlock
            | TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::FootnoteDefinition
            | TagEnd::MetadataBlock(_)
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
    }

    pub(super) fn finish(mut self) -> String {
        self.flush_line_start_prefix(true);
        while self.output.ends_with('\n') {
            self.output.pop();
        }
        self.output
    }

    fn push_text(&mut self, text: &str) {
        if let Some(image_alt) = self.images.last_mut() {
            image_alt.push_str(text);
            return;
        }
        if let Some(code_block) = self.code_block.as_mut() {
            code_block.content.push_str(text);
            return;
        }

        let in_link_label = self
            .links
            .last()
            .is_some_and(|link| link.destination.is_some());
        let chars = text.chars().collect::<Vec<_>>();
        for (index, &ch) in chars.iter().enumerate() {
            if ch == '\n' {
                self.flush_line_start_prefix(true);
                self.output.push('\n');
                continue;
            }

            if !self.line_start_prefix.is_empty()
                || self.output.is_empty()
                || self.output.ends_with('\n')
            {
                self.line_start_prefix.push(ch);
                match classify_line_start_prefix(&self.line_start_prefix, false) {
                    PrefixClassification::Pending => continue,
                    PrefixClassification::Safe => self.flush_line_start_prefix(false),
                    PrefixClassification::MarkdownMarker => self.flush_line_start_prefix(true),
                }
                continue;
            }

            let previous = index
                .checked_sub(1)
                .and_then(|offset| chars.get(offset).copied());
            let next = chars.get(index + 1).copied();
            self.output
                .push(safe_literal_char(ch, previous, next, in_link_label));
        }
    }

    fn flush_line_start_prefix(&mut self, at_line_end: bool) {
        if self.line_start_prefix.is_empty() {
            return;
        }

        let classification = classify_line_start_prefix(&self.line_start_prefix, at_line_end);
        let in_link_label = self
            .links
            .last()
            .is_some_and(|link| link.destination.is_some());
        let chars = self.line_start_prefix.chars().collect::<Vec<_>>();
        for (index, &ch) in chars.iter().enumerate() {
            let previous = index
                .checked_sub(1)
                .and_then(|offset| chars.get(offset).copied());
            let next = chars.get(index + 1).copied();
            let safe = if classification == PrefixClassification::MarkdownMarker {
                safe_markdown_marker_char(&chars, index, ch)
            } else {
                None
            }
            .unwrap_or_else(|| safe_literal_char(ch, previous, next, in_link_label));
            self.output.push(safe);
        }
        self.line_start_prefix.clear();
    }

    fn push_inline_code(&mut self, code: &str) {
        if !self.images.is_empty() {
            self.push_text(code);
            return;
        }
        self.flush_line_start_prefix(false);
        if !self.links.is_empty() {
            self.push_text(code);
            return;
        }
        let delimiter = "`".repeat(longest_backtick_run(code).saturating_add(1).max(1));
        let needs_padding = code.starts_with(['`', ' ']) || code.ends_with(['`', ' ']);
        self.output.push_str(&delimiter);
        if needs_padding {
            self.output.push(' ');
        }
        self.output.push_str(code);
        if needs_padding {
            self.output.push(' ');
        }
        self.output.push_str(&delimiter);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrefixClassification {
    Pending,
    Safe,
    MarkdownMarker,
}

fn classify_line_start_prefix(prefix: &str, at_line_end: bool) -> PrefixClassification {
    let chars = prefix.chars().collect::<Vec<_>>();
    let Some(&first) = chars.first() else {
        return PrefixClassification::Safe;
    };

    match first {
        '#' => {
            let marker_len = chars.iter().take_while(|&&ch| ch == '#').count();
            if marker_len > 6 {
                return PrefixClassification::Safe;
            }
            match chars.get(marker_len) {
                Some(ch) if ch.is_whitespace() => PrefixClassification::MarkdownMarker,
                Some(_) => PrefixClassification::Safe,
                None if at_line_end => PrefixClassification::MarkdownMarker,
                None => PrefixClassification::Pending,
            }
        }
        '-' => {
            if chars.get(1).is_some_and(|ch| ch.is_whitespace()) {
                return PrefixClassification::MarkdownMarker;
            }
            if chars.iter().all(|ch| *ch == '-' || ch.is_whitespace()) {
                if at_line_end {
                    if chars.iter().filter(|&&ch| ch == '-').count() >= 3 {
                        PrefixClassification::MarkdownMarker
                    } else {
                        PrefixClassification::Safe
                    }
                } else {
                    PrefixClassification::Pending
                }
            } else {
                PrefixClassification::Safe
            }
        }
        '+' => match chars.get(1) {
            Some(ch) if ch.is_whitespace() => PrefixClassification::MarkdownMarker,
            Some(_) => PrefixClassification::Safe,
            None if at_line_end => PrefixClassification::MarkdownMarker,
            None => PrefixClassification::Pending,
        },
        '=' => {
            if chars.iter().all(|ch| *ch == '=' || ch.is_whitespace()) {
                if at_line_end {
                    PrefixClassification::MarkdownMarker
                } else {
                    PrefixClassification::Pending
                }
            } else {
                PrefixClassification::Safe
            }
        }
        ch if ch.is_ascii_digit() => {
            let digit_len = chars.iter().take_while(|ch| ch.is_ascii_digit()).count();
            if digit_len > 9 {
                return PrefixClassification::Safe;
            }
            let Some(delimiter) = chars.get(digit_len) else {
                return if at_line_end {
                    PrefixClassification::Safe
                } else {
                    PrefixClassification::Pending
                };
            };
            if !matches!(delimiter, '.' | ')') {
                return PrefixClassification::Safe;
            }
            match chars.get(digit_len + 1) {
                Some(ch) if ch.is_whitespace() => PrefixClassification::MarkdownMarker,
                Some(_) => PrefixClassification::Safe,
                None if at_line_end => PrefixClassification::MarkdownMarker,
                None => PrefixClassification::Pending,
            }
        }
        _ => PrefixClassification::Safe,
    }
}

fn safe_markdown_marker_char(chars: &[char], index: usize, ch: char) -> Option<char> {
    match chars.first().copied() {
        Some('#') if index == 0 => Some('＃'),
        Some('-') if index == 0 => Some('－'),
        Some('+') if index == 0 => Some('＋'),
        Some('=') if index == 0 => Some('＝'),
        Some(first) if first.is_ascii_digit() && matches!(ch, '.' | ')') => {
            Some(if ch == '.' { '．' } else { '）' })
        }
        _ => None,
    }
}

fn safe_literal_char(
    ch: char,
    previous: Option<char>,
    next: Option<char>,
    in_link_label: bool,
) -> char {
    match ch {
        '\\' => '＼',
        '`' => '｀',
        '[' => '［',
        ']' => '］',
        '<' => '＜',
        '>' => '＞',
        '*' => '＊',
        '_' if previous.is_some_and(char::is_alphanumeric)
            && next.is_some_and(char::is_alphanumeric) =>
        {
            '_'
        }
        '_' => '＿',
        '~' => '～',
        '|' => '｜',
        _ if in_link_label && matches!(ch, '(' | ')') => match ch {
            '(' => '（',
            ')' => '）',
            _ => unreachable!(),
        },
        _ => ch,
    }
}

fn longest_backtick_run(text: &str) -> usize {
    text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0)
}

fn heading_prefix(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "#",
        HeadingLevel::H2 => "##",
        HeadingLevel::H3 => "###",
        HeadingLevel::H4 | HeadingLevel::H5 | HeadingLevel::H6 => "###",
    }
}

fn safe_markdown_link(destination: &str) -> Option<String> {
    let destination = destination.trim();
    let lower = destination.to_ascii_lowercase();
    (!destination.is_empty() && (lower.starts_with("https://") || lower.starts_with("http://")))
        .then(|| destination.replace(['\n', '\r', '<', '>'], ""))
}
