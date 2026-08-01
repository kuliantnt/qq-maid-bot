import { createTodo } from "../../api.js";
import { refreshTodos, showResult, valueOf, numberValue } from "./todo.js";
export function resolveTimePrecision(dueAt, selected) {
    return dueAt !== null ? "date_time" : selected;
}
export async function submitTodo(form, dialog) {
    const title = valueOf("todo-create-title").trim();
    const targetRef = valueOf("todo-create-target");
    const recurrenceSelection = valueOf("todo-create-recurrence-kind");
    const recurrenceUnit = valueOf("todo-create-recurrence-unit");
    const recurrenceKind = todoRecurrenceKind(recurrenceSelection, recurrenceUnit);
    const error = document.getElementById("todo-create-error");
    if (!title || !targetRef) {
        if (error)
            error.textContent = "标题和目标不能为空";
        return showResult("标题和目标不能为空", true);
    }
    const button = form.querySelector("button[type=submit]");
    if (button)
        button.disabled = true;
    const dueDate = valueOf("todo-create-due-date") || null;
    const dueAt = valueOf("todo-create-due-at") || null;
    const reminderAt = valueOf("todo-create-reminder-at") || null;
    const selectedPrecision = valueOf("todo-create-time-precision");
    const timePrecision = resolveTimePrecision(dueAt, selectedPrecision);
    try {
        await createTodo({
            title,
            target_ref: targetRef,
            detail: valueOf("todo-create-detail").trim() || null,
            due_date: dueDate,
            due_at: dueAt,
            reminder_at: reminderAt,
            time_precision: timePrecision,
            recurrence_kind: recurrenceKind,
            // “不重复”必须同时丢弃可能残留的间隔，避免后端按 interval/unit 推导出重复规则。
            recurrence_interval: recurrenceKind === "none" ? null : numberValue("todo-create-recurrence-interval"),
            recurrence_unit: recurrenceUnit,
        });
        form.reset();
        dialog.close();
        await refreshTodos("refresh");
        showResult("Todo 已创建", false);
    }
    catch (cause) {
        // 创建失败保留用户已填写内容，仅展示明确错误，不关闭弹窗。
        if (error)
            error.textContent = cause instanceof Error ? cause.message : "Todo 创建失败";
        showResult(cause instanceof Error ? cause.message : "Todo 创建失败", true);
    }
    finally {
        if (button)
            button.disabled = false;
    }
}
/** 页面用统一的“间隔重复”入口，提交前按单位转换为后端支持的枚举。 */
export function todoRecurrenceKind(selection, unit) {
    if (selection === "none")
        return "none";
    switch (unit) {
        case "minute": return "every_n_minutes";
        case "hour": return "every_n_hours";
        case "week": return "every_n_weeks";
        case "month": return "every_n_months";
        case "year": return "every_n_years";
        default: return "every_n_days";
    }
}
