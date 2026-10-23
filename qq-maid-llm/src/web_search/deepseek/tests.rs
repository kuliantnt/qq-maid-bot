use super::*;
use crate::config::{OpenAiApiMode, ProviderMode};
use crate::provider::types::ModelRoute;
use crate::web_search::{WebSearchBackend, build_web_search_executor};
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

fn request() -> WebSearchRequest {
    WebSearchRequest {
        query: "Rust site:rust-lang.org".to_owned(),
        raw_question: Some("不应发送的原始上下文".to_owned()),
        max_results: Some(10),
        context_size: None,
        topic: None,
        time_range: None,
        backend_override: Some(WebSearchBackend::DeepSeek),
        model_override: None,
    }
}

async fn server(status: u16, body: Value) -> (LlmConfig, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let app = Router::new().route(
        "/v1/tools/search",
        post(move |headers: HeaderMap, Json(payload): Json<Value>| {
            let captured = captured.clone();
            let body = body.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer test-key");
                captured.lock().await.push(payload);
                (StatusCode::from_u16(status).unwrap(), Json(body))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = LlmConfig {
        deepseek_api_key: Some("test-key".to_owned()),
        deepseek_base_url: base.clone(),
        ..config(base.clone(), false)
    };
    (config, requests)
}

#[tokio::test]
async fn routed_search_uses_independent_api_and_summary_without_conversation() {
    let (mut config, captured) = server(200, json!({"results":[
        {"title":"Rust", "url":"https://rust-lang.org", "snippet":"短摘要", "summary":"模型读取的长摘要"},
        {"title":"unsafe", "url":"javascript:alert(1)", "snippet":"", "summary":""}
    ]})).await;
    config.deepseek_base_url.push_str("/v1/");
    config.web_search.max_results = 3;
    let executor = build_web_search_executor(&config).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let outcome = executor.query_stream(request(), tx).await.unwrap();
    assert_eq!(outcome.provider, "deepseek");
    assert_eq!(outcome.sources.len(), 1);
    assert_eq!(outcome.sources[0].snippet, "模型读取的长摘要");
    // answer 必须带真实检索摘要，而不只是参考来源链接。
    assert!(outcome.answer.contains("模型读取的长摘要"));
    assert!(outcome.answer.contains("https://rust-lang.org"));
    // 非流式上游仍通过公共默认实现输出最终文本。
    assert!(rx.recv().await.is_some());
    assert_eq!(
        captured.lock().await[0],
        json!({"query":"Rust site:rust-lang.org", "count":3, "need_content":false})
    );
}

#[tokio::test]
async fn answer_keeps_each_retrieval_summary_within_display_budget() {
    let summaries = (0..5)
        .map(|index| format!("第 {index} 条检索摘要|{}", "内容".repeat(300)))
        .collect::<Vec<_>>();
    let body = json!({
        "results": summaries.iter().enumerate().map(|(index, summary)| json!({
            "title": format!("来源 {index}"),
            "url": format!("https://example.test/{index}"),
            "snippet": "短摘要",
            "summary": summary,
        })).collect::<Vec<_>>()
    });
    let (config, _) = server(200, body).await;
    let outcome = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap();

    assert_eq!(outcome.sources.len(), 5);
    for (index, summary) in summaries.iter().enumerate() {
        let head = summary.chars().take(20).collect::<String>();
        assert!(outcome.answer.contains(&head), "missing summary {index}");
        // 参考来源链接仍完整保留在 answer 和结构化 sources 中。
        assert!(
            outcome
                .answer
                .contains(&format!("https://example.test/{index}"))
        );
        // 结构化 sources 保留完整摘要，展示层的截断只作用于 answer。
        assert_eq!(&outcome.sources[index].snippet, summary);
    }
    // 摘要正文受总预算约束，`/查` 的 1500 字符展示上限仍能容纳参考来源。
    assert!(outcome.answer.chars().count() < 1_500);
}

#[tokio::test]
async fn errors_and_empty_results_are_distinct() {
    for (status, code) in [
        (401, "authentication_failed"),
        (402, "quota_exhausted"),
        (429, "rate_limited"),
        (503, "upstream_unavailable"),
    ] {
        let (config, _) = server(status, json!({"error":{"message":"upstream failure"}})).await;
        let err = DeepSeekSearchExecutor::new(&config)
            .unwrap()
            .query(request())
            .await
            .unwrap_err();
        assert_eq!(err.code, code);
        assert!(err.message.contains("upstream failure"));
    }
    let (config, _) = server(200, json!({"results":[]})).await;
    let out = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap();
    assert!(out.sources.is_empty() && out.answer.is_empty());
    let (config, _) = server(200, json!({})).await;
    assert!(
        DeepSeekSearchExecutor::new(&config)
            .unwrap()
            .query(request())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validates_unicode_length_and_missing_key_before_network() {
    let (config, captured) = server(200, json!({"results":[]})).await;
    let executor = DeepSeekSearchExecutor::new(&config).unwrap();
    for query in [" ".to_owned(), "搜".repeat(129)] {
        let err = executor
            .query(WebSearchRequest { query, ..request() })
            .await
            .unwrap_err();
        assert_eq!(err.code, "bad_request");
    }
    assert!(captured.lock().await.is_empty());
    executor
        .query(WebSearchRequest {
            query: "搜".repeat(128),
            ..request()
        })
        .await
        .unwrap();
    let config = LlmConfig {
        deepseek_api_key: None,
        ..config
    };
    let err = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap_err();
    assert_eq!(err.code, "web_search_not_configured");
    // Core 依靠 stage 前缀区分 DeepSeek 和 Tavily 的缺 Key 文案。
    assert_eq!(err.stage, "deepseek_search_config");
}

fn config(base_url: String, stream: bool) -> LlmConfig {
    let route = ModelRoute::parse_config("deepseek:model-under-test", "test").unwrap();
    LlmConfig {
        provider: ProviderMode::DeepSeek,
        model_route: route.clone(),
        configured_model_routes: vec![("test".to_owned(), route)],
        web_search: WebSearchConfig {
            default_backend: WebSearchBackend::ProviderNative,
            default_model: "deepseek:model-under-test".to_owned(),
            ..Default::default()
        },
        tavily_api_key: None,
        openai_api_key: None,
        openai_base_url: None,
        openai_api_mode: OpenAiApiMode::ChatOnly,
        deepseek_api_key: Some("test-deepseek-key".to_owned()),
        deepseek_base_url: base_url,
        deepseek_model: "deepseek:model-under-test".to_owned(),
        bigmodel_api_key: None,
        bigmodel_base_url: "https://example.test".to_owned(),
        bigmodel_model: "unused".to_owned(),
        gemini_api_key: None,
        gemini_base_url: "https://example.test".to_owned(),
        gemini_model: "unused".to_owned(),
        openai_compatible_providers: Vec::new(),
        openai_responses_providers: Vec::new(),
        stream,
        request_timeout_seconds: 5,
        media_max_bytes: 1024,
        max_output_tokens: 1200,
    }
}

#[tokio::test]
async fn first_response_timeout_is_bounded() {
    let app = Router::new().route(
        "/v1/tools/search",
        post(|| async {
            tokio::time::sleep(Duration::from_millis(1_200)).await;
            Json(json!({"results": []}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = config(base, false);
    config.web_search.connect_timeout_seconds = 1;
    config.web_search.first_response_timeout_seconds = 1;
    let err = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap_err();
    assert_eq!(err.code, "timeout");
    assert_eq!(err.stage, "deepseek_search_first_response");
}

#[tokio::test]
async fn upstream_errors_redact_credentials() {
    let (config, _) = server(401, json!({"error":{"message":"invalid key test-key"}})).await;
    let err = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap_err();
    assert!(!err.message.contains("test-key"));
    assert!(err.message.contains("<redacted>"));
}

/// 校验正文里每个 Markdown 行内链接都完整：`](` 后必须在同一行遇到 `)`，
/// 且以 `- [` 开头的来源行必须整行闭合。
fn assert_no_broken_markdown_links(answer: &str) {
    for (offset, _) in answer.match_indices("](") {
        let tail = &answer[offset + 2..];
        let Some(end) = tail.find(')') else {
            panic!("链接缺少右括号: {}", &answer[offset.saturating_sub(60)..]);
        };
        assert!(!tail[..end].contains('\n'), "链接被截断: {}", &tail[..end]);
    }
    for line in answer.lines().filter(|line| line.starts_with("- [")) {
        assert!(line.ends_with(')'), "来源行被截断: {line}");
    }
}

fn source_link_lines(answer: &str) -> Vec<&str> {
    answer
        .lines()
        .filter(|line| line.starts_with("- ["))
        .collect()
}

fn summary_entries(answer: &str) -> Vec<&str> {
    answer
        .lines()
        .filter(|line| line.starts_with(|ch: char| ch.is_ascii_digit()))
        .collect()
}

fn long_title(index: usize) -> String {
    format!("结果 {index} 标题{}🚀", "超长标题".repeat(48))
}

#[test]
fn answer_budget_keeps_links_whole_with_long_titles_summaries_and_urls() {
    // 标题接近 200 字符、URL 超过 200 字符、摘要远超单条上限，且含中文、Emoji 和
    // Markdown 特殊字符：这是最容易产生残缺链接和摘要断层的组合。
    let sources = (0..8)
        .map(|index| WebSearchSource {
            title: long_title(index),
            url: format!("https://example.test/{index}/{}", "a".repeat(200)),
            snippet: format!(
                "结果 {index} 摘要{}[内嵌](https://inner.test/{index})😀",
                "公开内容".repeat(80)
            ),
        })
        .collect::<Vec<_>>();

    let answer = render_answer(&sources);

    assert!(
        answer.chars().count() <= 1_400,
        "answer 超出预算: {}",
        answer.chars().count()
    );
    // `/查` 的 1500 字符上限包含「【联网查询】\n\n」前缀，因此下游不能再整体截断。
    assert!(
        answer.chars().count() + "【联网查询】\n\n".chars().count() <= 1_500,
        "answer 与 /查 前缀合计超出展示上限"
    );
    assert_no_broken_markdown_links(&answer);

    // 前几条长标题、长链接不能挤掉后续结果：每条有摘要的结果都保留有意义摘要。
    let entries = summary_entries(&answer);
    assert_eq!(entries.len(), 8);
    for (index, entry) in entries.iter().enumerate() {
        let (_, summary) = entry.split_once('：').expect("摘要条目缺少分隔符");
        assert!(
            summary.chars().count() >= 30,
            "第 {index} 条摘要过短: {summary}"
        );
        assert!(
            summary.contains("公开内容"),
            "第 {index} 条摘要丢失: {summary}"
        );
    }

    // 参考来源链接完整展示，超预算来源只省略展示、不进入残缺状态。
    let links = source_link_lines(&answer);
    assert!(links.len() >= 2, "参考来源链接不足: {}", links.len());
    for (index, link) in links.iter().enumerate() {
        assert!(
            sources[index].url.is_empty() || link.contains(&sources[index].url),
            "来源与链接不对应: {link}"
        );
    }
    // 结构化 sources 不受正文预算影响。
    assert_eq!(sources.len(), 8);
    assert_eq!(sources[7].title, long_title(7));
}

#[test]
fn long_leading_titles_do_not_crowd_out_later_result_summaries() {
    let sources = (0..6)
        .map(|index| WebSearchSource {
            title: if index < 2 {
                "很长的标题".repeat(40)
            } else {
                format!("来源 {index}")
            },
            url: format!("https://example.test/{index}"),
            snippet: format!("第 {index} 条摘要：{}", "可核实内容".repeat(30)),
        })
        .collect::<Vec<_>>();

    let answer = render_answer(&sources);

    assert!(answer.chars().count() <= 1_400);
    assert_no_broken_markdown_links(&answer);
    let entries = summary_entries(&answer);
    assert_eq!(entries.len(), 6, "后续结果不应被前部超长标题挤掉");
    for (index, entry) in entries.iter().enumerate() {
        let (prefix, summary) = entry.split_once('：').expect("摘要条目缺少分隔符");
        assert!(
            prefix.chars().count() <= 70,
            "展示标题未按上限截断: {prefix}"
        );
        assert!(
            summary.chars().count() >= 30,
            "第 {index} 条摘要过短: {summary}"
        );
    }
}

#[tokio::test]
async fn oversized_url_link_is_omitted_while_structured_source_remains() {
    // 单条 URL 超过整个展示预算，无论怎么压缩摘要都无法完整展示：整条链接省略。
    let long_url = format!("https://example.test/segment/{}", "b".repeat(1_500));
    let body = json!({"results":[
        {"title":"长链接来源", "url":long_url, "snippet":"长链接摘要", "summary":"长链接摘要"},
        {"title":"正常来源", "url":"https://example.test/normal", "snippet":"正常摘要", "summary":"正常摘要"}
    ]});
    let (config, _) = server(200, body).await;
    let outcome = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap();

    assert_eq!(outcome.sources.len(), 2);
    assert_eq!(outcome.sources[0].url, long_url);
    assert_eq!(outcome.sources[0].snippet, "长链接摘要");
    assert_no_broken_markdown_links(&outcome.answer);
    assert!(
        !outcome.answer.contains(&long_url),
        "超长链接必须整条省略，不能截断"
    );
    assert!(outcome.answer.contains("https://example.test/normal"));
    assert!(outcome.answer.contains("长链接摘要") && outcome.answer.contains("正常摘要"));
}

#[test]
fn sources_without_summary_keep_links_without_empty_entries() {
    let sources = vec![
        WebSearchSource {
            title: "有摘要".to_owned(),
            url: "https://example.test/a".to_owned(),
            snippet: "第一段摘要内容".to_owned(),
        },
        WebSearchSource {
            title: "无摘要".to_owned(),
            url: "https://example.test/b".to_owned(),
            snippet: "   ".to_owned(),
        },
        WebSearchSource {
            title: "有摘要二".to_owned(),
            url: "https://example.test/c".to_owned(),
            snippet: "第三段摘要内容".to_owned(),
        },
    ];

    let answer = render_answer(&sources);

    assert_no_broken_markdown_links(&answer);
    let entries = summary_entries(&answer);
    assert_eq!(entries.len(), 2, "没有摘要的结果不生成空条目");
    assert!(entries[0].contains("第一段摘要内容"));
    assert!(entries[1].contains("第三段摘要内容"));
    assert!(!answer.contains("无摘要："));
    // 来源链接不受摘要缺失影响，三条都能完整展示。
    assert_eq!(source_link_lines(&answer).len(), 3);
    assert!(answer.contains("https://example.test/b"));
}

#[test]
fn summary_markdown_links_are_never_cut_in_half() {
    // 官方摘要里自带 Markdown 链接时，按条预算可能正好落在链接内部。
    let long_url = format!("https://example.test/{}", "c".repeat(120));
    let sources = (0..10)
        .map(|index| WebSearchSource {
            title: format!("来源 {index}"),
            url: format!("https://example.test/{index}"),
            snippet: format!("说明 {index} [详细资料]({long_url}) 后续文字"),
        })
        .collect::<Vec<_>>();

    let answer = render_answer(&sources);

    assert!(answer.chars().count() <= 1_400);
    assert_no_broken_markdown_links(&answer);
    let entries = summary_entries(&answer);
    assert_eq!(entries.len(), 10);
    for (index, entry) in entries.iter().enumerate() {
        let (_, summary) = entry.split_once('：').expect("摘要条目缺少分隔符");
        assert!(summary.contains("说明"), "第 {index} 条摘要丢失: {summary}");
        assert!(
            !summary.contains("](http"),
            "第 {index} 条摘要出现残缺链接: {summary}"
        );
        assert!(summary.ends_with('…'), "第 {index} 条摘要未截断: {summary}");
    }
}

#[test]
fn markdown_special_characters_stay_escaped_and_links_complete() {
    let sources = vec![WebSearchSource {
        title: "标题[含]方括号\\与反斜杠".to_owned(),
        url: "https://example.test/wiki/Rust_(programming_language)".to_owned(),
        snippet: "摘要 [内嵌](https://example.test/inner) 😀 `code` *粗*".to_owned(),
    }];

    let answer = render_answer(&sources);

    assert_no_broken_markdown_links(&answer);
    assert!(answer.contains("\\[含\\]"));
    // 链接标签只转义一次，不能因为再次转义而破坏标题可读性。
    assert!(
        answer.contains("- [标题\\[含\\]方括号\\\\与反斜杠](https://example.test/wiki/Rust_(programming_language))"),
        "来源链接标签转义异常: {answer}"
    );
    // URL 中的成对圆括号属于合法链接，不能被当成 Markdown 结构截断。
    assert!(answer.contains("https://example.test/wiki/Rust_(programming_language)"));
    assert!(answer.contains("😀"));
}
