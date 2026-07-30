//! LLM 上下文字符预算工具。
//!
//! 这里统一做“按字符近似估算”的本地保护，不读取环境变量，也不替代
//! provider 侧真实 token/context window 校验。上层负责把业务输入拆成带
//! retention policy 的预算项，本模块只负责按策略保留、淘汰和生成统一日志。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::error::LlmError;

/// 结构化图片在字符预算中的固定估算成本。
///
/// Data URL 的 Base64 长度反映传输体积，不等同于模型实际占用的文本 token；图片字节数
/// 已由 provider 的 `media_max_bytes` 单独限制。这里仍为每张图片保留固定成本，使图片数量
/// 会进入预算，同时避免把数百 KB 的编码正文误当作用户文本。
const STRUCTURED_IMAGE_ESTIMATED_CHARS: usize = 1024;

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
    debug!(
        stage,
        raw_model_context_chars = initial_estimate.raw_chars,
        budgeted_model_context_chars = initial_estimate.budgeted_chars,
        structured_image_count = initial_estimate.structured_image_count,
        structured_image_data_chars = initial_estimate.structured_image_data_chars,
        structured_image_budget_chars = initial_estimate.structured_image_budget_chars,
        tool_schema_chars = initial_estimate.tool_schema_chars,
        text_and_protocol_chars = initial_estimate.text_and_protocol_chars,
        "tool loop context budget estimate"
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
        "tool loop entered forced finalization budget"
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
    let model_context = if payload.get("input").is_some() {
        json!({"input": payload.get("input"), "tools": payload.get("tools")})
    } else {
        json!({"messages": payload.get("messages"), "tools": payload.get("tools")})
    };
    let raw_chars = estimated_json_chars(&model_context, stage)?;
    let tool_schema_chars = payload
        .get("tools")
        .map(|tools| estimated_json_chars(tools, stage))
        .transpose()?
        .unwrap_or(0);
    let mut budget_view = model_context;
    let mut media = StructuredImageBudget::default();
    mask_structured_image_data_urls(&mut budget_view, &mut media);
    let budgeted_chars = estimated_json_chars(&budget_view, stage)?;
    let structured_image_budget_chars =
        media.count.saturating_mul(STRUCTURED_IMAGE_ESTIMATED_CHARS);

    Ok(ToolLoopBudgetEstimate {
        raw_chars,
        budgeted_chars,
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

fn mask_structured_image_data_urls(value: &mut Value, budget: &mut StructuredImageBudget) {
    match value {
        Value::Object(object) => {
            match object.get("type").and_then(Value::as_str) {
                // OpenAI Responses：{"type":"input_image","image_url":"data:image/..."}
                Some("input_image") => {
                    if let Some(url) = object.get_mut("image_url") {
                        mask_image_data_url(url, budget);
                    }
                }
                // Chat Completions：
                // {"type":"image_url","image_url":{"url":"data:image/..."}}
                Some("image_url") => {
                    if let Some(url) = object
                        .get_mut("image_url")
                        .and_then(Value::as_object_mut)
                        .and_then(|image_url| image_url.get_mut("url"))
                    {
                        mask_image_data_url(url, budget);
                    }
                }
                _ => {}
            }
            for child in object.values_mut() {
                mask_structured_image_data_urls(child, budget);
            }
        }
        Value::Array(items) => {
            for child in items {
                mask_structured_image_data_urls(child, budget);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn mask_image_data_url(value: &mut Value, budget: &mut StructuredImageBudget) {
    let Some(url) = value.as_str().filter(|url| is_base64_image_data_url(url)) else {
        return;
    };
    budget.count += 1;
    budget.data_url_chars = budget.data_url_chars.saturating_add(url.chars().count());
    // 预算视图只替换估算副本；返回给 provider 的原始 payload 不会被修改。
    *value = Value::String("i".repeat(STRUCTURED_IMAGE_ESTIMATED_CHARS));
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
pub fn estimated_json_chars<T: Serialize>(
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
            "context budget exceeded"
        );
    } else if report.evicted_chars > 0 {
        debug!(
            scope,
            max_input_chars = report.max_input_chars,
            output_reserve_chars = report.output_reserve_chars,
            retained_chars = report.retained_chars,
            evicted_chars = report.evicted_chars,
            evicted_items,
            "context budget evicted input items"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serializer;

    fn config(limit: usize) -> ContextBudgetConfig {
        ContextBudgetConfig {
            context_window_chars: limit + 10,
            output_reserve_chars: 10,
            protected_recent_turns: 1,
        }
    }

    #[test]
    fn evicts_by_kind_priority_and_keeps_original_order() {
        let items = vec![
            BudgetItem::new(BudgetItemKind::Required, "system", 20),
            BudgetItem::new(BudgetItemKind::Knowledge, "knowledge", 30),
            BudgetItem::new(BudgetItemKind::Memory, "memory", 30),
            BudgetItem::new(BudgetItemKind::OldHistory, "old", 30),
            BudgetItem::new(BudgetItemKind::Session, "session", 30),
            BudgetItem::new(BudgetItemKind::RecentHistoryProtected, "recent", 20),
            BudgetItem::new(BudgetItemKind::Required, "user", 20),
        ];

        let budgeted = apply_context_budget(items, config(90)).unwrap();

        assert_eq!(budgeted.items, vec!["system", "memory", "recent", "user"]);
        assert_eq!(budgeted.report.evicted_chars, 90);
    }

    #[test]
    fn protected_items_exceeding_limit_returns_context_budget_error() {
        let items = vec![
            BudgetItem::new(BudgetItemKind::Required, "system", 60),
            BudgetItem::new(BudgetItemKind::RecentHistoryProtected, "recent", 60),
            BudgetItem::new(BudgetItemKind::OldHistory, "old", 10),
        ];

        let err = apply_context_budget(items, config(100)).unwrap_err();

        assert_eq!(err.code, "context_budget_exceeded");
        assert_eq!(err.stage, "context_budget");
    }

    #[test]
    fn reserve_must_be_smaller_than_context_window() {
        let err = ContextBudgetConfig {
            context_window_chars: 100,
            output_reserve_chars: 100,
            protected_recent_turns: 1,
        }
        .validate()
        .unwrap_err();

        assert_eq!(err.code, "config");
    }

    #[test]
    fn tool_loop_compaction_keeps_call_and_result_pairing() {
        let payload = json!({
            "messages": [
                {"role": "user", "content": "请查资料"},
                {"role": "assistant", "tool_calls": [{"id": "call-1", "type": "function", "function": {"name": "knowledge_search", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "call-1", "content": "重要证据".repeat(80)}
            ],
            "tools": [{"type": "function", "function": {"name": "knowledge_search"}}],
            "tool_choice": "auto"
        });
        let (fitted, disabled) = fit_tool_loop_payload(
            ContextBudgetConfig {
                context_window_chars: 420,
                output_reserve_chars: 40,
                protected_recent_turns: 0,
            },
            payload,
            "tool_loop",
        )
        .unwrap();
        assert!(disabled);
        assert_eq!(fitted["messages"][1]["tool_calls"][0]["id"], "call-1");
        assert_eq!(fitted["messages"][2]["tool_call_id"], "call-1");
        assert!(fitted.get("tools").is_none());
        assert!(fitted.get("tool_choice").is_none());
        assert!(
            fitted["messages"][2]["content"]
                .as_str()
                .unwrap()
                .contains("工具结果已省略")
        );
    }

    #[test]
    fn short_chat_tool_outputs_are_compacted_as_strings_and_keep_pairing() {
        let payload = json!({
            "messages": [
                {"role": "user", "content": "查资料"},
                {"role": "assistant", "tool_calls": [
                    {"id": "call-1", "type": "function", "function": {"name": "search", "arguments": "{}"}},
                    {"id": "call-2", "type": "function", "function": {"name": "weather", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "call-1", "content": "a".repeat(80)},
                {"role": "tool", "tool_call_id": "call-2", "content": "b".repeat(80)}
            ],
            "tools": [{"type": "function", "function": {"name": "search"}}],
            "tool_choice": "auto",
            "parallel_tool_calls": false
        });
        let (fitted, disabled) = fit_tool_loop_payload(
            ContextBudgetConfig {
                context_window_chars: 440,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            },
            payload,
            "tool_loop",
        )
        .unwrap();

        assert!(disabled);
        assert!(fitted.get("tools").is_none());
        assert!(fitted.get("tool_choice").is_none());
        assert!(fitted.get("parallel_tool_calls").is_none());
        assert_eq!(fitted["messages"][1]["tool_calls"][0]["id"], "call-1");
        assert_eq!(fitted["messages"][1]["tool_calls"][1]["id"], "call-2");
        for (index, call_id) in [(2, "call-1"), (3, "call-2")] {
            assert_eq!(fitted["messages"][index]["tool_call_id"], call_id);
            assert!(fitted["messages"][index]["content"].is_string());
            assert!(
                fitted["messages"][index]["content"]
                    .as_str()
                    .unwrap()
                    .contains("工具结果已省略")
            );
        }
        serde_json::to_string(&fitted).unwrap();
    }

    #[test]
    fn short_responses_tool_outputs_are_compacted_as_strings_and_keep_pairing() {
        let payload = json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "查资料"}]},
                {"type": "function_call", "call_id": "call-1", "name": "search", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call-1", "output": "a".repeat(80)},
                {"type": "function_call", "call_id": "call-2", "name": "weather", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call-2", "output": "b".repeat(80)}
            ],
            "tools": [{"type": "function", "name": "search"}],
            "parallel_tool_calls": false
        });
        let (fitted, disabled) = fit_tool_loop_payload(
            ContextBudgetConfig {
                context_window_chars: 445,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            },
            payload,
            "tool_loop",
        )
        .unwrap();

        assert!(disabled);
        assert!(fitted.get("tools").is_none());
        assert!(fitted.get("tool_choice").is_none());
        assert!(fitted.get("parallel_tool_calls").is_none());
        for (index, call_id) in [(2, "call-1"), (4, "call-2")] {
            assert_eq!(fitted["input"][index]["call_id"], call_id);
            assert!(fitted["input"][index]["output"].is_string());
            assert!(
                fitted["input"][index]["output"]
                    .as_str()
                    .unwrap()
                    .contains("工具结果已省略")
            );
        }
        serde_json::to_string(&fitted).unwrap();
    }

    #[test]
    fn responses_tool_loop_counts_structured_images_without_base64_body() {
        let first_data_url = format!("data:image/png;base64,{}", "a".repeat(80_000));
        let second_data_url = format!("data:image/jpeg;base64,{}", "b".repeat(90_000));
        let payload = json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "依次看这两张图"},
                    {"type": "input_image", "image_url": first_data_url},
                    {"type": "input_image", "image_url": second_data_url}
                ]
            }],
            "tools": [{"type": "function", "name": "inspect"}]
        });

        let (fitted, disabled) = fit_tool_loop_payload(config(4_000), payload.clone(), "tool_loop")
            .expect("structured image data should use the independent media estimate");

        assert!(!disabled);
        assert_eq!(
            fitted, payload,
            "the real provider payload must stay intact"
        );
        let estimate = tool_loop_budget_estimate(&fitted, "tool_loop").unwrap();
        assert_eq!(estimate.structured_image_count, 2);
        assert!(estimate.structured_image_data_chars > 160_000);
        assert_eq!(estimate.structured_image_budget_chars, 2_048);
        assert!(estimate.budgeted_chars < 4_000);
    }

    #[test]
    fn chat_tool_loop_counts_structured_images_without_base64_body() {
        let payload = json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "看图"},
                    {"type": "image_url", "image_url": {
                        "url": format!("data:image/webp;base64,{}", "a".repeat(100_000))
                    }}
                ]
            }],
            "tools": [{"type": "function", "function": {"name": "inspect"}}]
        });

        let (fitted, disabled) = fit_tool_loop_payload(config(2_500), payload.clone(), "tool_loop")
            .expect("chat image data should use the same independent media estimate");

        assert!(!disabled);
        assert_eq!(fitted, payload);
        let estimate = tool_loop_budget_estimate(&fitted, "tool_loop").unwrap();
        assert_eq!(estimate.structured_image_count, 1);
        assert_eq!(estimate.structured_image_budget_chars, 1_024);
    }

    #[test]
    fn plain_text_data_url_is_not_exempt_from_tool_loop_budget() {
        let payload = json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("data:image/png;base64,{}", "a".repeat(8_000))
                }]
            }],
            "tools": []
        });

        let err = fit_tool_loop_payload(config(2_000), payload, "tool_loop").unwrap_err();

        assert_eq!(err.code, "context_budget_exceeded");
        assert_eq!(err.stage, "tool_loop");
    }

    #[test]
    fn unrelated_image_url_field_is_not_exempt_from_tool_loop_budget() {
        let payload = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "image_url": {"url": format!("data:image/png;base64,{}", "a".repeat(8_000))}
                }]
            }],
            "tools": []
        });

        let err = fit_tool_loop_payload(config(2_000), payload, "tool_loop").unwrap_err();

        assert_eq!(err.code, "context_budget_exceeded");
    }

    struct FailingSerialize;

    impl Serialize for FailingSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(serde::ser::Error::custom("serialize failed"))
        }
    }

    #[test]
    fn estimated_json_chars_returns_error_on_serialize_failure() {
        let err = estimated_json_chars(&FailingSerialize, "context_budget").unwrap_err();

        assert_eq!(err.code, "context_budget_estimate_error");
        assert_eq!(err.stage, "context_budget");
        assert!(err.message.contains("failed to estimate JSON chars"));
    }
}
