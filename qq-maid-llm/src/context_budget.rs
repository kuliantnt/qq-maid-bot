//! LLM 上下文字符预算工具。
//!
//! 这里统一做“按字符近似估算”的本地保护，不读取环境变量，也不替代
//! provider 侧真实 token/context window 校验。上层负责把业务输入拆成带
//! retention policy 的预算项，本模块只负责按策略保留、淘汰和生成统一日志。

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::LlmError;

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
            Self::RecentHistoryProtected => RetentionPolicy::Protected,
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
