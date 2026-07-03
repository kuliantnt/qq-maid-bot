//! 联网搜索指令的处理流程。
//! 负责解析 `/查` `/查询` `/search` 等指令，调用查询执行器进行联网搜索，
//! 并格式化搜索结果回复。同时处理超时、配置缺失、上游异常等错误场景。

use std::{future::Future, pin::Pin};

use serde_json::json;
use tokio::sync::mpsc;

use crate::{
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        query::QueryRequest,
        session::SessionRecord,
    },
};

use super::{
    RespondRequest, RespondResponse, RustRespondService,
    common::{
        command_response, command_response_with_stream, session_error, structured_command_body,
        truncate_chars,
    },
};

// 联网搜索查询内容最大字符数限制
const WEB_SEARCH_QUERY_MAX_LENGTH: usize = 200;
// /查 指令的空参数用法提示
const WEB_SEARCH_USAGE_REPLY: &str = "用法：/查 关键词（也可用 /查询 关键词 或 /search 关键词）
例如：/查 Cloudflare D1 binding DB is not configured";
// 查询超长时的提示
const WEB_SEARCH_TOO_LONG_REPLY: &str = "查询内容太长了，请压缩到 200 字以内再试。";
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

impl RustRespondService {
    /// 处理联网搜索指令的主入口。校验参数、调用查询执行器、格式化结果或错误回复。
    pub(super) async fn handle_web_search_command(
        &self,
        command: ParsedCommand,
        req: &RespondRequest,
        session: &mut SessionRecord,
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
        let policy = self.resolve_agent_policy(req)?;
        let outcome = match self
            .query_executor
            .query(QueryRequest {
                query: query.to_owned(),
                raw_question: Some(command_text.clone()),
                max_results: None,
                context_size: None,
                model_override: Some(policy.search_model.clone()),
            })
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(
                    error_code = err.code,
                    error_stage = err.stage,
                    query_provider = self.query_executor.provider_name(),
                    "web search command failed"
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
                    false,
                );
                return Ok(response);
            }
        };
        let reply = if outcome.answer.trim().is_empty() {
            WEB_SEARCH_EMPTY_RESULT_REPLY.to_owned()
        } else {
            format_web_search_command_reply(&outcome.answer)
        };
        self.session_store
            .append_exchange(session, &command_text, &reply)
            .map_err(session_error)?;

        let response = build_web_search_success_response(
            session.session_id.clone(),
            command.action,
            reply,
            outcome.provider,
            outcome.elapsed_ms,
            false,
        );
        Ok(response)
    }

    /// `/查` 真流式路径：只消费执行器的 `query_stream`，不退回完整 `query()` 伪增量。
    pub async fn handle_web_search_command_stream<F>(
        &self,
        command: ParsedCommand,
        req: &RespondRequest,
        session: &mut SessionRecord,
        mut on_delta: F,
    ) -> Result<RespondResponse, LlmError>
    where
        F: FnMut(String) -> Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send>> + Send,
    {
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
        let policy = self.resolve_agent_policy(req)?;
        let (delta_tx, mut delta_rx) = mpsc::channel(16);
        let query_executor = self.query_executor.clone();
        let query_req = QueryRequest {
            query: query.to_owned(),
            raw_question: Some(command_text.clone()),
            max_results: None,
            context_size: None,
            model_override: Some(policy.search_model.clone()),
        };
        let query_task =
            tokio::spawn(async move { query_executor.query_stream(query_req, delta_tx).await });
        while let Some(delta) = delta_rx.recv().await {
            if !delta.is_empty()
                && let Err(err) = on_delta(delta).await
            {
                // 接收方取消时必须停止继续 poll provider stream，而不是只停止转发。
                query_task.abort();
                return Err(err);
            }
        }
        let outcome = match query_task.await.map_err(|err| {
            LlmError::provider(format!("web search stream task failed: {err}"), "internal")
        })? {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(
                    error_code = err.code,
                    error_stage = err.stage,
                    query_provider = self.query_executor.provider_name(),
                    "web search stream command failed"
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
                    true,
                );
                return Ok(response);
            }
        };
        let reply = if outcome.answer.trim().is_empty() {
            WEB_SEARCH_EMPTY_RESULT_REPLY.to_owned()
        } else {
            format_web_search_command_reply(&outcome.answer)
        };
        self.session_store
            .append_exchange(session, &command_text, &reply)
            .map_err(session_error)?;

        Ok(build_web_search_success_response(
            session.session_id.clone(),
            command.action,
            reply,
            outcome.provider,
            outcome.elapsed_ms,
            true,
        ))
    }
}

/// 从用户文本中解析联网搜索指令（/查、/查询、/search 等）。
pub(super) fn parse_web_search_command(text: &str) -> Option<ParsedCommand> {
    if let Some(command) = parse_slash_command(text) {
        return matches!(command.action.as_str(), "web_search").then_some(command);
    }
    parse_compact_web_search_command(text)
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

fn format_web_search_command_reply(answer: &str) -> String {
    let mut text = answer.trim().to_owned();
    if text.is_empty() {
        text = "没查到明确结果。可以换一个关键词再试。".to_owned();
    }
    if !text.starts_with("【联网查询】") {
        text = format!("【联网查询】\n\n{text}");
    }
    truncate_chars(&text, 1500)
}

fn format_web_search_error_reply(err: &LlmError) -> String {
    match err.code.as_str() {
        "config" => WEB_SEARCH_CONFIG_ERROR_REPLY.to_owned(),
        "timeout" => WEB_SEARCH_TIMEOUT_REPLY.to_owned(),
        _ => WEB_SEARCH_UPSTREAM_ERROR_REPLY.to_owned(),
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
    }));
    response
}
