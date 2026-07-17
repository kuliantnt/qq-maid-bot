use super::qq::{QqMarkdownRenderer, qq_markdown_options};
use super::*;
use pulldown_cmark::{Event, Parser, Tag};

fn assert_no_reactivated_block_structure(markdown: &str) -> String {
    let rendered = to_qq(markdown);
    let unexpected = Parser::new_ext(&rendered, qq_markdown_options())
        .filter(|event| {
            matches!(
                event,
                Event::Start(Tag::Heading { .. } | Tag::List(_) | Tag::Item) | Event::Rule
            )
        })
        .map(|event| format!("{event:?}"))
        .collect::<Vec<_>>();

    assert!(
        unexpected.is_empty(),
        "rendered Markdown reactivated block structure: source={markdown:?}, rendered={rendered:?}, events={unexpected:?}"
    );
    rendered
}

#[test]
fn dynamic_markdown_helpers_share_escape_and_link_rules() {
    assert_eq!(
        escape_inline(" qq-maid_bot\nrelease "),
        r"qq\-maid\_bot release"
    );
    assert_eq!(
        escape_text("第一行 *强调*\n第二行 [链接]"),
        "第一行 \\*强调\\*  \n第二行 \\[链接\\]"
    );

    let destination = "https://github.com/kuliantnt/qq-maid-bot/releases.atom";
    assert_eq!(
        link("打开 [订阅源]", destination),
        r"[打开 \[订阅源\]](<https://github.com/kuliantnt/qq-maid-bot/releases.atom>)"
    );
    assert_eq!(
        to_chat_text(&link("打开订阅源", destination)),
        format!("打开订阅源（{destination}）")
    );
    assert_eq!(link("本地地址", "file:///tmp/feed.xml"), "本地地址");
    assert_eq!(
        link("异常地址", "https://example.test\nmalicious"),
        "异常地址"
    );
}

#[test]
fn plain_renderer_resolves_reference_links_and_omits_definitions() {
    let markdown = "## What's Changed\n\n* by [@maid][1] in [#414][2]\n\n[1]: https://example.test/maid\n[2]: https://example.test/pull/414";

    let text = to_plain_text(markdown);

    assert!(text.contains("What's Changed"));
    assert!(text.contains("• by @maid（https://example.test/maid）"));
    assert!(text.contains("#414（https://example.test/pull/414）"));
    assert!(!text.contains("[1]:"));
    assert!(!text.contains("[2]:"));
}

#[test]
fn plain_renderer_parses_escapes_but_preserves_code_literals() {
    let markdown = r"\#\# title \[codex\] qq\-maid\-bot `path\to\file`";

    let text = to_plain_text(markdown);

    assert_eq!(text, r"## title [codex] qq-maid-bot path\to\file");
}

#[test]
fn qq_renderer_keeps_headings_lists_inline_links_and_code() {
    let markdown = "## What's Changed\n\n* by [@maid][1] in [#414][2]\n\n`path\\to\\file`\n\n[1]: https://example.test/maid\n[2]: https://example.test/pull/414";

    let rendered = to_qq(markdown);

    assert!(rendered.starts_with("## What's Changed"));
    assert!(rendered.contains("- by [@maid](<https://example.test/maid>)"));
    assert!(rendered.contains("[#414](<https://example.test/pull/414>)"));
    assert!(rendered.contains(r"`path\to\file`"));
    assert!(!rendered.contains("[1]:"));
}

#[test]
fn qq_renderer_resolves_escapes_without_deleting_code_backslashes() {
    let markdown = r"\#\# title \[codex\] qq\-maid\-bot `C:\work\qq-maid`";

    let rendered = to_qq(markdown);

    assert_eq!(
        rendered,
        r"＃# title ［codex］ qq-maid-bot `C:\work\qq-maid`"
    );
    assert!(!rendered.contains(r"\#"));
    assert!(!rendered.contains(r"\["));
    assert!(!rendered.contains(r"\-"));
}

#[test]
fn qq_renderer_keeps_literal_markers_from_reactivating_structure() {
    let markdown = r"\#\# title

\* literal

正文含 \]\(、\`、\*、\_ 和 \[codex\]";

    let rendered = to_qq(markdown);

    assert!(rendered.starts_with("＃# title"));
    assert!(rendered.contains("＊ literal"));
    assert!(rendered.contains("正文含 ］(、｀、＊、＿ 和 ［codex］"));
    assert!(!rendered.contains("\n## title"));
    assert!(!rendered.contains("\n- literal"));
    assert!(!rendered.contains("]( "));
}

#[test]
fn qq_renderer_sanitizes_literal_block_markers_at_line_boundaries() {
    let markdown = r"正文
\-\-\-

标题
\=\=\=

\| a \| b \|

1\) 字面序号";

    let rendered = to_qq(markdown);

    assert!(rendered.contains("－--"));
    assert!(rendered.contains("＝=="));
    assert!(rendered.contains("｜ a ｜ b ｜"));
    assert!(rendered.contains("1） 字面序号"));
    assert!(!rendered.contains("\n---\n"));
    assert!(!rendered.contains("\n===\n"));
}

#[test]
fn qq_renderer_only_sanitizes_real_line_start_markers() {
    let markdown = r"2026-07-10 发布

127.0.0.1

3D rendering

\#408

\+86

\-webkit

1\. 项目

12\) 项目

\# 标题

\- 项目

\+ 项目";

    let rendered = to_qq(markdown);

    for literal in [
        "2026-07-10 发布",
        "127.0.0.1",
        "3D rendering",
        "#408",
        "+86",
        "-webkit",
    ] {
        assert!(rendered.contains(literal), "missing literal: {literal}");
    }
    assert!(rendered.contains("1． 项目"));
    assert!(rendered.contains("12） 项目"));
    assert!(rendered.contains("＃ 标题"));
    assert!(rendered.contains("－ 项目"));
    assert!(rendered.contains("＋ 项目"));
}

#[test]
fn pulldown_cmark_drops_continuation_indent_and_splits_ordered_escape() {
    for indent in [" ", "  ", "   "] {
        let markdown = format!(
            "正文\n{indent}\\# 字面标题\n{indent}\\- 字面列表\n{indent}1\\. 字面序号\n{indent}\\---\n{indent}\\==="
        );
        let events = Parser::new_ext(&markdown, qq_markdown_options()).collect::<Vec<_>>();
        let text = events
            .iter()
            .filter_map(|event| match event {
                Event::Text(text) => Some(text.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            text,
            [
                "正文",
                "# 字面标题",
                "- 字面列表",
                "1",
                ". 字面序号",
                "---",
                "===",
            ]
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::SoftBreak))
                .count(),
            5
        );
    }
}

#[test]
fn qq_renderer_does_not_reactivate_indented_literal_block_markers() {
    for indent in [" ", "  ", "   "] {
        for literal in [
            r"\# 字面标题",
            r"\- 字面列表",
            r"\+ 字面列表",
            r"1\. 字面序号",
            r"1\) 字面序号",
            r"\---",
            r"\===",
        ] {
            // 同时覆盖段落续行和空行后的独立块，后者可直接触发 thematic break。
            for separator in ["\n", "\n\n"] {
                let markdown = format!("正文{separator}{indent}{literal}");
                assert_no_reactivated_block_structure(&markdown);
            }
        }
    }
}

#[test]
fn qq_renderer_keeps_safe_literals_and_real_ast_blocks() {
    let literals = r"\#408

\+86

\-webkit

2026-07-10 发布

127.0.0.1

3D rendering";
    let rendered = assert_no_reactivated_block_structure(literals);
    for literal in [
        "#408",
        "+86",
        "-webkit",
        "2026-07-10 发布",
        "127.0.0.1",
        "3D rendering",
    ] {
        assert!(rendered.contains(literal), "missing literal: {literal}");
    }

    let structured = to_qq("# 真实标题\n\n- 第一项\n- 第二项\n\n1. 第三项");
    let events = Parser::new_ext(&structured, qq_markdown_options()).collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Start(Tag::Heading { .. })))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Start(Tag::List(None))))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Start(Tag::List(Some(1)))))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Start(Tag::Item)))
            .count(),
        3
    );
}

#[test]
fn qq_renderer_handles_ordered_markers_split_across_text_events() {
    let mut renderer = QqMarkdownRenderer::default();
    renderer.push(Event::Text("12".into()));
    renderer.push(Event::Text(") 项目".into()));

    assert_eq!(renderer.finish(), "12） 项目");

    let structured = to_qq("1. 第一项\n2. 第二项\n\n12) 第十二项");
    assert!(structured.contains("1. 第一项\n2. 第二项"));
    assert!(structured.contains("12. 第十二项"));
}

#[test]
fn qq_renderer_keeps_images_as_text_and_avoids_nested_links() {
    let standalone = to_qq("![封面](https://img.example.test/cover.png)");
    let linked =
        to_qq("[![封面](https://img.example.test/cover.png)](https://example.test/article)");
    let empty = to_qq("![](https://img.example.test/empty.png)");
    let linked_empty =
        to_qq("[![](https://img.example.test/empty.png)](https://example.test/article)");
    let unsafe_image = to_qq("![本地封面](file:///tmp/cover.png)");
    let unsafe_link = to_qq("[![封面](https://img.example.test/cover.png)](javascript:alert(1))");

    assert_eq!(standalone, "封面");
    assert_eq!(linked, "[封面](<https://example.test/article>)");
    assert_eq!(linked.matches("](<").count(), 1);
    assert!(!linked.contains("[["));
    assert_eq!(empty, "");
    assert_eq!(linked_empty, "");
    assert_eq!(unsafe_image, "本地封面");
    assert_eq!(unsafe_link, "封面");
}

#[test]
fn limited_renderer_does_not_leave_image_links_nested_or_unclosed() {
    let markdown = "前言 [![封面图片文字](https://img.example.test/cover.png)](https://example.test/article) 后续正文继续增长 ![尾图](https://img.example.test/end.png)";

    for limit in 1..=markdown.chars().count() {
        let rendered = to_qq_with_limit(markdown, limit);

        assert!(rendered.chars().count() <= limit);
        assert!(
            !rendered.contains("[["),
            "nested link at limit {limit}: {rendered}"
        );
        assert_eq!(
            rendered.matches("](<").count(),
            rendered.matches(">)").count(),
            "unclosed link at limit {limit}: {rendered}"
        );
        assert_eq!(
            rendered.matches('[').count(),
            rendered.matches("](<").count(),
            "dangling link opener at limit {limit}: {rendered}"
        );
        assert_eq!(
            rendered.matches(']').count(),
            rendered.matches("](<").count(),
            "dangling link closer at limit {limit}: {rendered}"
        );
        assert!(
            !rendered.contains("[]("),
            "empty link at limit {limit}: {rendered}"
        );
    }
}

#[test]
fn qq_renderer_sanitizes_literal_markers_after_list_line_breaks() {
    let markdown = "- 第一行  \n  \\# 字面标题  \n  \\- 字面列表";

    let rendered = to_qq(markdown);

    assert!(rendered.contains("\n＃ 字面标题"));
    assert!(rendered.contains("\n－ 字面列表"));
    assert!(!rendered.contains("\n# 字面标题"));
    assert!(!rendered.contains("\n- 字面列表"));
}

#[test]
fn qq_renderer_sanitizes_link_labels_without_changing_destination() {
    let markdown = r"[left \] middle \[ `code` and \` tick](https://example.test/a)";

    let rendered = to_qq(markdown);

    assert_eq!(
        rendered,
        "[left ］ middle ［ code and ｀ tick](<https://example.test/a>)"
    );
    assert_eq!(rendered.matches("](<").count(), 1);
}

#[test]
fn qq_renderer_uses_safe_delimiters_for_code_with_backticks() {
    let markdown = "````text\ninside ``` fence\n````\n\n``code ` tick``";

    let rendered = to_qq(markdown);

    assert!(rendered.starts_with("````text\ninside ``` fence\n````"));
    assert!(rendered.contains("``code ` tick``"));
}

#[test]
fn limited_renderer_does_not_cut_links_or_inline_code() {
    for markdown in [
        "前言 [codex](https://example.test/release) 后续正文继续增长",
        "前言 `cargo test --workspace` 后续正文继续增长",
    ] {
        let rendered = to_qq_with_limit(markdown, 18);

        assert!(rendered.chars().count() <= 18);
        assert!(rendered.ends_with('…'));
        assert_eq!(
            rendered.matches("[codex](<").count(),
            rendered.matches(">)").count()
        );
        assert_eq!(rendered.matches('`').count() % 2, 0);
    }
}

#[test]
fn limited_renderer_closes_fenced_code_and_respects_unicode_boundaries() {
    let markdown = "```rust\nfn main() {\n    println!(\"你好世界🙂再见\");\n}\n```\n\n末尾";

    let rendered = to_qq_with_limit(markdown, 24);

    assert!(rendered.chars().count() <= 24);
    assert!(rendered.contains('…'));
    assert_eq!(rendered.matches("```").count() % 2, 0);

    let chinese = to_qq_with_limit("你好世界🙂再见", 6);
    assert_eq!(chinese, "你好世界🙂…");
    assert_eq!(chinese.chars().count(), 6);
}
