use super::*;

const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>测试 RSS</title>
    <item>
      <title>第一篇</title>
      <link>https://example.test/a#frag</link>
      <guid>guid-a</guid>
      <pubDate>Wed, 17 Jun 2026 08:00:00 GMT</pubDate>
      <description><![CDATA[<p>Hello <b>RSS</b></p><script>alert(1)</script>]]></description>
    </item>
  </channel>
</rss>"#;

const ATOM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>测试 Atom</title>
  <entry>
    <id>tag:example.test,2026:a</id>
    <title>Atom 第一篇</title>
    <link href="https://example.test/atom-a"/>
    <updated>2026-06-17T08:00:00Z</updated>
    <summary type="html">&lt;p&gt;Hello Atom&lt;/p&gt;</summary>
  </entry>
</feed>"#;

#[test]
fn parses_rss_2_feed() {
    let feed = parse_feed_bytes(RSS.as_bytes(), None, 120).unwrap();
    assert_eq!(feed.title, "测试 RSS");
    assert_eq!(feed.items.len(), 1);
    assert_eq!(feed.items[0].title, "第一篇");
    assert_eq!(feed.items[0].item_key, "id:guid-a");
    assert_eq!(
        feed.items[0].link.as_deref(),
        Some("https://example.test/a")
    );
    assert_eq!(feed.items[0].summary.as_deref(), Some("Hello RSS"));
}

#[test]
fn parses_atom_feed() {
    let feed = parse_feed_bytes(ATOM.as_bytes(), None, 120).unwrap();
    assert_eq!(feed.title, "测试 Atom");
    assert_eq!(feed.items[0].item_key, "id:tag:example.test,2026:a");
    assert!(feed.items[0].published_at.is_some());
    assert!(feed.items[0].updated_at.is_some());
}

#[test]
fn guid_and_revision_hash_are_stable_for_same_content() {
    let feed = parse_feed_bytes(RSS.as_bytes(), None, 120).unwrap();
    let first = feed.items[0].revision_hash.clone();
    let second = parse_feed_bytes(RSS.as_bytes(), None, 120).unwrap().items[0].clone();
    assert_eq!(feed.items[0].item_key, second.item_key);
    assert_eq!(first, second.revision_hash);
}

#[test]
fn fallback_item_key_is_stable_without_guid_or_link() {
    let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>源</title><item><title>无链接</title><pubDate>Wed, 17 Jun 2026 08:00:00 GMT</pubDate></item></channel></rss>"#;
    let a = parse_feed_bytes(xml.as_bytes(), None, 120).unwrap().items[0].clone();
    let b = parse_feed_bytes(xml.as_bytes(), None, 120).unwrap().items[0].clone();
    assert!(a.item_key.starts_with("fallback:"));
    assert_eq!(a.item_key, b.item_key);
    assert_eq!(a.revision_hash, b.revision_hash);
}

#[test]
fn atom_same_entry_id_changes_revision_when_status_updates() {
    let investigating = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Status</title>
  <entry>
    <id>tag:example.test,2026:incident-1</id>
    <title>Incident</title>
    <updated>2026-06-17T08:00:00Z</updated>
    <summary type="html">&lt;p&gt;Investigating&lt;/p&gt;</summary>
  </entry>
</feed>"#;
    let resolved = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Status</title>
  <entry>
    <id>tag:example.test,2026:incident-1</id>
    <title>Incident</title>
    <updated>2026-06-17T09:00:00Z</updated>
    <summary type="html">&lt;p&gt;Resolved&lt;/p&gt;</summary>
  </entry>
</feed>"#;

    let first = parse_feed_bytes(investigating.as_bytes(), None, 120)
        .unwrap()
        .items[0]
        .clone();
    let second = parse_feed_bytes(resolved.as_bytes(), None, 120)
        .unwrap()
        .items[0]
        .clone();

    assert_eq!(first.item_key, second.item_key);
    assert_ne!(first.revision_hash, second.revision_hash);
    assert_ne!(first.updated_at, second.updated_at);
}

#[test]
fn statuspage_component_order_does_not_change_revision() {
    let first = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Status</title>
  <entry>
    <id>https://status.example.test/incidents/incident-1</id>
    <title>Incident</title>
    <updated>2026-06-17T08:00:00Z</updated>
    <summary type="html">
      &lt;p&gt;Status: Resolved&lt;/p&gt;
      &lt;p&gt;Affected components&lt;/p&gt;
      &lt;ul&gt;&lt;li&gt;Files (Operational)&lt;/li&gt;&lt;li&gt;Search (Operational)&lt;/li&gt;&lt;/ul&gt;
    </summary>
  </entry>
</feed>"#;
    let reordered = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Status</title>
  <entry>
    <id>https://status.example.test/incidents/incident-1</id>
    <title>Incident</title>
    <updated>2026-06-17T08:00:00Z</updated>
    <summary type="html">
      &lt;p&gt;Status: Resolved&lt;/p&gt;
      &lt;p&gt;Affected components&lt;/p&gt;
      &lt;ul&gt;&lt;li&gt;Search (Operational)&lt;/li&gt;&lt;li&gt;Files (Operational)&lt;/li&gt;&lt;/ul&gt;
    </summary>
  </entry>
</feed>"#;

    let first_item = parse_feed_bytes(first.as_bytes(), None, 500).unwrap().items[0].clone();
    let reordered_item = parse_feed_bytes(reordered.as_bytes(), None, 500)
        .unwrap()
        .items[0]
        .clone();

    assert_eq!(first_item.item_key, reordered_item.item_key);
    assert_eq!(first_item.updated_at, reordered_item.updated_at);
    assert_eq!(first_item.revision_hash, reordered_item.revision_hash);
}

#[test]
fn placeholder_null_titles_and_summaries_are_ignored() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>null</title>
  <entry>
    <id>tag:example.test,2026:null-title</id>
    <title>null</title>
    <updated>2026-06-17T08:00:00Z</updated>
    <summary>null</summary>
  </entry>
</feed>"#;

    let feed = parse_feed_bytes(xml.as_bytes(), None, 120).unwrap();

    assert_eq!(feed.title, "未命名订阅");
    assert_eq!(feed.items[0].title, "无标题");
    assert_eq!(feed.items[0].summary, None);
}

#[test]
fn rss_title_only_comes_from_title_field() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Release notes from qq-maid-bot</title>
  <entry>
    <id>tag:example.test,2026:release</id>
    <title>v0.14.2</title>
    <link href="https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2"/>
    <updated>2026-07-08T08:00:00Z</updated>
    <summary type="html">&lt;p&gt;What's Changed&lt;/p&gt;&lt;p&gt;CPA 传输协议最终回答要求：不应进入标题。&lt;/p&gt;</summary>
    <content type="html">&lt;article&gt;&lt;p&gt;cpa_final_answer&lt;/p&gt;&lt;p&gt;tool_call&lt;/p&gt;&lt;/article&gt;</content>
  </entry>
</feed>"#;

    let feed = parse_feed_bytes(xml.as_bytes(), None, 500).unwrap();

    assert_eq!(feed.items[0].title, "v0.14.2");
    assert!(
        feed.items[0]
            .summary
            .as_deref()
            .unwrap()
            .contains("What's Changed")
    );
    assert!(!feed.items[0].title.contains("CPA 传输协议最终回答要求"));
    assert!(!feed.items[0].title.contains("cpa_final_answer"));
    assert!(!feed.items[0].title.contains("tool_call"));
}

#[test]
fn rss_title_sanitizes_newlines_and_markdown_chars() {
    let title = sanitize_rss_title(" v0.14.2\n[测试](link) ", 240).unwrap();

    assert_eq!(title, "v0.14.2 [测试](link)");
    assert!(!title.contains('\n'));
}

#[test]
fn html_summary_is_cleaned() {
    assert_eq!(
        clean_summary_text("<p>一 <b>二</b></p><style>x</style>", 20).as_deref(),
        Some("一 二")
    );
}

#[test]
fn html_summary_preserves_markdown_blocks() {
    assert_eq!(
            clean_summary_text(
                "<p>Status: Resolved</p><p>Affected components</p><ul><li>Files (Operational)</li><li>Search (Operational)</li></ul>",
                500,
            )
            .as_deref(),
            Some("Status: Resolved\n\nAffected components\n\n- Files (Operational)\n- Search (Operational)")
        );
}

#[test]
fn github_release_notes_render_as_qq_markdown_without_reference_definitions() {
    let html = r#"
<h2>What's Changed</h2>
<ul>
  <li>docs: 重构 README by <a href="https://github.com/kuliantnt">@kuliantnt</a> in <a href="https://github.com/kuliantnt/qq-maid-bot/pull/408">#408</a></li>
  <li>[codex] 修复 qq-maid-bot 推送 in <a href="https://github.com/kuliantnt/qq-maid-bot/pull/409">#409</a></li>
</ul>
"#;

    let summary = clean_summary_text(html, 1000).unwrap();

    assert!(summary.starts_with("## What's Changed\n\n- docs: 重构 README"));
    assert!(summary.contains("[@kuliantnt](<https://github.com/kuliantnt>)"));
    assert!(summary.contains("[#408](<https://github.com/kuliantnt/qq-maid-bot/pull/408>)"));
    assert!(summary.contains("- ［codex］ 修复 qq-maid-bot 推送"));
    assert!(!summary.contains("[1]:"));
    assert!(!summary.contains("\\#"));
    assert!(!summary.contains("\\["));
    assert!(!summary.contains("\\-"));
}

#[test]
fn markdown_escapes_are_parsed_instead_of_globally_removed() {
    let summary = clean_summary_text(
        r#"\#\# What's Changed

\* \[codex\] 修复 qq\-maid\-bot"#,
        500,
    )
    .unwrap();

    assert!(summary.contains("＃# What's Changed"));
    assert!(summary.contains("＊ ［codex］ 修复 qq-maid-bot"));
}

#[test]
fn html_code_block_preserves_backslashes_and_special_characters() {
    let summary = clean_summary_text(
        r#"<pre><code class="language-text">C:\work\qq-maid\config_[prod].toml</code></pre>"#,
        500,
    )
    .unwrap();

    assert!(summary.starts_with('`') && summary.ends_with('`'));
    assert!(summary.contains(r"C:\work\qq-maid\config_[prod].toml"));
    assert!(!summary.contains(r"C:workqq-maid"));
}

#[test]
fn description_wins_over_content_encoded_full_article() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>日报</title>
    <item>
      <title>今日汇总</title>
      <link>https://example.test/daily</link>
      <guid>daily-1</guid>
      <description><![CDATA[<p>短摘要优先</p>]]></description>
      <content:encoded><![CDATA[<article><p>全文第一段，不应在 description 存在时进入推送。</p><p>全文第二段。</p></article>]]></content:encoded>
    </item>
  </channel>
</rss>"#;

    let feed = parse_feed_bytes(xml.as_bytes(), None, 500).unwrap();

    assert_eq!(feed.items[0].summary.as_deref(), Some("短摘要优先"));
}

#[test]
fn atom_summary_wins_over_full_content() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom 周报</title>
  <entry>
    <id>tag:example.test,2026:weekly</id>
    <title>Atom 周报</title>
    <link href="https://example.test/weekly"/>
    <updated>2026-06-17T08:00:00Z</updated>
    <summary type="html">&lt;p&gt;Atom 短摘要&lt;/p&gt;</summary>
    <content type="html">&lt;article&gt;&lt;p&gt;Atom 完整正文，不应覆盖摘要。&lt;/p&gt;&lt;/article&gt;</content>
  </entry>
</feed>"#;

    let feed = parse_feed_bytes(xml.as_bytes(), None, 500).unwrap();

    assert_eq!(feed.items[0].summary.as_deref(), Some("Atom 短摘要"));
}

#[test]
fn daily_full_content_is_cleaned_and_limited_when_summary_missing() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>长正文日报</title>
    <item>
      <title>日报 2026-06-17</title>
      <link>https://example.test/daily-long</link>
      <guid>daily-long-1</guid>
      <content:encoded><![CDATA[
        <article>
          <style>.hidden { display: none; }</style>
          <script>alert("ignore")</script>
          <p>完整正文第一段。第二段包含很多中文内容，需要被限制长度，不能完整推送。</p>
          <p>末尾正文不应出现。</p>
        </article>
      ]]></content:encoded>
    </item>
  </channel>
</rss>"#;

    let feed = parse_feed_bytes(xml.as_bytes(), None, 18).unwrap();
    let summary = feed.items[0].summary.as_deref().unwrap();

    assert_eq!(summary.chars().count(), 18);
    assert!(summary.ends_with('…'));
    assert!(summary.starts_with("完整正文第一段"));
    assert!(!summary.contains("alert"));
    assert!(!summary.contains("末尾正文"));
}

#[test]
fn html_summary_strips_script_style_and_extra_whitespace() {
    assert_eq!(
            clean_summary_text(
                "<section><style>p{color:red}</style><p>第一段</p><script>bad()</script><p>第二段</p></section>",
                500,
            )
            .as_deref(),
            Some("第一段\n\n第二段")
        );
}

#[test]
fn unicode_summary_truncates_at_char_boundary() {
    let summary = clean_summary_text("你好世界🙂再见", 6).unwrap();

    assert_eq!(summary, "你好世界🙂…");
    assert_eq!(summary.chars().count(), 6);
}

#[test]
fn summary_limit_keeps_markdown_structures_closed() {
    let mut saw_ellipsis = false;
    for raw in [
        "前言 [发布说明](https://example.test/releases/416) 后续正文继续增长",
        "前言 `cargo test --workspace` 后续正文继续增长",
        "```rust\nfn main() {\n    println!(\"long code\");\n}\n```\n\n后续正文",
    ] {
        let summary = clean_summary_text(raw, 22).unwrap();

        assert!(summary.chars().count() <= 22);
        saw_ellipsis |= summary.contains('…');
        assert_eq!(
            summary.matches("](<").count(),
            summary.matches(">)").count()
        );
        assert_eq!(summary.matches("```").count() % 2, 0);
    }
    assert!(saw_ellipsis);
}

#[tokio::test]
async fn blocks_private_and_metadata_urls() {
    assert!(
        validate_feed_url("http://localhost/feed.xml", false)
            .await
            .is_err()
    );
    assert!(
        validate_feed_url("http://127.0.0.1/feed.xml", false)
            .await
            .is_err()
    );
    assert!(
        validate_feed_url("http://169.254.169.254/latest/meta-data", false)
            .await
            .is_err()
    );
}
