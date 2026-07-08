//! 联网搜索指令的处理流程。
//!
//! `/查` `/查询` `/search` 只负责用户入口兼容、参数校验、session 记录和展示；
//! 实际联网查询统一通过 `runtime/tools/search.rs` 中的 `web_search` Tool 执行。

use std::{future::Future, pin::Pin};

use qq_maid_llm::web_search::WebSearchOutcome;
use serde_json::json;

use crate::{
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        session::SessionRecord,
        tools::{
            WEB_SEARCH_QUERY_MAX_LENGTH, WEB_SEARCH_TOOL_NAME, WebSearchDeltaHandler,
            WebSearchTool, WebSearchToolRequest,
        },
    },
};

use super::{
    RespondRequest, RespondResponse, RustRespondService,
    common::{
        command_response, command_response_with_stream, session_error, structured_command_body,
        truncate_chars,
    },
};

// /查 指令的空参数用法提示
const WEB_SEARCH_USAGE_REPLY: &str = "用法：/查 关键词（也可用 /查询 关键词 或 /search 关键词）
例如：/查 Cloudflare D1 binding DB is not configured";
// 查询超长时的提示
const WEB_SEARCH_TOO_LONG_REPLY: &str = "查询内容太长了，请压缩到 200 字以内再试。";
// Level 2 进度事件的兼容文本：用户可见，但不写入 session 或模型上下文。
const WEB_SEARCH_RUNNING_DELTA: &str = "正在联网查询中…\n\n";
// 搜索结果为空时的回复
const WEB_SEARCH_EMPTY_RESULT_REPLY: &str = "【联网查询】

没查到明确结果。可以换一个关键词再试。";
// 联网查询未配置时的回复
const WEB_SEARCH_CONFIG_ERROR_REPLY: &str = "【联网查询】

联网查询还没有配置好，请检查 OPENAI_API_KEY、OPENAI_BASE_URL 和查询模型配置。";
// 联网查询超时时的回复
const WEB_SEARCH_TIMEOUT_REPLY: &str = "【联网查询】

联网查询超时了，请稍后再试。";
// 上游服务异常时的回复
const WEB_SEARCH_UPSTREAM_ERROR_REPLY: &str = "【联网查询】

联网查询服务暂时不可用，可能是上游接口、代理或网络配置异常。请稍后再试。";

#[derive(Debug, Clone)]
struct WebSearchToolOutput {
    answer: String,
    provider: String,
    elapsed_ms: u64,
}

impl RustRespondService {
    /// 处理联网搜索指令的主入口。校验参数、显式执行 `web_search` Tool、格式化结果或错误回复。
    pub(super) async fn handle_web_search_command(
        &self,
        command: ParsedCommand,
        req: &RespondRequest,
        session: &mut SessionRecord,
    ) -> Result<RespondResponse, LlmError> {
        self.handle_web_search_command_inner(command, req, session, false, None)
            .await
    }

    /// `/查` 流式入口：通过 `WebSearchTool::query_stream` 转发真实搜索增量。
    pub async fn handle_web_search_command_stream<F>(
        &self,
        command: ParsedCommand,
        req: &RespondRequest,
        session: &mut SessionRecord,
        on_delta: F,
    ) -> Result<RespondResponse, LlmError>
    where
        F: FnMut(String) -> Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send>> + Send,
    {
        self.handle_web_search_command_inner(
            command,
            req,
            session,
            true,
            Some(Box::new(on_delta) as WebSearchDeltaHandler<'_>),
        )
        .await
    }

    async fn handle_web_search_command_inner(
        &self,
        command: ParsedCommand,
        req: &RespondRequest,
        session: &mut SessionRecord,
        stream: bool,
        mut on_delta: Option<WebSearchDeltaHandler<'_>>,
    ) -> Result<RespondResponse, LlmError> {
        let query = command.argument.trim();
        if query.is_empty() {
            return Ok(command_response(
                WEB_SEARCH_USAGE_REPLY,
                Some(session.session_id.clone()),
                Some(command.action),
            ));
        }
        if query.chars().count() > WEB_SEARCH_QUERY_MAX_LENGTH {
            return Ok(command_response(
                WEB_SEARCH_TOO_LONG_REPLY,
                Some(session.session_id.clone()),
                Some(command.action),
            ));
        }

        let command_text = format!("/{} {}", command.raw_command, command.argument);
        let raw_question = web_search_raw_question(&command_text, req);
        if stream && let Some(on_delta) = on_delta.as_mut() {
            on_delta(WEB_SEARCH_RUNNING_DELTA.to_owned()).await?;
        }
        let policy = self.resolve_agent_policy(req)?;
        let output_result = if stream {
            self.execute_web_search_tool_stream(
                query,
                &raw_question,
                Some(policy.search_model.clone()),
                on_delta.take(),
            )
            .await
        } else {
            self.execute_web_search_tool(query, &raw_question, Some(policy.search_model.clone()))
                .await
        };
        let output = match output_result {
            Ok(output) => output,
            Err(err) => {
                tracing::warn!(
                    error_code = err.code,
                    error_stage = err.stage,
                    query_provider = self.query_executor.provider_name(),
                    "web search command tool failed"
                );
                let reply = format_web_search_error_reply(&err);
                self.session_store
                    .append_exchange(session, &command_text, &reply)
                    .map_err(session_error)?;

                let response = build_web_search_response(
                    session.session_id.clone(),
                    command.action.clone(),
                    reply,
                    self.query_executor.provider_name().to_owned(),
                    Some(err.code.clone()),
                    Some(err.stage.clone()),
                    stream,
                );
                return Ok(response);
            }
        };

        let reply = if output.answer.trim().is_empty() {
            WEB_SEARCH_EMPTY_RESULT_REPLY.to_owned()
        } else {
            format_web_search_command_reply(&output.answer)
        };
        self.session_store
            .append_exchange(session, &command_text, &reply)
            .map_err(session_error)?;

        Ok(build_web_search_success_response(
            session.session_id.clone(),
            command.action,
            reply,
            output.provider,
            output.elapsed_ms,
            stream,
        ))
    }

    async fn execute_web_search_tool(
        &self,
        query: &str,
        raw_question: &str,
        model_override: Option<String>,
    ) -> Result<WebSearchToolOutput, LlmError> {
        let tool = WebSearchTool::new(self.query_executor.clone());
        let outcome = tool
            .query(WebSearchToolRequest {
                query: query.to_owned(),
                raw_question: Some(raw_question.to_owned()),
                max_results: None,
                context_size: None,
                model_override,
            })
            .await?;
        Ok(web_search_output_from_outcome(outcome))
    }

    async fn execute_web_search_tool_stream(
        &self,
        query: &str,
        raw_question: &str,
        model_override: Option<String>,
        on_delta: Option<WebSearchDeltaHandler<'_>>,
    ) -> Result<WebSearchToolOutput, LlmError> {
        let tool = WebSearchTool::new(self.query_executor.clone());
        let outcome = tool
            .query_stream_with_handler(
                WebSearchToolRequest {
                    query: query.to_owned(),
                    raw_question: Some(raw_question.to_owned()),
                    max_results: None,
                    context_size: None,
                    model_override,
                },
                on_delta,
            )
            .await?;
        Ok(WebSearchToolOutput {
            answer: outcome.answer,
            provider: outcome.provider,
            elapsed_ms: outcome.elapsed_ms,
        })
    }
}

/// 从用户文本中解析联网搜索指令（/查、/查询、/search 等）。
pub(super) fn parse_web_search_command(text: &str) -> Option<ParsedCommand> {
    if let Some(command) = parse_slash_command(text) {
        return matches!(command.action.as_str(), "web_search").then_some(command);
    }
    parse_compact_web_search_command(text)
}

/// 在 router 已判定 `RespondPlan::WebSearch` 后，将输入统一转换为 web_search 命令。
/// 显式 `/查` 保留原命令；自然语言搜索意图用原话作为 query 复用同一查询流程。
pub(super) fn web_search_command_for_plan(text: &str) -> ParsedCommand {
    parse_web_search_command(text).unwrap_or_else(|| ParsedCommand {
        action: "web_search".to_owned(),
        argument: text.trim().to_owned(),
        raw_command: "查".to_owned(),
    })
}

fn web_search_raw_question(command_text: &str, req: &RespondRequest) -> String {
    let Some(quoted_context) = quoted_search_context(req) else {
        return command_text.to_owned();
    };
    format!("{command_text}\n\n引用消息上下文：\n{quoted_context}")
}

fn quoted_search_context(req: &RespondRequest) -> Option<String> {
    let quoted = req.quoted.as_ref()?;
    if !quoted.lookup_found {
        return None;
    }

    let mut lines = Vec::new();
    if let Some(text) = quoted
        .text_summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("引用文本：{text}"));
    }
    for part in &quoted.input_parts {
        if let Some(text) = part
            .text_content()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("引用文本：{text}"));
        }
    }
    for media in &quoted.media_summaries {
        let summary = media.summary.trim();
        if !summary.is_empty() {
            lines.push(format!("引用媒体：{summary}"));
        }
    }

    (!lines.is_empty()).then(|| truncate_chars(&lines.join("\n"), 800))
}

fn parse_compact_web_search_command(text: &str) -> Option<ParsedCommand> {
    let text = text.trim();

    // 中文 `/查今天新闻`、`/查询今日八卦` 很常省略空格。
    // 这里只给联网查询补兼容，避免扩大到所有 slash 命令后影响既有语义。
    for raw_command in ["查询", "查"] {
        let prefix = format!("/{raw_command}");
        let Some(argument) = text.strip_prefix(&prefix) else {
            continue;
        };
        let argument = argument.trim();
        if argument.is_empty() {
            continue;
        }
        return Some(ParsedCommand {
            action: "web_search".to_owned(),
            argument: argument.to_owned(),
            raw_command: raw_command.to_owned(),
        });
    }

    None
}

pub(super) fn format_web_search_command_reply(answer: &str) -> String {
    let mut text = answer.trim().to_owned();
    if text.is_empty() {
        text = "没查到明确结果。可以换一个关键词再试。".to_owned();
    }
    if !text.starts_with("【联网查询】") {
        text = format!("【联网查询】\n\n{text}");
    }
    truncate_chars(&text, 1500)
}

pub(super) fn format_web_search_error_reply(err: &LlmError) -> String {
    match err.code.as_str() {
        "config" => WEB_SEARCH_CONFIG_ERROR_REPLY.to_owned(),
        "timeout" => WEB_SEARCH_TIMEOUT_REPLY.to_owned(),
        _ => WEB_SEARCH_UPSTREAM_ERROR_REPLY.to_owned(),
    }
}

fn web_search_output_from_outcome(outcome: WebSearchOutcome) -> WebSearchToolOutput {
    WebSearchToolOutput {
        answer: outcome.answer,
        provider: outcome.provider,
        elapsed_ms: outcome.elapsed_ms,
    }
}

fn build_web_search_response(
    session_id: String,
    command: String,
    reply: String,
    query_provider: String,
    query_error_code: Option<String>,
    query_error_stage: Option<String>,
    stream: bool,
) -> RespondResponse {
    let mut response = command_response_with_stream(
        structured_command_body(reply),
        Some(session_id),
        Some(command),
        stream,
    );
    let mut diagnostics = json!({
        "backend": "rust",
        "session_backend": "rust",
        "used_memory": false,
        "used_search": true,
        "query_provider": query_provider,
        "search_tool": WEB_SEARCH_TOOL_NAME,
    });
    if let Some(code) = query_error_code {
        diagnostics["query_error_code"] = json!(code);
    }
    if let Some(stage) = query_error_stage {
        diagnostics["query_error_stage"] = json!(stage);
    }
    response.diagnostics = Some(diagnostics);
    response
}

fn build_web_search_success_response(
    session_id: String,
    command: String,
    reply: String,
    query_provider: String,
    query_elapsed_ms: u64,
    stream: bool,
) -> RespondResponse {
    let mut response = command_response_with_stream(
        structured_command_body(reply),
        Some(session_id),
        Some(command),
        stream,
    );
    response.diagnostics = Some(json!({
        "backend": "rust",
        "session_backend": "rust",
        "used_memory": false,
        "used_search": true,
        "query_provider": query_provider,
        "query_elapsed_ms": query_elapsed_ms,
        "search_tool": WEB_SEARCH_TOOL_NAME,
    }));
    response
}
