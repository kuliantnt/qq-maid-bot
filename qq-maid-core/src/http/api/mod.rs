//! Web 控制台 API 模块。
//!
//! `common` 只承载鉴权、分页、响应 envelope 和 HTTP 错误等 Web API 基础设施；
//! Todo、后续 Memory 等资源仍各自维护领域 DTO、权限规则和业务服务。

pub(crate) mod common;
pub(crate) mod knowledge;
pub(crate) mod memory;
pub(crate) mod todo;
pub(crate) mod user_data;

use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use pulldown_cmark::{Options, Parser, html};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MarkdownRenderRequest {
    markdown: String,
}

pub async fn render_markdown(
    payload: Result<Json<MarkdownRenderRequest>, JsonRejection>,
) -> Response {
    let Json(payload) = match payload {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": {
                        "code": "invalid_json",
                        "message": err.body_text(),
                    }
                })),
            )
                .into_response();
        }
    };

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(&payload.markdown, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    let sanitized_html = ammonia::clean(&html_output);

    Json(json!({
        "ok": true,
        "html": sanitized_html,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn render_markdown_outputs_sanitized_html() {
        let response = render_markdown(Ok(Json(MarkdownRenderRequest {
            markdown: "# Hi\n\n<script>alert(1)</script>\n\n- ok".to_owned(),
        })))
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["ok"], true);
        let html = json["html"].as_str().unwrap();
        assert!(html.contains("<h1>Hi</h1>"));
        assert!(!html.contains("<script>"));
    }
}
