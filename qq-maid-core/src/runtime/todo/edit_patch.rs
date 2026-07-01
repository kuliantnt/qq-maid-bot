//! Todo 编辑增量补丁的共享类型。
//!
//! 指令侧（`/todo edit` flow）和工具调用侧（`edit_todo` Tool）各自维护过
//! 一份字段相同的 `TodoEditPatch`，并将“补丁应用到草稿”的逻辑各写一遍。
//! 这里把类型和 apply 逻辑收敛到一处，两侧 parse 函数仍按各自来源
//! （LLM JSON / 工具结构化参数）单独实现，但产出统一的 `TodoEditPatch`。
//!
//! 行为不变量：两侧 parse 阶段都已对字符串字段做 trim + 空归 None，
//! 因此 `apply_to_draft` 不再重复 `clean_string`，与历史实现的最终结果一致。
//! 如果未来 parse 路径允许未清洗字段进入补丁，必须在这里补回清洗逻辑，
//! 否则空白标题/详情会直接写入待办。

use serde::{Deserialize, Serialize};

use crate::runtime::todo::{TodoItemDraft, TodoTimePrecision};

/// 待办编辑操作的增量补丁，只包含需要修改的字段。
///
/// `Serialize/Deserialize` 用于工具调用侧 prepare 阶段把补丁序列化进
/// 预解析参数，再在 execute 阶段反序列化恢复；指令侧不需要序列化，
/// 但 derive 无副作用，统一派生避免两份定义。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoEditPatch {
    /// 新的标题；未明确修改时为 None。
    pub title: Option<String>,
    /// 新的详情/备注；未明确修改时为 None。
    pub detail: Option<String>,
    /// 新的截止日期（YYYY-MM-DD）；未明确修改时为 None。
    pub due_date: Option<String>,
    /// 新的截止时间（YYYY-MM-DD HH:MM:SS 或 RFC3339）；未明确修改时为 None。
    pub due_at: Option<String>,
    /// 新的时间精度；未明确修改时为 None。
    pub time_precision: Option<TodoTimePrecision>,
}

impl TodoEditPatch {
    /// 是否存在至少一项修改。
    pub fn has_changes(&self) -> bool {
        self.title.is_some()
            || self.detail.is_some()
            || self.due_date.is_some()
            || self.due_at.is_some()
            || self.time_precision.is_some()
    }
}

/// 将编辑补丁应用到现有草稿，返回更新后的草稿。
///
/// 应用规则（与历史两侧一致）：
/// - `due_at` 优先：设置截止时间时，`due_date` 同步覆盖，默认精度为 `DateTime`；
/// - 仅 `due_date` 时清空 `due_at`，默认精度为 `Date`；
/// - 仅 `time_precision` 时只调整精度；
/// - 任何字段存在都把 `raw_text` 刷新为本次用户输入。
///
/// 字段清洗已在 parse 阶段完成，这里不再 `clean_string`，避免重复逻辑
/// 与两侧行为漂移；具体原因见模块顶部不变量说明。
pub fn apply_to_draft(
    mut draft: TodoItemDraft,
    patch: &TodoEditPatch,
    raw_text: &str,
) -> TodoItemDraft {
    if let Some(title) = &patch.title {
        draft.title = title.clone();
    }
    if let Some(detail) = &patch.detail {
        draft.detail = Some(detail.clone());
    }
    if let Some(due_at) = &patch.due_at {
        draft.due_at = Some(due_at.clone());
        // 设置截止时间时同步覆盖截止日期，保持 due_at / due_date 一致。
        draft.due_date = patch.due_date.clone();
        draft.time_precision = patch
            .time_precision
            .clone()
            .unwrap_or(TodoTimePrecision::DateTime);
    } else if let Some(due_date) = &patch.due_date {
        draft.due_date = Some(due_date.clone());
        // 仅设日期时清空时间分量，避免旧 due_at 残留导致精度语义错乱。
        draft.due_at = None;
        draft.time_precision = patch
            .time_precision
            .clone()
            .unwrap_or(TodoTimePrecision::Date);
    } else if let Some(precision) = &patch.time_precision {
        draft.time_precision = precision.clone();
    }
    draft.raw_text = Some(raw_text.to_owned());
    draft
}
