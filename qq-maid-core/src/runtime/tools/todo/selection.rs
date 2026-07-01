//! prepare/execute 共用的预解析选择与结果映射 helper。
//!
//! prepare 阶段把“可见编号 -> 内部 ID”结果序列化进 arguments；
//! execute 阶段优先复用预解析结果，缺失时再现场解析。这里统一维护两条路径，
//! 避免各 Tool 各自手抄 PreboundResolvedSelection 读写。

use std::collections::HashSet;

use serde_json::Value;

use qq_maid_llm::tool::{ToolCallDependency, ToolContext, ToolOutput, ToolPreparation};

use crate::{
    error::LlmError,
    runtime::todo::{TodoEditPatch, TodoItem, TodoStore},
};

use super::common::{
    PREBOUND_EDIT_DRAFT_KEY, PREBOUND_ERROR_OUTPUT_KEY, PREBOUND_SELECTION_KEY,
    PREBOUND_SINGLE_ID_KEY, PREBOUND_SINGLE_LABEL_KEY, TodoSelectionLabel, TodoSelectionRequest,
    bad_tool_arguments, required_non_empty_text, single_todo_selection_request, todo_edit_patch,
    todo_selection_request,
};
use super::scope::{
    ResolvedTodoSelection, TodoToolScope, TodoToolSelectionResolution, TodoToolSingleItemResolution,
};

/// prepare 阶段把 scope.resolve_selection 的结果转换成可序列化的预解析结构。
pub(in crate::runtime::tools::todo) fn resolve_prepared_selection(
    scope: &mut TodoToolScope,
    selection: &TodoSelectionRequest,
    todo_store: &TodoStore,
) -> Result<super::common::PreparedResolvedSelection, LlmError> {
    use super::common::PreparedSelectionMatch;

    let resolved = match scope.resolve_selection(selection, todo_store)? {
        TodoToolSelectionResolution::Resolved(resolved) => resolved,
        // 这里保留业务错误输出，不在 prepare 阶段抛异常，避免直接调用 execute_json
        // 时把原本应返回给模型/测试的结构化失败升级成 Err。
        TodoToolSelectionResolution::Output(output) => {
            return Ok(super::common::PreparedResolvedSelection {
                labels: Vec::new(),
                matched: Vec::new(),
                missing: Vec::new(),
                error_output: Some(output.value),
            });
        }
    };
    Ok(super::common::PreparedResolvedSelection {
        labels: resolved.labels.clone(),
        matched: resolved
            .matched
            .iter()
            .map(|(label, id)| PreparedSelectionMatch {
                label: label.clone(),
                id: id.clone(),
            })
            .collect(),
        missing: resolved.missing.clone(),
        error_output: resolved.error_output.map(|output| output.value),
    })
}

/// 从 execute 阶段收到带预解析的 arguments 里重建选择结果。
pub(in crate::runtime::tools::todo) fn prepared_selection_argument(
    arguments: &Value,
) -> Result<Option<super::common::PreparedResolvedSelection>, LlmError> {
    arguments
        .get(PREBOUND_SELECTION_KEY)
        .cloned()
        .map(|value| {
            serde_json::from_value::<super::common::PreparedResolvedSelection>(value).map_err(
                |err| {
                    LlmError::new(
                        "bad_tool_arguments",
                        format!("invalid prepared selection payload: {err}"),
                        "tool",
                    )
                },
            )
        })
        .transpose()
}

/// 从 execute 阶段的 arguments 解析出最终 ResolvedTodoSelection。
///
/// 优先复用 prepare 阶段写回的预解析；缺失时现场解析，保证直接调用 execute 也成立。
pub(in crate::runtime::tools::todo) fn resolved_selection_from_arguments(
    scope: &mut TodoToolScope,
    todo_store: &TodoStore,
    arguments: &Value,
    allow_many: bool,
) -> Result<ResolvedTodoSelection, LlmError> {
    if let Some(prepared) = prepared_selection_argument(arguments)? {
        return Ok(ResolvedTodoSelection {
            labels: prepared.labels,
            matched: prepared
                .matched
                .into_iter()
                .map(|item| (item.label, item.id))
                .collect(),
            missing: prepared.missing,
            error_output: prepared.error_output.map(ToolOutput::json),
        });
    }
    let selection = if allow_many {
        todo_selection_request(arguments, true)?
    } else {
        single_todo_selection_request(arguments)?
    };
    match scope.resolve_selection(&selection, todo_store)? {
        TodoToolSelectionResolution::Resolved(resolved) => Ok(resolved),
        TodoToolSelectionResolution::Output(output) => Ok(ResolvedTodoSelection {
            labels: Vec::new(),
            matched: Vec::new(),
            missing: Vec::new(),
            error_output: Some(output),
        }),
    }
}

/// 多编号 Tool 的通用 prepare：解析 selection，写回预解析，按 reference/numbers 设置依赖。
///
/// `selection_scope` 为受限 Tool Loop 注入的请求级选择作用域；普通调用传 `None`。
pub(in crate::runtime::tools::todo) fn prepare_selection_arguments(
    session_store: &crate::runtime::session::SessionStore,
    todo_store: &TodoStore,
    context: &ToolContext,
    arguments: Value,
    allow_many: bool,
    selection_scope: Option<super::scope::SelectionScope>,
) -> Result<ToolPreparation, LlmError> {
    let mut scope = TodoToolScope::load(session_store, context, selection_scope)?;
    let selection = if allow_many {
        todo_selection_request(&arguments, true)?
    } else {
        single_todo_selection_request(&arguments)?
    };
    let dependency = match selection {
        TodoSelectionRequest::Reference(_) => ToolCallDependency::PreviousCallSuccess,
        TodoSelectionRequest::Numbers(_) => ToolCallDependency::None,
    };
    let prepared_arguments = match selection {
        TodoSelectionRequest::Numbers(_) => {
            let resolved = resolve_prepared_selection(&mut scope, &selection, todo_store)?;
            let mut prepared = arguments.clone();
            let object = prepared
                .as_object_mut()
                .ok_or_else(|| bad_tool_arguments("tool arguments must be a JSON object"))?;
            object.insert(
                PREBOUND_SELECTION_KEY.to_owned(),
                serde_json::to_value(resolved).map_err(|err| {
                    bad_tool_arguments(format!("failed to encode prepared selection: {err}"))
                })?,
            );
            prepared
        }
        TodoSelectionRequest::Reference(_) => arguments,
    };
    Ok(ToolPreparation::ready(prepared_arguments).with_dependency(dependency))
}

/// 从解析结果取内部 ID 列表。
pub(in crate::runtime::tools::todo) fn prepared_selection_ids(
    resolved: &ResolvedTodoSelection,
) -> Vec<String> {
    resolved.matched.iter().map(|(_, id)| id.clone()).collect()
}

/// edit_todo execute 专用：从 arguments 还原 (item, label, patch, raw_text)。
pub(in crate::runtime::tools::todo) fn prepared_edit_target(
    scope: &mut TodoToolScope,
    todo_store: &TodoStore,
    arguments: &Value,
) -> Result<TodoToolSingleItemResolutionWithDraft, LlmError> {
    use super::common::TODO_SELECTION_NOT_FOUND_CODE;

    if let Some(id) = arguments
        .get(PREBOUND_SINGLE_ID_KEY)
        .and_then(Value::as_str)
    {
        let label_value = arguments
            .get(PREBOUND_SINGLE_LABEL_KEY)
            .cloned()
            .ok_or_else(|| bad_tool_arguments("missing prepared edit label"))?;
        let label = serde_json::from_value::<TodoSelectionLabel>(label_value)
            .map_err(|err| bad_tool_arguments(format!("invalid prepared edit label: {err}")))?;
        let patch_value = arguments
            .get(PREBOUND_EDIT_DRAFT_KEY)
            .cloned()
            .ok_or_else(|| bad_tool_arguments("missing prepared edit patch"))?;
        let patch = serde_json::from_value::<TodoEditPatch>(patch_value)
            .map_err(|err| bad_tool_arguments(format!("invalid prepared edit patch: {err}")))?;
        let raw_text = required_non_empty_text(arguments, "raw_text")?;
        let item = todo_store
            .get_by_id(&scope.owner, id)
            .map_err(super::common::todo_tool_error)?
            .ok_or_else(|| {
                LlmError::new(
                    TODO_SELECTION_NOT_FOUND_CODE,
                    "selected todo no longer exists",
                    "tool",
                )
            })?;
        return Ok(TodoToolSingleItemResolutionWithDraft::Item {
            item: Box::new(item),
            label,
            patch,
            raw_text,
        });
    }

    if let Some(output) = arguments.get(PREBOUND_ERROR_OUTPUT_KEY).cloned() {
        return Ok(TodoToolSingleItemResolutionWithDraft::Output(
            ToolOutput::json(output),
        ));
    }

    let selection = single_todo_selection_request(arguments)?;
    let resolved = match scope.resolve_selection(&selection, todo_store)? {
        TodoToolSelectionResolution::Resolved(resolved) => resolved,
        TodoToolSelectionResolution::Output(output) => {
            return Ok(TodoToolSingleItemResolutionWithDraft::Output(output));
        }
    };
    let item = match resolved.single_item(todo_store, &scope.owner)? {
        TodoToolSingleItemResolution::Item(item) => *item,
        TodoToolSingleItemResolution::Output(output) => {
            return Ok(TodoToolSingleItemResolutionWithDraft::Output(output));
        }
    };
    Ok(TodoToolSingleItemResolutionWithDraft::Item {
        item: Box::new(item),
        label: resolved.single_label(),
        patch: todo_edit_patch(arguments)?,
        raw_text: required_non_empty_text(arguments, "raw_text")?,
    })
}

/// edit_todo execute 的解析结果；带草稿以便装箱控制 enum 体积。
pub(in crate::runtime::tools::todo) enum TodoToolSingleItemResolutionWithDraft {
    Item {
        // 编辑目标沿用装箱，避免带草稿的枚举触发 large_enum_variant。
        item: Box<TodoItem>,
        label: TodoSelectionLabel,
        patch: TodoEditPatch,
        raw_text: String,
    },
    Output(ToolOutput),
}

/// 从批量结果里按 resolved 顺序挑出成功条目；保留 label 供面向模型回填。
pub(in crate::runtime::tools::todo) fn selected_items_for_result(
    resolved: &ResolvedTodoSelection,
    items: &[TodoItem],
) -> Vec<(TodoSelectionLabel, TodoItem)> {
    let mut result = Vec::new();
    for (label, id) in &resolved.matched {
        if let Some(item) = items.iter().find(|item| &item.id == id) {
            result.push((label.clone(), item.clone()));
        }
    }
    result
}

/// complete_todos 结果里 skipped 编号回填为 missing_numbers。
pub(in crate::runtime::tools::todo) fn missing_selection_labels_for_result(
    resolved: &ResolvedTodoSelection,
    skipped_ids: &[String],
) -> Vec<TodoSelectionLabel> {
    let mut missing = resolved.missing.clone();
    for (label, id) in &resolved.matched {
        if skipped_ids.iter().any(|skipped| skipped == id) && !missing.contains(label) {
            missing.push(label.clone());
        }
    }
    missing
}

/// restore_todos 结果里未恢复的编号回填为 missing_numbers。
pub(in crate::runtime::tools::todo) fn missing_selection_labels_excluding_items(
    resolved: &ResolvedTodoSelection,
    items: &[(TodoSelectionLabel, TodoItem)],
) -> Vec<TodoSelectionLabel> {
    let restored_ids = items
        .iter()
        .map(|(_, item)| item.id.as_str())
        .collect::<HashSet<_>>();
    let mut missing = resolved.missing.clone();
    for (label, id) in &resolved.matched {
        if !restored_ids.contains(id.as_str()) && !missing.contains(label) {
            missing.push(label.clone());
        }
    }
    missing
}

/// missing 标签序列化成模型 JSON 中的 number/reference 字段。
pub(in crate::runtime::tools::todo) fn missing_numbers_json(
    labels: &[TodoSelectionLabel],
) -> Vec<Value> {
    labels
        .iter()
        .map(|label| match label {
            TodoSelectionLabel::Number(number) => serde_json::json!(number),
            TodoSelectionLabel::Reference(reference) => serde_json::json!(reference.as_str()),
        })
        .collect()
}

/// delete_todos source_condition 里的 label 文本，按编号或 reference 文本渲染。
pub(in crate::runtime::tools::todo) fn todo_selection_label_text(
    label: &TodoSelectionLabel,
) -> String {
    use super::common::TodoReference;

    match label {
        TodoSelectionLabel::Number(number) => number.to_string(),
        TodoSelectionLabel::Reference(reference) => match reference {
            TodoReference::Last => TodoReference::Last.as_str().to_owned(),
        },
    }
}
