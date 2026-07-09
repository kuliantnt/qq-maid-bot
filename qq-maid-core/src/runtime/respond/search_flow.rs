//! 联网搜索指令的处理流程。
//!
//! `/查` `/查询` `/search` 只负责用户入口兼容、参数校验、session 记录和展示；
//! 实际联网查询统一通过 `runtime/tools/search.rs` 中的 `web_search` Tool 执行。

use std::{collections::HashMap, future::Future, pin::Pin};

use qq_maid_llm::{
    provider::types::{ChatMessage, ChatRequest, ReasoningEffort},
    web_search::WebSearchOutcome,
};
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
// Level 2 进度事件的兼容文本：用户可见，但不写入 session 或模型上下文。
const WEB_SEARCH_RUNNING_DELTA: &str = "正在联网查询中…\n\n";
const WEB_SEARCH_REWRITE_PURPOSE: &str = "search_query_rewrite";
const WEB_SEARCH_REWRITE_MAX_OUTPUT_TOKENS: u64 = 96;
const WEB_SEARCH_REWRITE_CONTEXT_MAX_CHARS: usize = 1200;
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

        let command_text = format!("/{} {}", command.raw_command, command.argument);
        let raw_question = web_search_raw_question(&command_text, req);
        let policy = self.resolve_agent_policy(req)?;
        let search_query = self
            .prepare_web_search_query(query, req, &raw_question, &policy.main_model)
            .await;
        if stream && let Some(on_delta) = on_delta.as_mut() {
            on_delta(WEB_SEARCH_RUNNING_DELTA.to_owned()).await?;
        }
        let output_result = if stream {
            self.execute_web_search_tool_stream(
                &search_query,
                &raw_question,
                Some(policy.search_model.clone()),
                on_delta.take(),
            )
            .await
        } else {
            self.execute_web_search_tool(
                &search_query,
                &raw_question,
                Some(policy.search_model.clone()),
            )
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

    async fn prepare_web_search_query(
        &self,
        query: &str,
        req: &RespondRequest,
        raw_question: &str,
        rewrite_model: &str,
    ) -> String {
        let query = normalize_query_text(query);
        if !should_rewrite_search_query(&query, req) {
            return query;
        }

        match self
            .rewrite_web_search_query(&query, req, raw_question, rewrite_model)
            .await
        {
            Ok(Some(rewritten)) => rewritten,
            Ok(None) => fallback_web_search_query(&query, req),
            Err(err) => {
                tracing::warn!(
                    error_code = err.code,
                    error_stage = err.stage,
                    "web search query rewrite failed; using local compact fallback"
                );
                fallback_web_search_query(&query, req)
            }
        }
    }

    async fn rewrite_web_search_query(
        &self,
        query: &str,
        req: &RespondRequest,
        raw_question: &str,
        rewrite_model: &str,
    ) -> Result<Option<String>, LlmError> {
        let mut metadata = HashMap::new();
        metadata.insert("purpose".to_owned(), WEB_SEARCH_REWRITE_PURPOSE.to_owned());
        let outcome = self
            .provider
            .chat(ChatRequest {
                session_id: req.session_id.clone(),
                model: Some(rewrite_model.to_owned()),
                messages: vec![
                    ChatMessage::system(web_search_rewrite_system_prompt()),
                    ChatMessage::user(web_search_rewrite_user_prompt(query, raw_question)),
                ],
                context_budget: None,
                max_output_tokens: Some(WEB_SEARCH_REWRITE_MAX_OUTPUT_TOKENS),
                reasoning_effort: Some(ReasoningEffort::Low),
                metadata,
            })
            .await?;
        Ok(clean_rewritten_search_query(&outcome.reply))
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

fn should_rewrite_search_query(query: &str, req: &RespondRequest) -> bool {
    if query.chars().count() > WEB_SEARCH_QUERY_MAX_LENGTH {
        return true;
    }
    quoted_search_context(req).is_some() && query_needs_quoted_context(query)
}

fn query_needs_quoted_context(query: &str) -> bool {
    let compact = query
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '，' | '。' | '？' | '?' | '！' | '!'))
        .collect::<String>()
        .to_lowercase();
    [
        "帮我查询",
        "帮我查",
        "帮我看看",
        "帮忙查询",
        "帮忙查",
        "查下",
        "查一下",
        "查询一下",
        "搜下",
        "搜一下",
        "搜索一下",
        "整理下上下文",
        "整理一下上下文",
        "结合上下文",
        "根据上下文",
        "引用内容",
        "这条",
        "这个",
        "上面",
    ]
    .iter()
    .any(|needle| compact.contains(needle))
}

fn web_search_rewrite_system_prompt() -> String {
    format!(
        "你是搜索查询改写器。任务：把用户追问和可能存在的引用上下文整理成一个公开网页搜索 query。\n\
要求：\n\
- 只输出 query 本身，不要解释、不要 Markdown、不要加引号。\n\
- query 必须不超过 {WEB_SEARCH_QUERY_MAX_LENGTH} 个字符。\n\
- 优先保留专有名词、URL、错误码、版本号、日期/时间范围、产品名和用户限制条件。\n\
- 用户只说“帮我查一下”“整理上下文查询一下”时，从引用上下文中抽取可搜索实体和条件。\n\
- 不要把整段聊天原文塞进 query。"
    )
}

fn web_search_rewrite_user_prompt(query: &str, raw_question: &str) -> String {
    format!(
        "用户追问：\n{}\n\n原始问题与引用上下文：\n{}",
        query,
        truncate_chars(raw_question, WEB_SEARCH_REWRITE_CONTEXT_MAX_CHARS)
    )
}

fn clean_rewritten_search_query(output: &str) -> Option<String> {
    if rewrite_output_has_invalid_shape(output) {
        return None;
    }
    let mut text = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("```"))
        .collect::<Vec<_>>()
        .join(" ");
    text = normalize_query_text(&text);
    for prefix in [
        "query:",
        "query：",
        "搜索 query:",
        "搜索 query：",
        "搜索词:",
        "搜索词：",
        "查询词:",
        "查询词：",
        "查询:",
        "查询：",
    ] {
        if text.to_lowercase().starts_with(prefix) {
            text = text[prefix.len()..].trim().to_owned();
        }
    }
    if rewrite_query_is_invalid(&text) {
        None
    } else {
        Some(text)
    }
}

fn fallback_web_search_query(query: &str, req: &RespondRequest) -> String {
    let cleaned_query = strip_search_instruction_shell(query);
    let base = if let Some(quoted) =
        quoted_search_context(req).filter(|_| query_needs_quoted_context(query))
    {
        let quoted = compact_quoted_search_context(&quoted);
        if cleaned_query.is_empty() {
            quoted
        } else {
            format!("{cleaned_query} {quoted}")
        }
    } else if cleaned_query.is_empty() {
        query.to_owned()
    } else {
        cleaned_query
    };
    compact_search_query(&base, WEB_SEARCH_QUERY_MAX_LENGTH)
}

fn normalize_query_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rewrite_output_has_invalid_shape(output: &str) -> bool {
    let non_empty = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if non_empty.len() != 1 {
        return true;
    }
    let line = non_empty[0];
    line.starts_with("```")
        || line.starts_with('#')
        || line.starts_with('>')
        || line.starts_with("- ")
        || line.starts_with("* ")
        || rewrite_line_starts_with_numbered_list_marker(line)
}

fn rewrite_line_starts_with_numbered_list_marker(line: &str) -> bool {
    let mut chars = line.chars();
    let (Some(first), Some(marker)) = (chars.next(), chars.next()) else {
        return false;
    };
    if !first.is_ascii_digit() {
        return false;
    }
    // `1. Rust` 是编号列表；`1.75 Rust` / `4.22.0 Wrangler` 是合法版本号查询。
    match marker {
        '.' | ')' => chars.next().is_some_and(char::is_whitespace),
        '、' => true,
        _ => false,
    }
}

fn rewrite_query_is_invalid(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > WEB_SEARCH_QUERY_MAX_LENGTH {
        return true;
    }
    if is_wrapped_in_quotes(text) {
        return true;
    }
    let lower = text.to_lowercase();
    [
        "我将",
        "我会",
        "我可以",
        "建议搜索",
        "建议查询",
        "建议联网查询",
        "可以搜索",
        "可以查询",
        "以下是",
        "好的",
        "我来",
        "帮你查",
        "来帮你",
        "这是",
        "搜索query是",
        "搜索 query 是",
        "搜索关键词",
        "查询query是",
        "查询 query 是",
        "查询关键词",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_wrapped_in_quotes(text: &str) -> bool {
    matches!(
        (text.chars().next(), text.chars().next_back(),),
        (Some('"'), Some('"'))
            | (Some('\''), Some('\''))
            | (Some('“'), Some('”'))
            | (Some('‘'), Some('’'))
            | (Some('`'), Some('`'))
    )
}

fn strip_search_instruction_shell(query: &str) -> String {
    let mut text = normalize_query_text(query);
    for phrase in [
        "帮我查询一下",
        "帮我查询",
        "帮我查一下",
        "帮我查",
        "帮忙查询一下",
        "帮忙查询",
        "帮忙查一下",
        "帮忙查",
        "整理一下上下文查询一下",
        "整理下上下文查询一下",
        "整理一下上下文",
        "整理下上下文",
        "结合上下文查询一下",
        "根据上下文查询一下",
        "联网查询一下",
        "联网查询",
        "查一下",
        "查询一下",
        "搜一下",
        "搜索一下",
        "看看",
        "看下",
        "这个",
        "这条",
        "上面",
    ] {
        text = text.replace(phrase, " ");
    }
    normalize_query_text(&text)
        .trim_matches(|ch| {
            matches!(
                ch,
                ':' | '：' | ',' | '，' | '.' | '。' | '?' | '？' | '!' | '！'
            )
        })
        .trim()
        .to_owned()
}

fn compact_quoted_search_context(context: &str) -> String {
    let mut selected = context
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            line.contains("http://")
                || line.contains("https://")
                || line.chars().any(|ch| ch.is_ascii_digit())
                || line.chars().any(|ch| ch.is_ascii_uppercase())
                || line.contains("错误")
                || line.contains("报错")
                || line.contains("版本")
                || line.contains("标题")
                || line.contains("发布")
                || line.contains("公告")
        })
        .take(6)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        selected = context
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(3)
            .collect();
    }
    normalize_query_text(&selected.join(" "))
}

fn compact_search_query(text: &str, limit: usize) -> String {
    let text = normalize_query_text(text);
    let total = text.chars().count();
    if total <= limit {
        return text;
    }
    if limit <= 1 {
        return text.chars().take(limit).collect();
    }

    let head_len = (limit * 2 / 3).max(1);
    let tail_len = limit.saturating_sub(head_len + 1).max(1);
    let head = text.chars().take(head_len).collect::<String>();
    let tail = text
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{head} {tail}").trim().to_owned()
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

pub(crate) fn format_web_search_command_reply(answer: &str) -> String {
    let mut text = answer.trim().to_owned();
    if text.is_empty() {
        text = "没查到明确结果。可以换一个关键词再试。".to_owned();
    }
    if !text.starts_with("【联网查询】") {
        text = format!("【联网查询】\n\n{text}");
    }
    truncate_chars(&text, 1500)
}

pub(crate) fn format_web_search_error_reply(err: &LlmError) -> String {
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
