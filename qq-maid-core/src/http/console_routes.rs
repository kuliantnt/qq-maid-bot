//! Web 控制台静态资源、状态摘要、Markdown 预览和安全响应头。
//!
//! 静态资源统一使用重新验证式缓存：构建产物不带内容哈希、URL 不含版本号，
//! 固定文件名一旦被重建覆盖，长期 `immutable` 缓存会让浏览器无限期使用旧版本。
//! 具体策略见 [`CONSOLE_ASSET_CACHE_CONTROL`]。

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use pulldown_cmark::{Options, Parser, html};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::routes::OpsHttpState;

pub(super) async fn console_configuration(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::management::require_admin(&state, &headers, false) {
        return with_console_cors(*response, &state, &headers);
    }
    let Some(config_center) = state.config_center.as_ref() else {
        return with_console_cors(StatusCode::NOT_FOUND.into_response(), &state, &headers);
    };
    let response = match config_center.current_snapshot() {
        Ok(snapshot) => Json(json!({
            "ok": true,
            "configuration": snapshot,
            "registered_tools": state.registered_tools.as_ref(),
            "restart": {"available": state.restart_controller.available()},
        }))
        .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": {"code": err.code(), "message": err.message()}})),
        )
            .into_response(),
    };
    with_console_cors(response, &state, &headers)
}

pub(super) async fn healthz(State(state): State<OpsHttpState>) -> Json<serde_json::Value> {
    let provider = state.provider.as_ref();
    Json(json!({
        "ok": true,
        "ready": !state.setup_required,
        "state": if state.setup_required { "setup_required" } else { "ready" },
        "provider": provider.map(|value| value.name()).unwrap_or("not_configured"),
        "model": provider.map(|value| value.model()).unwrap_or("not_configured"),
        "stream": provider.map(|value| value.stream_enabled()).unwrap_or(false),
        "upstream": state.upstream_status.snapshot(),
    }))
}

pub(super) async fn console_index(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
) -> Response {
    let mut response = with_console_csp(with_console_cors(
        Html(include_str!("../../../web-console/dist/index.html")).into_response(),
        &state,
        &headers,
    ));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// 控制台固定名静态资源的统一缓存策略。
///
/// 构建产物没有内容哈希、URL 也不含版本号，一旦 dist 重新构建覆盖同名文件，
/// 浏览器或代理中 `public, max-age=31536000, immutable` 的旧缓存永远不会失效，
/// 只能靠用户强制刷新才能拿到新版本。因此对全部固定名资源（JS/CSS/文本与
/// 背景图片）统一采用重新验证策略：允许共享缓存保存副本，但每次使用前都必须
/// 回源重新验证，浏览器不会无限期复用陈旧内容。当前响应未下发
/// ETag / Last-Modified，重新验证实际表现为完整重新拉取；后续若引入内容
/// 版本化（文件名带哈希），再对带哈希的 URL 单独启用 `immutable` 长期缓存。
const CONSOLE_ASSET_CACHE_CONTROL: &str = "public, max-age=0, must-revalidate";

// dist 新增前端模块时必须同步登记；下方测试会校验构建产物与静态 import 均已覆盖。
const CONSOLE_ASSETS: &[(&str, &str, &str)] = &[
    (
        "agent-tools.js",
        include_str!("../../../web-console/dist/agent-tools.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "api-routes.js",
        include_str!("../../../web-console/dist/api-routes.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "api.js",
        include_str!("../../../web-console/dist/api.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "app.js",
        include_str!("../../../web-console/dist/app.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "background.js",
        include_str!("../../../web-console/dist/background.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "console-shell.js",
        include_str!("../../../web-console/dist/console-shell.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "dom.js",
        include_str!("../../../web-console/dist/dom.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "file-cache.js",
        include_str!("../../../web-console/dist/file-cache.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "styles.css",
        include_str!("../../../web-console/dist/styles.css"),
        "text/css; charset=utf-8",
    ),
    (
        "theme.js",
        include_str!("../../../web-console/dist/theme.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "types.js",
        include_str!("../../../web-console/dist/types.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/agent-fields.js",
        include_str!("../../../web-console/dist/views/configuration/agent-fields.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/autosave.js",
        include_str!("../../../web-console/dist/views/configuration/autosave.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/configuration.js",
        include_str!("../../../web-console/dist/views/configuration/configuration.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/fields.js",
        include_str!("../../../web-console/dist/views/configuration/fields.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/model-route-editor.js",
        include_str!("../../../web-console/dist/views/configuration/model-route-editor.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/navigation.js",
        include_str!("../../../web-console/dist/views/configuration/navigation.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/opencode-providers.js",
        include_str!("../../../web-console/dist/views/configuration/opencode-providers.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/public-fields.js",
        include_str!("../../../web-console/dist/views/configuration/public-fields.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/secret-fields.js",
        include_str!("../../../web-console/dist/views/configuration/secret-fields.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/state.js",
        include_str!("../../../web-console/dist/views/configuration/state.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/theme-selector.js",
        include_str!("../../../web-console/dist/views/configuration/theme-selector.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/tts.js",
        include_str!("../../../web-console/dist/views/configuration/tts.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/ui.js",
        include_str!("../../../web-console/dist/views/configuration/ui.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/configuration/web-search.js",
        include_str!("../../../web-console/dist/views/configuration/web-search.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/dashboard.js",
        include_str!("../../../web-console/dist/views/dashboard.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/markdown.js",
        include_str!("../../../web-console/dist/views/markdown.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/platforms.js",
        include_str!("../../../web-console/dist/views/platforms.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/storage.js",
        include_str!("../../../web-console/dist/views/storage.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/todo/todo-card.js",
        include_str!("../../../web-console/dist/views/todo/todo-card.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/todo/todo-form.js",
        include_str!("../../../web-console/dist/views/todo/todo-form.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/todo/todo-paging.js",
        include_str!("../../../web-console/dist/views/todo/todo-paging.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "views/todo/todo.js",
        include_str!("../../../web-console/dist/views/todo/todo.js"),
        "text/javascript; charset=utf-8",
    ),
];

const CONSOLE_BINARY_ASSETS: &[(&str, &[u8], &str)] = &[
    (
        "background/default.png",
        include_bytes!("../../../web-console/dist/background/default.png"),
        "image/png",
    ),
    (
        "background/special.webp",
        include_bytes!("../../../web-console/dist/background/special.webp"),
        "image/webp",
    ),
];

pub(super) async fn console_asset(
    State(state): State<OpsHttpState>,
    Path(asset): Path<String>,
    headers: HeaderMap,
) -> Response {
    let found = CONSOLE_ASSETS
        .iter()
        .find(|(path, _, _)| *path == asset)
        .map(|(_, body, content_type)| (*body, *content_type));
    match found {
        Some((body, content_type)) => static_console_asset(body, content_type, &state, &headers),
        None => match CONSOLE_BINARY_ASSETS
            .iter()
            .find(|(path, _, _)| *path == asset)
            .map(|(_, body, content_type)| (*body, *content_type))
        {
            Some((body, content_type)) => {
                static_console_binary_asset(body, content_type, &state, &headers)
            }
            None => with_console_cors(StatusCode::NOT_FOUND.into_response(), &state, &headers),
        },
    }
}

fn static_console_asset(
    body: &'static str,
    content_type: &'static str,
    state: &OpsHttpState,
    headers: &HeaderMap,
) -> Response {
    let mut response = with_console_cors(body.into_response(), state, headers);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CONSOLE_ASSET_CACHE_CONTROL),
    );
    response
}

fn static_console_binary_asset(
    body: &'static [u8],
    content_type: &'static str,
    state: &OpsHttpState,
    headers: &HeaderMap,
) -> Response {
    let mut response = with_console_cors(body.into_response(), state, headers);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CONSOLE_ASSET_CACHE_CONTROL),
    );
    response
}

#[derive(Serialize)]
struct ConsoleCapabilityRow {
    platform: String,
    scope: String,
    label: String,
    enabled: bool,
    inbound: crate::http::console::ConsoleCapabilities,
    outbound: crate::http::console::ConsoleCapabilities,
}

pub(super) async fn console_status(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
) -> Response {
    let external = state.console_status_source.snapshot();
    let capabilities = external
        .platforms
        .iter()
        .flat_map(|platform| {
            platform
                .capability_scopes
                .iter()
                .map(|scope| ConsoleCapabilityRow {
                    platform: platform.id.clone(),
                    scope: scope.id.clone(),
                    label: scope.label.clone(),
                    enabled: scope.enabled,
                    inbound: scope.capabilities.inbound.clone(),
                    outbound: scope.capabilities.outbound.clone(),
                })
        })
        .collect::<Vec<_>>();
    let mut storage = state.core_summary.core_storage();
    storage.extend(external.storage);
    let upstream = state.upstream_status.snapshot();
    let provider = state.provider.as_ref();
    let response = Json(json!({
        "runtime": {
            "ok": true,
            "ready": !state.setup_required,
            "state": if state.setup_required { "setup_required" } else { "ready" },
            "version": state.core_summary.application_version,
            "started_at": state.core_summary.started_at,
            "uptime_seconds": state.core_summary.started_instant.elapsed().as_secs(),
        },
        "provider": {
            "name": provider.map(|value| value.name()).unwrap_or("not_configured"),
            "model": provider.map(|value| value.model()).unwrap_or("not_configured"),
            "streaming": provider.map(|value| value.stream_enabled()).unwrap_or(false),
            "configured": provider.is_some() && state.core_summary.provider_configured,
            "upstream": upstream,
        },
        "platforms": external.platforms,
        "capabilities": capabilities,
        "storage": storage,
        "configuration": {
            "web_console_enabled": state.config.web_console_enabled,
            "cors_allowlist_configured": !state.config.web_console_allowed_origins.is_empty(),
            "listen": state.core_summary.listen_summary,
            "rss_enabled": state.core_summary.rss_enabled,
            "tool_calling_enabled": state.core_summary.tool_calling_enabled,
        }
    }))
    .into_response();
    with_console_cors(response, &state, &headers)
}

#[derive(Debug, Deserialize)]
struct MarkdownRenderRequest {
    markdown: String,
}

pub(super) async fn markdown_render(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.len() > 64 * 1024 {
        return with_console_cors(
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"ok": false, "error": "markdown payload too large"})),
            )
                .into_response(),
            &state,
            &headers,
        );
    }

    let payload = match serde_json::from_slice::<MarkdownRenderRequest>(&body) {
        Ok(payload) => payload,
        Err(_) => {
            return with_console_cors(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "error": "invalid markdown render payload"})),
                )
                    .into_response(),
                &state,
                &headers,
            );
        }
    };
    let html = render_markdown_html(&payload.markdown);
    with_console_cors(
        Json(json!({"ok": true, "html": html})).into_response(),
        &state,
        &headers,
    )
}

pub(super) async fn markdown_render_preflight(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
) -> Response {
    with_console_preflight_cors(StatusCode::NO_CONTENT.into_response(), &state, &headers)
}

fn render_markdown_html(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(markdown, options);
    let mut html = String::new();
    html::push_html(&mut html, parser);
    let mut cleaner = ammonia::Builder::default();
    cleaner.add_tags(["input"]);
    cleaner.add_tag_attributes("input", ["type", "checked", "disabled"]);
    cleaner.clean(&html).to_string()
}

fn with_console_security(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

fn with_console_csp(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        // img-src 允许 blob:：自定义背景通过 POST 文件接口读取为 Blob 后，
        // 前端以 object URL（blob:）渲染，CSP 必须放行才能显示。
        HeaderValue::from_static(
            "default-src 'self'; style-src 'self'; script-src 'self'; img-src 'self' data: blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        ),
    );
    response
}

pub(super) fn with_console_cors(
    mut response: Response,
    state: &OpsHttpState,
    headers: &HeaderMap,
) -> Response {
    let Some(origin) = allowed_console_origin(state, headers) else {
        return with_console_security(response);
    };
    let Ok(value) = HeaderValue::from_str(origin) else {
        return with_console_security(response);
    };
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("origin"));
    with_console_security(response)
}

fn with_console_preflight_cors(
    mut response: Response,
    state: &OpsHttpState,
    headers: &HeaderMap,
) -> Response {
    let Some(origin) = allowed_console_origin(state, headers) else {
        return with_console_security(response);
    };
    let Ok(value) = HeaderValue::from_str(origin) else {
        return with_console_security(response);
    };
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
    response.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static(
            "origin, access-control-request-method, access-control-request-headers",
        ),
    );
    with_console_security(response)
}

pub(super) fn allowed_console_origin<'a>(
    state: &'a OpsHttpState,
    headers: &'a HeaderMap,
) -> Option<&'a str> {
    let origin = headers.get(header::ORIGIN)?.to_str().ok()?;
    state
        .config
        .web_console_allowed_origins
        .iter()
        .map(String::as_str)
        .find(|allowed| *allowed == origin)
}

#[cfg(test)]
mod tests {
    use super::{CONSOLE_ASSET_CACHE_CONTROL, CONSOLE_ASSETS, CONSOLE_BINARY_ASSETS};
    use crate::{
        error::LlmError,
        http::routes::{OpsHttpConfig, OpsHttpState, build_router},
        util::metrics::LlmMetrics,
    };
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{HeaderMap, StatusCode, header},
    };
    use qq_maid_llm::provider::{
        ChatOutcome, LlmProvider,
        status::{UpstreamStatus, observe_provider},
        types::{ChatRequest, TokenUsage},
    };
    use regex::Regex;
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };
    use tower::ServiceExt;

    fn dist_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../web-console/dist")
    }

    fn collect_dist_files(root: &Path, directory: &Path, files: &mut Vec<String>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_dist_files(root, &path, files);
            } else {
                files.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/"),
                );
            }
        }
    }

    fn resolve_import(source: &str, specifier: &str) -> String {
        let mut components = source
            .rsplit_once('/')
            .map(|(parent, _)| parent.split('/').collect::<Vec<_>>())
            .unwrap_or_default();
        for component in specifier.split('/') {
            match component {
                "." | "" => {}
                ".." => {
                    components.pop();
                }
                value => components.push(value),
            }
        }
        components.join("/")
    }

    #[test]
    fn embedded_console_assets_match_dist_output() {
        let root = dist_root();
        let mut dist_files = Vec::new();
        collect_dist_files(&root, &root, &mut dist_files);
        dist_files.retain(|path| path != "index.html");
        dist_files.sort();

        let mut embedded = CONSOLE_ASSETS
            .iter()
            .map(|(path, body, _)| {
                assert!(!body.is_empty(), "控制台资源内容不能为空: {path}");
                (*path).to_owned()
            })
            .collect::<Vec<_>>();
        embedded.extend(CONSOLE_BINARY_ASSETS.iter().map(|(path, body, _)| {
            assert!(!body.is_empty(), "控制台资源内容不能为空: {path}");
            (*path).to_owned()
        }));
        embedded.sort();

        assert_eq!(
            embedded, dist_files,
            "dist 构建产物必须全部注册到控制台资源表"
        );
    }

    #[test]
    fn html_and_javascript_static_imports_are_embedded() {
        let mut registered = CONSOLE_ASSETS
            .iter()
            .map(|(path, _, _)| *path)
            .collect::<std::collections::HashSet<_>>();
        registered.extend(CONSOLE_BINARY_ASSETS.iter().map(|(path, _, _)| *path));
        let html = std::fs::read_to_string(dist_root().join("index.html")).unwrap();
        let html_asset = Regex::new(r#"(?:src|href)=["'](/console/[^"'?#]+)["']"#).unwrap();
        for captures in html_asset.captures_iter(&html) {
            let path = captures[1].trim_start_matches("/console/");
            assert!(registered.contains(path), "HTML 静态资源未注册: {path}");
        }

        let static_import =
            Regex::new(r#"(?m)^\s*(?:import|export)\s+(?:.*?\s+from\s+)?["']([^"']+)["'];?\s*$"#)
                .unwrap();
        for (source, body, content_type) in CONSOLE_ASSETS {
            if *content_type != "text/javascript; charset=utf-8" {
                continue;
            }
            for captures in static_import.captures_iter(body) {
                let specifier = &captures[1];
                if !specifier.starts_with('.') {
                    continue;
                }
                let imported = resolve_import(source, specifier);
                assert!(
                    registered.contains(imported.as_str()),
                    "JavaScript 静态 import 未注册: {source} -> {specifier} ({imported})"
                );
            }
        }
    }

    // 与 http::routes::tests 相同的 MockProvider 模式，仅用于构建路由状态。
    #[derive(Clone)]
    struct MockProvider;

    #[async_trait]
    impl LlmProvider for MockProvider {
        async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
            Ok(ChatOutcome {
                reply: "# 标题\n- hello".to_owned(),
                output_parts: Vec::new(),
                metrics: LlmMetrics {
                    provider: "mock".to_owned(),
                    model: "mock-model".to_owned(),
                    stream: true,
                    ttfe_ms: Some(1),
                    ttft_ms: Some(2),
                    total_latency_ms: 3,
                },
                usage: Some(TokenUsage {
                    input_tokens: None,
                    cached_input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                }),
                fallback_used: false,
                agent: Default::default(),
            })
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn model(&self) -> &str {
            "mock-model"
        }

        fn stream_enabled(&self) -> bool {
            true
        }
    }

    fn test_console_state() -> OpsHttpState {
        let upstream_status = UpstreamStatus::default();
        let provider = observe_provider(Arc::new(MockProvider), upstream_status.clone());
        OpsHttpState::from_parts(
            OpsHttpConfig {
                web_console_enabled: true,
                web_console_allowed_origins: Vec::new(),
                web_console_trusted_proxy_ips: Vec::new(),
                web_console_secure_cookies: false,
            },
            provider,
            upstream_status,
        )
    }

    async fn get_console_response(state: OpsHttpState, path: &str) -> (StatusCode, HeaderMap) {
        let app = build_router(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        (response.status(), response.headers().clone())
    }

    fn assert_revalidatable_cache_control(headers: &HeaderMap, path: &str) {
        let value = headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_else(|| panic!("{path} 必须返回 Cache-Control"));
        assert_eq!(
            value, CONSOLE_ASSET_CACHE_CONTROL,
            "{path} 应使用重新验证式缓存策略"
        );
        assert!(
            !value.contains("31536000") && !value.contains("immutable"),
            "{path} 不得使用长期不可变缓存"
        );
    }

    #[tokio::test]
    async fn console_index_uses_no_cache_without_long_max_age() {
        let (status, headers) = get_console_response(test_console_state(), "/console/").await;

        assert_eq!(status, StatusCode::OK);
        let value = headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_else(|| panic!("/console/ 必须返回 Cache-Control"));
        assert_eq!(value, "no-cache");
        assert!(
            !value.contains("31536000") && !value.contains("immutable"),
            "/console/ 不得使用长期不可变缓存"
        );
    }

    #[tokio::test]
    async fn fixed_name_text_assets_are_revalidated_not_immutable() {
        for (path, _, _) in CONSOLE_ASSETS {
            let (status, headers) =
                get_console_response(test_console_state(), &format!("/console/{path}")).await;

            assert_eq!(status, StatusCode::OK, "{path}");
            assert_revalidatable_cache_control(&headers, path);
        }
    }

    #[tokio::test]
    async fn fixed_name_binary_assets_are_revalidated_not_immutable() {
        for (path, _, _) in CONSOLE_BINARY_ASSETS {
            let (status, headers) =
                get_console_response(test_console_state(), &format!("/console/{path}")).await;

            assert_eq!(status, StatusCode::OK, "{path}");
            assert_revalidatable_cache_control(&headers, path);
        }
    }
}
