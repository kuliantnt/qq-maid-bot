//! Web 控制台静态资源、状态摘要、Markdown 预览和安全响应头。

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
    with_console_csp(with_console_cors(
        Html(include_str!("../../../web-console/dist/index.html")).into_response(),
        &state,
        &headers,
    ))
}

// dist 新增前端模块时必须同步登记；下方测试会校验构建产物与静态 import 均已覆盖。
const CONSOLE_ASSETS: &[(&str, &str, &str)] = &[
    (
        "styles.css",
        include_str!("../../../web-console/dist/styles.css"),
        "text/css; charset=utf-8",
    ),
    (
        "app.js",
        include_str!("../../../web-console/dist/app.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "agent-tools.js",
        include_str!("../../../web-console/dist/agent-tools.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "api.js",
        include_str!("../../../web-console/dist/api.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "dom.js",
        include_str!("../../../web-console/dist/dom.js"),
        "text/javascript; charset=utf-8",
    ),
    (
        "types.js",
        include_str!("../../../web-console/dist/types.js"),
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
        "views/configuration.js",
        include_str!("../../../web-console/dist/views/configuration.js"),
        "text/javascript; charset=utf-8",
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
        None => with_console_cors(StatusCode::NOT_FOUND.into_response(), &state, &headers),
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
        HeaderValue::from_static(
            "default-src 'self'; style-src 'self'; script-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
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
    use super::CONSOLE_ASSETS;
    use regex::Regex;
    use std::path::{Path, PathBuf};

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
        embedded.sort();

        assert_eq!(
            embedded, dist_files,
            "dist 构建产物必须全部注册到控制台资源表"
        );
    }

    #[test]
    fn html_and_javascript_static_imports_are_embedded() {
        let registered = CONSOLE_ASSETS
            .iter()
            .map(|(path, _, _)| *path)
            .collect::<std::collections::HashSet<_>>();
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
}
