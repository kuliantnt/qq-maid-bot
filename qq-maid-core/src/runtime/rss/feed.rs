//! RSS / Atom 拉取、解析和安全清理。
//!
//! Feed 是不可信外部输入。本模块只返回普通文本字段，并在请求前拦截
//! localhost、内网、link-local 和云 metadata 等地址，避免把外部内容当成
//! 机器人指令或可信系统信息处理。

use std::{net::IpAddr, time::Duration};

use feed_rs::{model, parser};
use qq_maid_common::text::truncate_chars_with_ellipsis as truncate_chars;
use regex::Regex;
use reqwest::{StatusCode, redirect::Policy};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::lookup_host;
use url::Url;

use crate::storage::rss::RssFeedItem;

const DEFAULT_USER_AGENT: &str = "qq-maid-rss/0.1 (+https://github.com/kuliantnt/qqbot)";
const RSS_HTML_TEXT_WIDTH: usize = 4000;

#[derive(Debug, Clone)]
pub struct RssFetchConfig {
    pub timeout_seconds: u64,
    pub max_body_bytes: usize,
    pub user_agent: String,
    pub allow_private_networks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFeed {
    pub title: String,
    pub items: Vec<RssFeedItem>,
}

#[derive(Debug, Error)]
pub enum RssFeedError {
    #[error("URL 不能为空")]
    EmptyUrl,
    #[error("只支持 http/https RSS 地址")]
    UnsupportedScheme,
    #[error("RSS 地址不能包含用户名或密码")]
    UrlCredentials,
    #[error("RSS 地址缺少主机名")]
    MissingHost,
    #[error("RSS 地址指向本机、内网、link-local 或 metadata 地址")]
    UnsafeHost,
    #[error("DNS 解析失败：{0}")]
    Dns(String),
    #[error("HTTP 客户端初始化失败：{0}")]
    Client(String),
    #[error("RSS 请求失败：{0}")]
    Request(String),
    #[error("RSS 地址返回 HTTP {0}")]
    Status(StatusCode),
    #[error("RSS 响应体过大")]
    BodyTooLarge,
    #[error("RSS/Atom 解析失败：{0}")]
    Parse(String),
}

#[derive(Debug, Clone)]
pub struct RssFetcher {
    config: RssFetchConfig,
    client: reqwest::Client,
}

impl Default for RssFetchConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 15,
            max_body_bytes: 2 * 1024 * 1024,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            allow_private_networks: false,
        }
    }
}

impl RssFetcher {
    pub fn new(config: RssFetchConfig) -> Result<Self, RssFeedError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .redirect(Policy::limited(5))
            .user_agent(config.user_agent.clone())
            .build()
            .map_err(|err| RssFeedError::Client(err.to_string()))?;
        Ok(Self { config, client })
    }

    pub async fn fetch(&self, url: &str, summary_limit: usize) -> Result<ParsedFeed, RssFeedError> {
        let url = validate_feed_url(url, self.config.allow_private_networks).await?;
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(|err| RssFeedError::Request(reqwest_error_summary(&err)))?;
        let status = response.status();
        if !status.is_success() {
            return Err(RssFeedError::Status(status));
        }
        if response
            .content_length()
            .is_some_and(|len| len > self.config.max_body_bytes as u64)
        {
            return Err(RssFeedError::BodyTooLarge);
        }
        let bytes = read_limited_body(response, self.config.max_body_bytes).await?;
        parse_feed_bytes(&bytes, Some(url.as_str()), summary_limit)
    }
}

pub async fn validate_feed_url(raw: &str, allow_private: bool) -> Result<Url, RssFeedError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(RssFeedError::EmptyUrl);
    }
    let url = Url::parse(raw).map_err(|_| RssFeedError::UnsupportedScheme)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(RssFeedError::UnsupportedScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(RssFeedError::UrlCredentials);
    }
    let host = url.host_str().ok_or(RssFeedError::MissingHost)?;
    if is_blocked_hostname(host) && !allow_private {
        return Err(RssFeedError::UnsafeHost);
    }
    if allow_private {
        return Ok(url);
    }
    let port = url
        .port_or_known_default()
        .ok_or(RssFeedError::MissingHost)?;
    let addrs = lookup_host((host, port))
        .await
        .map_err(|err| RssFeedError::Dns(err.to_string()))?
        .collect::<Vec<_>>();
    if addrs.is_empty() || addrs.iter().any(|addr| is_blocked_ip(addr.ip())) {
        return Err(RssFeedError::UnsafeHost);
    }
    Ok(url)
}

pub fn parse_feed_bytes(
    bytes: &[u8],
    base_uri: Option<&str>,
    summary_limit: usize,
) -> Result<ParsedFeed, RssFeedError> {
    let feed = parser::Builder::new()
        // 缺失 ID 时不要让 feed-rs 生成随机 UUID；否则无 GUID/无链接的条目重启后会重复。
        .id_generator(|_, _, _| String::new())
        .base_uri(base_uri)
        .build()
        .parse(bytes)
        .map_err(|err| RssFeedError::Parse(err.to_string()))?;
    let title = feed
        .title
        .as_ref()
        .and_then(|text| clean_feed_text(&text.content))
        .unwrap_or_else(|| "未命名订阅".to_owned());
    let items = feed
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| normalize_entry(&feed, entry, index as i64, summary_limit))
        .collect::<Vec<_>>();
    Ok(ParsedFeed { title, items })
}

pub fn clean_summary_text(raw: &str, limit: usize) -> Option<String> {
    let without_scripts = strip_script_style(raw);
    // 摘要用于直接推送给 QQ，必须保留 feed 中原有的段落和列表换行。
    // 这里把 html2text 宽度放大，避免它按终端列宽插入额外软换行。
    let rendered = html2text::from_read(without_scripts.as_bytes(), RSS_HTML_TEXT_WIDTH)
        .unwrap_or(without_scripts);
    let clean = clean_multiline_text(&strip_markdown_emphasis(&rendered));
    if clean.is_empty() || is_placeholder_null(&clean) {
        None
    } else {
        Some(truncate_chars(&clean, limit))
    }
}

fn preferred_summary_text(
    summary: Option<&str>,
    full_content: Option<&str>,
    limit: usize,
) -> Option<String> {
    // feed-rs 会把 RSS description 和 Atom summary 映射到 summary，
    // 把 RSS content:encoded 映射到 content。RSS 聚合源常把 content:encoded
    // 放整篇文章，因此只有摘要字段缺失或清理后为空时才回退到正文。
    summary
        .and_then(|text| clean_summary_text(text, limit))
        .or_else(|| full_content.and_then(|text| clean_summary_text(text, limit)))
}

fn normalize_entry(
    feed: &model::Feed,
    entry: &model::Entry,
    source_order: i64,
    summary_limit: usize,
) -> RssFeedItem {
    let title = entry
        .title
        .as_ref()
        .and_then(|text| clean_feed_text(&text.content))
        .unwrap_or_else(|| "无标题".to_owned());
    let link = entry.links.first().map(|link| normalize_link(&link.href));
    let original_published_at = entry.published.map(|time| time.to_rfc3339());
    let original_updated_at = entry.updated.map(|time| time.to_rfc3339());
    let published_at = original_published_at
        .clone()
        .or_else(|| original_updated_at.clone());
    let updated_at = original_updated_at
        .clone()
        .or_else(|| original_published_at.clone());
    let raw_summary = entry.summary.as_ref().map(|text| text.content.as_str());
    let raw_content = entry
        .content
        .as_ref()
        .and_then(|content| content.body.as_deref());
    let summary = preferred_summary_text(raw_summary, raw_content, summary_limit);
    let item_key = stable_item_key(
        feed,
        entry,
        link.as_deref(),
        &title,
        original_published_at.as_deref(),
    );
    let revision_hash = revision_hash(updated_at.as_deref(), &title, raw_summary, raw_content);
    RssFeedItem {
        item_key,
        revision_hash,
        title,
        link,
        published_at,
        updated_at,
        summary,
        source_order,
    }
}

fn stable_item_key(
    feed: &model::Feed,
    entry: &model::Entry,
    link: Option<&str>,
    title: &str,
    published_at: Option<&str>,
) -> String {
    let entry_id = entry.id.trim();
    if !entry_id.is_empty() {
        return format!("id:{entry_id}");
    }
    if let Some(link) = link.filter(|value| !value.trim().is_empty()) {
        return format!("link:{link}");
    }
    let feed_title = feed
        .title
        .as_ref()
        .and_then(|text| clean_feed_text(&text.content))
        .unwrap_or_default();
    let fallback_source = format!(
        "{}|{}|{}",
        feed_title,
        title.trim(),
        published_at.unwrap_or("")
    );
    format!("fallback:{}", sha256_hex(&fallback_source))
}

fn normalize_link(raw: &str) -> String {
    let raw = raw.trim();
    if let Ok(mut url) = Url::parse(raw) {
        url.set_fragment(None);
        return url.to_string();
    }
    raw.to_owned()
}

async fn read_limited_body(
    mut response: reqwest::Response,
    max_body_bytes: usize,
) -> Result<Vec<u8>, RssFeedError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| RssFeedError::Request(reqwest_error_summary(&err)))?
    {
        if body.len() + chunk.len() > max_body_bytes {
            return Err(RssFeedError::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn is_blocked_hostname(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "metadata.google.internal"
        || host == "metadata"
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.octets() == [169, 254, 169, 254]
        }
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

fn strip_script_style(raw: &str) -> String {
    let script = Regex::new("(?is)<script[^>]*>.*?</script>").expect("valid script regex");
    let style = Regex::new("(?is)<style[^>]*>.*?</style>").expect("valid style regex");
    let without_script = script.replace_all(raw, " ");
    style.replace_all(&without_script, " ").to_string()
}

fn clean_text(raw: &str) -> String {
    let no_control = raw
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t' | '\r'))
        .collect::<String>();
    no_control.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_multiline_text(raw: &str) -> String {
    let no_control = raw
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t' | '\r'))
        .collect::<String>();
    let normalized = no_control.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::new();
    let mut previous_blank = false;
    for line in normalized.lines() {
        let clean = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if clean.is_empty() {
            if !lines.is_empty() && !previous_blank {
                lines.push(String::new());
                previous_blank = true;
            }
        } else {
            lines.push(clean);
            previous_blank = false;
        }
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn clean_feed_text(raw: &str) -> Option<String> {
    let text = clean_text(raw);
    if text.is_empty() || is_placeholder_null(&text) {
        None
    } else {
        Some(text)
    }
}

fn clean_revision_text(raw: &str) -> Option<String> {
    let without_scripts = strip_script_style(raw);
    let rendered = html2text::from_read(without_scripts.as_bytes(), RSS_HTML_TEXT_WIDTH)
        .unwrap_or(without_scripts);
    let clean =
        canonicalize_revision_text(&clean_multiline_text(&strip_markdown_emphasis(&rendered)));
    if clean.is_empty() || is_placeholder_null(&clean) {
        None
    } else {
        Some(clean)
    }
}

fn canonicalize_revision_text(raw: &str) -> String {
    let mut output = Vec::new();
    let mut bullet_group = Vec::new();
    for line in raw.lines() {
        let clean = line.trim();
        if let Some(item) = unordered_bullet_item(clean) {
            bullet_group.push(item);
        } else {
            flush_bullet_group(&mut output, &mut bullet_group);
            output.push(clean.to_owned());
        }
    }
    flush_bullet_group(&mut output, &mut bullet_group);
    output.join("\n")
}

fn flush_bullet_group(output: &mut Vec<String>, bullet_group: &mut Vec<String>) {
    if bullet_group.is_empty() {
        return;
    }
    // RSS/Atom 中的无序列表顺序经常由源站生成逻辑决定；revision hash 只关心集合内容，
    // 避免 Statuspage 这类 feed 因组件列表顺序抖动而被误判为新更新。
    bullet_group.sort_by_key(|item| item.to_ascii_lowercase());
    output.extend(bullet_group.drain(..).map(|item| format!("* {item}")));
}

fn unordered_bullet_item(line: &str) -> Option<String> {
    for marker in ["* ", "- ", "+ ", "• "] {
        if let Some(value) = line.strip_prefix(marker) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn is_placeholder_null(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "null" | "none" | "undefined"
    )
}

fn strip_markdown_emphasis(raw: &str) -> String {
    raw.replace("**", "").replace("__", "").replace('`', "")
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn revision_hash(
    updated_at: Option<&str>,
    title: &str,
    summary: Option<&str>,
    content: Option<&str>,
) -> String {
    // revision 只包含 feed 自身内容，不能混入抓取时间，否则同内容重复轮询会误判更新。
    let input = [
        ("updated", updated_at.unwrap_or("").trim().to_owned()),
        ("title", clean_feed_text(title).unwrap_or_default()),
        (
            "summary",
            summary.and_then(clean_revision_text).unwrap_or_default(),
        ),
        (
            "content",
            content.and_then(clean_revision_text).unwrap_or_default(),
        ),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}\0{value}"))
    .collect::<Vec<_>>()
    .join("\0");
    sha256_hex(&input)
}

fn reqwest_error_summary(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timeout".to_owned()
    } else if error.is_connect() {
        "connect failed".to_owned()
    } else if error.is_decode() {
        "decode failed".to_owned()
    } else {
        "request failed".to_owned()
    }
}

#[cfg(test)]
mod tests {
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
    fn html_summary_is_cleaned() {
        assert_eq!(
            clean_summary_text("<p>一 <b>二</b></p><style>x</style>", 20).as_deref(),
            Some("一 二")
        );
    }

    #[test]
    fn html_summary_preserves_block_line_breaks() {
        assert_eq!(
            clean_summary_text(
                "<p>Status: Resolved</p><p>Affected components</p><ul><li>Files (Operational)</li><li>Search (Operational)</li></ul>",
                500,
            )
            .as_deref(),
            Some("Status: Resolved\n\nAffected components\n* Files (Operational)\n* Search (Operational)")
        );
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
}
