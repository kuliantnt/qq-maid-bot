//! LLM 上下文字符预算工具。
//!
//! 这里统一做“按字符近似估算”的本地保护，不读取环境变量，也不替代
//! provider 侧真实 token/context window 校验。上层负责把业务输入拆成带
//! retention policy 的预算项，本模块只负责按策略保留、淘汰和生成统一日志。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use crate::error::LlmError;

/// 结构化图片在字符预算中的固定估算成本。
///
/// Data URL 的 Base64 长度反映传输体积，不等同于模型实际占用的文本 token；图片字节数
/// 已由 provider 的 `media_max_bytes` 单独限制。这里仍为每张图片保留固定成本，使图片数量
/// 会进入预算，同时避免把数百 KB 的编码正文误当作用户文本。
const STRUCTURED_IMAGE_ESTIMATED_CHARS: usize = 1024;

/// Tool Loop 大上下文告警阈值（估算字符数）。
///
/// Issue #361 建议 `estimated_request_chars > 100_000` 时输出 warn；这里在
/// 请求发送前按同一语义告警，便于定位是哪一类输入（工具结果、历史、知识证据）
/// 把上下文推大。只输出尺寸与计数，不输出正文。
const LARGE_TOOL_LOOP_WARN_CHARS: usize = 100_000;

/// 上下文预算配置。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBudgetConfig {
    /// 模型上下文窗口的本地字符估算上限。
    pub context_window_chars: usize,
    /// 为输出预留的字符估算空间；有效输入预算为 window - reserve。
    pub output_reserve_chars: usize,
    /// 普通聊天中保护的最近完整 user/assistant 轮次数。
    pub protected_recent_turns: usize,
}

impl ContextBudgetConfig {
    pub fn effective_input_limit(self) -> usize {
        self.context_window_chars
            .saturating_sub(self.output_reserve_chars)
    }

    pub fn validate(self) -> Result<(), LlmError> {
        if self.context_window_chars == 0 {
            return Err(LlmError::config(
                "AGENT_CONTEXT_CHAR_LIMIT must be a positive integer",
            ));
        }
        if self.output_reserve_chars >= self.context_window_chars {
            return Err(LlmError::config(
                "AGENT_CONTEXT_OUTPUT_RESERVE_CHARS must be smaller than AGENT_CONTEXT_CHAR_LIMIT",
            ));
        }
        Ok(())
    }
}

/// 预算单位。首期只做字符估算，避免引入 provider 特定 tokenizer。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BudgetUnit {
    Chars,
}

/// 预算项的业务类型；保留策略由 kind 唯一决定，避免出现互相矛盾的配置。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BudgetItemKind {
    Required,
    HistorySummary,
    RecentHistoryProtected,
    OldHistory,
    Knowledge,
    Session,
    Memory,
    ToolSchema,
    ToolLoopAtomicTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetentionPolicy {
    Required,
    Protected,
    Evictable { priority: u8 },
}

impl BudgetItemKind {
    fn retention_policy(self) -> RetentionPolicy {
        match self {
            Self::Required | Self::ToolSchema | Self::ToolLoopAtomicTurn => {
                RetentionPolicy::Required
            }
            Self::HistorySummary | Self::RecentHistoryProtected => RetentionPolicy::Protected,
            Self::OldHistory => RetentionPolicy::Evictable { priority: 0 },
            Self::Knowledge => RetentionPolicy::Evictable { priority: 1 },
            Self::Session => RetentionPolicy::Evictable { priority: 2 },
            Self::Memory => RetentionPolicy::Evictable { priority: 3 },
        }
    }
}

/// 预算处理动作。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAction {
    Retained,
    Evicted,
    SummaryReused,
    RequiredExceeded,
}

/// 带估算成本的预算项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetItem<T> {
    pub kind: BudgetItemKind,
    pub value: T,
    pub estimated_chars: usize,
}

impl<T> BudgetItem<T> {
    pub fn new(kind: BudgetItemKind, value: T, estimated_chars: usize) -> Self {
        Self {
            kind,
            value,
            estimated_chars,
        }
    }
}

/// 单条预算日志。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BudgetLogEntry {
    pub kind: BudgetItemKind,
    pub action: BudgetAction,
    pub chars: usize,
}

/// 预算处理结果摘要。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BudgetReport {
    pub unit: BudgetUnit,
    pub max_input_chars: usize,
    pub output_reserve_chars: usize,
    pub retained_chars: usize,
    pub evicted_chars: usize,
    pub actions: Vec<BudgetLogEntry>,
}

impl BudgetReport {
    pub fn exceeded(&self) -> bool {
        self.actions
            .iter()
            .any(|entry| entry.action == BudgetAction::RequiredExceeded)
    }
}

/// 预算处理后的值列表与日志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budgeted<T> {
    pub items: Vec<T>,
    pub report: BudgetReport,
}

/// 按 retention policy 应用预算。可淘汰项按 kind 优先级淘汰，最终保留项保持原始顺序。
pub fn apply_context_budget<T>(
    items: Vec<BudgetItem<T>>,
    config: ContextBudgetConfig,
) -> Result<Budgeted<T>, LlmError> {
    config.validate()?;
    let max_input_chars = config.effective_input_limit();
    let mut retained = vec![true; items.len()];
    let mut total_chars = items.iter().map(|item| item.estimated_chars).sum::<usize>();
    let mut evicted_chars = 0usize;
    let mut actions = Vec::new();

    let protected_chars = items
        .iter()
        .filter(|item| {
            matches!(
                item.kind.retention_policy(),
                RetentionPolicy::Required | RetentionPolicy::Protected
            )
        })
        .map(|item| item.estimated_chars)
        .sum::<usize>();

    if protected_chars > max_input_chars {
        actions.extend(items.iter().map(|item| BudgetLogEntry {
            kind: item.kind,
            action: if matches!(
                item.kind.retention_policy(),
                RetentionPolicy::Required | RetentionPolicy::Protected
            ) {
                BudgetAction::RequiredExceeded
            } else {
                BudgetAction::Retained
            },
            chars: item.estimated_chars,
        }));
        let report = BudgetReport {
            unit: BudgetUnit::Chars,
            max_input_chars,
            output_reserve_chars: config.output_reserve_chars,
            retained_chars: total_chars,
            evicted_chars: 0,
            actions,
        };
        return Err(context_budget_exceeded(&report, "context_budget"));
    }

    if total_chars > max_input_chars {
        let mut candidates = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item.kind.retention_policy() {
                RetentionPolicy::Evictable { priority } => Some((priority, index)),
                RetentionPolicy::Required | RetentionPolicy::Protected => None,
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(priority, index)| (*priority, *index));

        for (_, index) in candidates {
            if total_chars <= max_input_chars {
                break;
            }
            retained[index] = false;
            total_chars = total_chars.saturating_sub(items[index].estimated_chars);
            evicted_chars += items[index].estimated_chars;
        }
    }

    for (index, item) in items.iter().enumerate() {
        actions.push(BudgetLogEntry {
            kind: item.kind,
            action: if retained[index] {
                BudgetAction::Retained
            } else {
                BudgetAction::Evicted
            },
            chars: item.estimated_chars,
        });
    }

    if total_chars > max_input_chars {
        for entry in &mut actions {
            if entry.action == BudgetAction::Retained {
                entry.action = BudgetAction::RequiredExceeded;
            }
        }
        let report = BudgetReport {
            unit: BudgetUnit::Chars,
            max_input_chars,
            output_reserve_chars: config.output_reserve_chars,
            retained_chars: total_chars,
            evicted_chars,
            actions,
        };
        return Err(context_budget_exceeded(&report, "context_budget"));
    }

    let report = BudgetReport {
        unit: BudgetUnit::Chars,
        max_input_chars,
        output_reserve_chars: config.output_reserve_chars,
        retained_chars: total_chars,
        evicted_chars,
        actions,
    };
    let items = items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| retained[index].then_some(item.value))
        .collect();
    Ok(Budgeted { items, report })
}

/// 检查一组不可淘汰输入是否满足预算，Tool Loop 首期使用该语义。
pub fn ensure_required_budget(
    config: ContextBudgetConfig,
    kind: BudgetItemKind,
    estimated_chars: usize,
    stage: &'static str,
) -> Result<BudgetReport, LlmError> {
    config.validate()?;
    let max_input_chars = config.effective_input_limit();
    let exceeded = estimated_chars > max_input_chars;
    let report = BudgetReport {
        unit: BudgetUnit::Chars,
        max_input_chars,
        output_reserve_chars: config.output_reserve_chars,
        retained_chars: estimated_chars,
        evicted_chars: 0,
        actions: vec![BudgetLogEntry {
            kind,
            action: if exceeded {
                BudgetAction::RequiredExceeded
            } else {
                BudgetAction::Retained
            },
            chars: estimated_chars,
        }],
    };
    if exceeded {
        Err(context_budget_exceeded(&report, stage))
    } else {
        Ok(report)
    }
}

/// 为 Tool Loop 计算一次可发送的上下文。
///
/// 工具结果是可压缩的输入；当它们把输入推过 `window - output_reserve` 时，
/// 先裁剪结果并关闭后续工具调用，给最终回答留出既定 reserve。只有用户历史和
/// 必须保留的协议内容本身仍然超限时，才返回 `context_budget_exceeded`。
pub fn fit_tool_loop_payload(
    config: ContextBudgetConfig,
    mut payload: Value,
    stage: &'static str,
) -> Result<(Value, bool), LlmError> {
    config.validate()?;
    let max_input_chars = config.effective_input_limit();
    let estimate = |value: &Value| {
        tool_loop_budget_estimate(value, stage).map(|estimate| estimate.budgeted_chars)
    };
    let initial_estimate = tool_loop_budget_estimate(&payload, stage)?;
    if initial_estimate.budgeted_chars > LARGE_TOOL_LOOP_WARN_CHARS {
        let input_item_count = match payload.get("input").or_else(|| payload.get("messages")) {
            Some(Value::Array(items)) => items.len(),
            _ => 0,
        };
        warn!(
            stage,
            estimated_request_chars = initial_estimate.budgeted_chars,
            tool_schema_chars = initial_estimate.tool_schema_chars,
            text_and_protocol_chars = initial_estimate.text_and_protocol_chars,
            structured_image_count = initial_estimate.structured_image_count,
            input_item_count,
            "检测到较大的 Tool Loop 输入"
        );
    }
    debug!(
        stage,
        raw_model_context_chars = initial_estimate.raw_chars,
        budgeted_model_context_chars = initial_estimate.budgeted_chars,
        structured_image_count = initial_estimate.structured_image_count,
        structured_image_data_chars = initial_estimate.structured_image_data_chars,
        structured_image_budget_chars = initial_estimate.structured_image_budget_chars,
        tool_schema_chars = initial_estimate.tool_schema_chars,
        text_and_protocol_chars = initial_estimate.text_and_protocol_chars,
        "Tool Loop 上下文预算估算完成"
    );
    if initial_estimate.budgeted_chars <= max_input_chars {
        return Ok((payload, false));
    }

    // 工具定义只服务于下一次工具调用；进入收尾轮后移除所有工具控制字段，
    // 避免空 tools 与 tool_choice=none 在部分兼容 Provider 上组成非法组合。
    if let Some(object) = payload.as_object_mut() {
        object.remove("tools");
        object.remove("tool_choice");
        object.remove("parallel_tool_calls");
    }
    compact_tool_outputs(&mut payload, max_input_chars, &estimate)?;
    let retained_chars = estimate(&payload)?;
    if retained_chars > max_input_chars {
        let report = BudgetReport {
            unit: BudgetUnit::Chars,
            max_input_chars,
            output_reserve_chars: config.output_reserve_chars,
            retained_chars,
            evicted_chars: 0,
            actions: vec![BudgetLogEntry {
                kind: BudgetItemKind::ToolLoopAtomicTurn,
                action: BudgetAction::RequiredExceeded,
                chars: retained_chars,
            }],
        };
        return Err(context_budget_exceeded(&report, stage));
    }
    let report = BudgetReport {
        unit: BudgetUnit::Chars,
        max_input_chars,
        output_reserve_chars: config.output_reserve_chars,
        retained_chars,
        evicted_chars: max_input_chars.saturating_sub(retained_chars),
        actions: vec![BudgetLogEntry {
            kind: BudgetItemKind::ToolLoopAtomicTurn,
            action: BudgetAction::Evicted,
            chars: retained_chars,
        }],
    };
    log_budget_report(stage, &report);
    tracing::debug!(
        stage,
        retained_chars,
        max_input_chars,
        output_reserve_chars = config.output_reserve_chars,
        tools_disabled = true,
        "Tool Loop 已进入强制最终回答预算阶段"
    );
    Ok((payload, true))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ToolLoopBudgetEstimate {
    raw_chars: usize,
    budgeted_chars: usize,
    structured_image_count: usize,
    structured_image_data_chars: usize,
    structured_image_budget_chars: usize,
    tool_schema_chars: usize,
    text_and_protocol_chars: usize,
}

fn tool_loop_budget_estimate(
    payload: &Value,
    stage: &'static str,
) -> Result<ToolLoopBudgetEstimate, LlmError> {
    // 只估算真正进入模型上下文的字段；model、stream、max token 等传输控制字段不计入。
    // 直接序列化 input/messages 与 tools 两个借用字段，避免为估算再构造一份
    // model_context 副本和一份掩码后的 budget_view 副本（Issue #361 内存放大点）。
    let input_slot = if payload.get("input").is_some() {
        payload.get("input")
    } else {
        payload.get("messages")
    };
    let input_chars = input_slot
        .map(|value| estimated_json_chars(value, stage))
        .transpose()?
        .unwrap_or(0);
    let tool_schema_chars = payload
        .get("tools")
        .map(|tools| estimated_json_chars(tools, stage))
        .transpose()?
        .unwrap_or(0);
    let raw_chars = input_chars.saturating_add(tool_schema_chars);
    let mut media = StructuredImageBudget::default();
    if let Some(input) = input_slot {
        count_structured_image_data_urls(input, &mut media);
    }
    if let Some(tools) = payload.get("tools") {
        count_structured_image_data_urls(tools, &mut media);
    }
    let structured_image_budget_chars =
        media.count.saturating_mul(STRUCTURED_IMAGE_ESTIMATED_CHARS);

    Ok(ToolLoopBudgetEstimate {
        raw_chars,
        budgeted_chars: raw_chars
            .saturating_sub(media.data_url_chars)
            .saturating_add(structured_image_budget_chars),
        structured_image_count: media.count,
        structured_image_data_chars: media.data_url_chars,
        structured_image_budget_chars,
        tool_schema_chars,
        // 这是“文本 + JSON 协议开销”，不会记录或输出任何正文内容。
        text_and_protocol_chars: raw_chars
            .saturating_sub(media.data_url_chars)
            .saturating_sub(tool_schema_chars),
    })
}

#[derive(Debug, Default)]
struct StructuredImageBudget {
    count: usize,
    data_url_chars: usize,
}

/// 只统计 base64 图片 data URL 的字符数，不复制或修改 payload。
///
/// 预算视图无需真的替换字符串：`budgeted = raw - data_url_chars + count * 1024`
/// 与旧实现“掩码后重新序列化”的结果一致，且不会为每轮请求多保留两份大对象。
fn count_structured_image_data_urls(value: &Value, budget: &mut StructuredImageBudget) {
    match value {
        Value::Object(object) => {
            match object.get("type").and_then(Value::as_str) {
                // OpenAI Responses：{"type":"input_image","image_url":"data:image/..."}
                Some("input_image") => {
                    if let Some(url) = object.get("image_url").and_then(Value::as_str) {
                        count_image_data_url(url, budget);
                    }
                }
                // Chat Completions：
                // {"type":"image_url","image_url":{"url":"data:image/..."}}
                Some("image_url") => {
                    if let Some(url) = object
                        .get("image_url")
                        .and_then(Value::as_object)
                        .and_then(|image_url| image_url.get("url"))
                        .and_then(Value::as_str)
                    {
                        count_image_data_url(url, budget);
                    }
                }
                _ => {}
            }
            for child in object.values() {
                count_structured_image_data_urls(child, budget);
            }
        }
        Value::Array(items) => {
            for child in items {
                count_structured_image_data_urls(child, budget);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn count_image_data_url(url: &str, budget: &mut StructuredImageBudget) {
    if !is_base64_image_data_url(url) {
        return;
    }
    budget.count += 1;
    budget.data_url_chars = budget.data_url_chars.saturating_add(url.chars().count());
}

fn is_base64_image_data_url(value: &str) -> bool {
    let Some((header, _data)) = value.trim().split_once(',') else {
        return false;
    };
    let header = header.to_ascii_lowercase();
    header.starts_with("data:image/") && header.ends_with(";base64")
}

fn compact_tool_outputs(
    value: &mut Value,
    max_input_chars: usize,
    estimate: &impl Fn(&Value) -> Result<usize, LlmError>,
) -> Result<(), LlmError> {
    let mut compacted_paths = std::collections::HashSet::new();
    while estimate(value)? > max_input_chars {
        let mut candidates = Vec::new();
        collect_tool_output_paths(value, &mut Vec::new(), &mut candidates);
        let Some((path, _current_len)) = candidates
            .into_iter()
            .filter(|(path, _)| !compacted_paths.contains(path))
            .max_by_key(|(_, len)| *len)
        else {
            break;
        };
        let Some(slot) = value.pointer_mut(&path) else {
            break;
        };
        let Some(text) = slot.as_str() else { break };
        let original_chars = text.chars().count();
        let marker = format!("[工具结果已省略，原始长度 {original_chars} 字符]");
        // 工具输出字段的协议类型不能改变：Chat Completions 的 content 和
        // Responses 的 function_call_output.output 都必须继续是字符串。
        *slot = Value::String(marker);
        compacted_paths.insert(path);
    }
    Ok(())
}

fn collect_tool_output_paths(
    value: &Value,
    path: &mut Vec<String>,
    output: &mut Vec<(String, usize)>,
) {
    match value {
        Value::Object(map) => {
            let is_tool = map.get("type").and_then(Value::as_str) == Some("function_call_output")
                || map.get("role").and_then(Value::as_str) == Some("tool");
            for (key, child) in map {
                path.push(key.clone());
                if is_tool
                    && (key == "output" || key == "content")
                    && let Some(text) = child.as_str()
                {
                    output.push((
                        format!(
                            "/{}",
                            path.iter()
                                .map(|p| p.replace('~', "~0").replace('/', "~1"))
                                .collect::<Vec<_>>()
                                .join("/")
                        ),
                        text.chars().count(),
                    ));
                }
                collect_tool_output_paths(child, path, output);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(index.to_string());
                collect_tool_output_paths(child, path, output);
                path.pop();
            }
        }
        _ => {}
    }
}

pub fn context_budget_exceeded(report: &BudgetReport, stage: &'static str) -> LlmError {
    log_budget_report(stage, report);
    LlmError::new(
        "context_budget_exceeded",
        format!(
            "context budget exceeded: retained {} chars, evicted {} chars, max input {} chars, output reserve {} chars",
            report.retained_chars,
            report.evicted_chars,
            report.max_input_chars,
            report.output_reserve_chars
        ),
        stage,
    )
}

/// 估算 JSON 序列化后的字符数；失败时必须显式返回错误，不能按 0 字符放行请求。
pub fn estimated_json_chars<T: Serialize + ?Sized>(
    value: &T,
    stage: &'static str,
) -> Result<usize, LlmError> {
    let text = serde_json::to_string(value).map_err(|err| {
        LlmError::new(
            "context_budget_estimate_error",
            format!("failed to estimate JSON chars for context budget: {err}"),
            stage,
        )
    })?;
    #[cfg(test)]
    if text.contains("__force_json_estimate_error__") {
        return Err(LlmError::new(
            "context_budget_estimate_error",
            "failed to estimate JSON chars for context budget: forced test error",
            stage,
        ));
    }
    Ok(text.chars().count())
}

/// 不保留正文的 JSON 字符计数 writer。
///
/// `serde_json::to_writer` 会把序列化结果以 UTF-8 字节流写入；本 writer 只累计
/// 完整字符数，不在堆上生成完整 String 副本。多字节 UTF-8 序列可能跨 `write`
/// 分片，内部用 4 字节缓冲暂存未完成的序列（serde_json 输出的 JSON 一定是
/// 合法 UTF-8，无需校验 continuation 字节）。
#[derive(Debug, Default)]
pub struct JsonCharCounter {
    chars: usize,
    partial: [u8; 4],
    partial_len: usize,
}

impl JsonCharCounter {
    /// 已累计的完整 UTF-8 字符数。
    pub fn chars(&self) -> usize {
        self.chars
    }
}

impl std::io::Write for JsonCharCounter {
    fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
        let written = buf.len();
        // 先补全上一片未完成的 UTF-8 序列。
        if self.partial_len > 0 {
            let first = self.partial[0];
            let total = utf8_sequence_len(first);
            let take = total.saturating_sub(self.partial_len).min(buf.len());
            self.partial[self.partial_len..self.partial_len + take].copy_from_slice(&buf[..take]);
            self.partial_len += take;
            buf = &buf[take..];
            if self.partial_len == total {
                self.chars += 1;
                self.partial_len = 0;
            } else {
                // 这一片仍凑不齐完整序列，等待下一片。
                return Ok(written);
            }
        }
        while !buf.is_empty() {
            let first = buf[0];
            let len = utf8_sequence_len(first);
            if buf.len() < len {
                self.partial[..buf.len()].copy_from_slice(buf);
                self.partial_len = buf.len();
                break;
            }
            self.chars += 1;
            buf = &buf[len..];
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 根据 UTF-8 首字节推断完整序列长度（1-4 字节）。
fn utf8_sequence_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// 估算 JSON 序列化后的字符数，但不保留序列化正文。
///
/// 与 [`estimated_json_chars`] 的差异仅在实现方式：序列化直接写入计数 writer，
/// 堆上不保留完整 String 副本，专供 DEBUG 诊断使用，避免诊断本身抬高
/// allocator / RSS 高水位（Issue #361）。失败语义与 `estimated_json_chars`
/// 一致：序列化失败必须显式返回错误，不能按 0 字符放行。
pub fn estimated_json_chars_counting<T: Serialize + ?Sized>(
    value: &T,
    stage: &'static str,
) -> Result<usize, LlmError> {
    let mut writer = JsonCharCounter::default();
    serde_json::to_writer(&mut writer, value).map_err(|err| {
        LlmError::new(
            "context_budget_estimate_error",
            format!("failed to estimate JSON chars for context budget: {err}"),
            stage,
        )
    })?;
    Ok(writer.chars())
}

pub fn log_budget_report(scope: &'static str, report: &BudgetReport) {
    let evicted_items = report
        .actions
        .iter()
        .filter(|entry| entry.action == BudgetAction::Evicted)
        .count();
    if report.exceeded() {
        warn!(
            scope,
            max_input_chars = report.max_input_chars,
            output_reserve_chars = report.output_reserve_chars,
            retained_chars = report.retained_chars,
            evicted_chars = report.evicted_chars,
            evicted_items,
            "上下文超出预算"
        );
    } else if report.evicted_chars > 0 {
        debug!(
            scope,
            max_input_chars = report.max_input_chars,
            output_reserve_chars = report.output_reserve_chars,
            retained_chars = report.retained_chars,
            evicted_chars = report.evicted_chars,
            evicted_items,
            "上下文预算已移除部分输入项"
        );
    }
}

#[cfg(test)]
mod tests;
