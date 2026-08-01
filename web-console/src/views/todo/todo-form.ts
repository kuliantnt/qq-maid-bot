import { createTodo } from "../../api.js";
import { refreshTodos, showResult, valueOf, numberValue } from "./todo.js";

export interface TodoDeadlineFields {
  dueDate: string | null;
  dueAt: string | null;
  timePrecision: "none" | "date" | "date_time";
}

/**
 * 将单个截止日期时间输入投影为后端兼容字段，并确保 due_date 与 due_at 始终同一天。
 * 编辑历史“仅日期”待办时仍接受 YYYY-MM-DD，避免无意补成当天零点。
 */
export function todoDeadlineFields(value: string | null): TodoDeadlineFields {
  const deadline = value?.trim() ?? "";
  if (!deadline) return { dueDate: null, dueAt: null, timePrecision: "none" };
  const dueDate = deadline.slice(0, 10);
  if (/^\d{4}-\d{2}-\d{2}$/.test(deadline)) {
    return { dueDate, dueAt: null, timePrecision: "date" };
  }
  return { dueDate, dueAt: deadline, timePrecision: "date_time" };
}

export async function submitTodo(form: HTMLFormElement, dialog: HTMLDialogElement): Promise<void> {
  const title = valueOf("todo-create-title").trim();
  const targetRef = valueOf("todo-create-target");
  const recurrenceSelection = valueOf("todo-create-recurrence-kind");
  const recurrenceUnit = valueOf("todo-create-recurrence-unit");
  const recurrenceKind = todoRecurrenceKind(recurrenceSelection, recurrenceUnit);
  const error = document.getElementById("todo-create-error");
  if (!title || !targetRef) {
    if (error) error.textContent = "标题和目标不能为空";
    return showResult("标题和目标不能为空", true);
  }
  const button = form.querySelector<HTMLButtonElement>("button[type=submit]");
  if (button) button.disabled = true;
  const deadline = todoDeadlineFields(valueOf("todo-create-deadline"));
  const reminderAt = valueOf("todo-create-reminder-at") || null;
  try {
    await createTodo({
      title,
      target_ref: targetRef,
      detail: valueOf("todo-create-detail").trim() || null,
      due_date: deadline.dueDate,
      due_at: deadline.dueAt,
      reminder_at: reminderAt,
      time_precision: deadline.timePrecision,
      recurrence_kind: recurrenceKind,
      // “不重复”必须同时丢弃可能残留的间隔，避免后端按 interval/unit 推导出重复规则。
      recurrence_interval: recurrenceKind === "none" ? null : numberValue("todo-create-recurrence-interval"),
      recurrence_unit: recurrenceUnit,
    });
    form.reset();
    dialog.close();
    await refreshTodos("refresh");
    showResult("Todo 已创建", false);
  } catch (cause) {
    // 创建失败保留用户已填写内容，仅展示明确错误，不关闭弹窗。
    if (error) error.textContent = cause instanceof Error ? cause.message : "Todo 创建失败";
    showResult(cause instanceof Error ? cause.message : "Todo 创建失败", true);
  } finally {
    if (button) button.disabled = false;
  }
}

/** 页面用统一的“间隔重复”入口，提交前按单位转换为后端支持的枚举。 */
export function todoRecurrenceKind(selection: string, unit: string): string {
  if (selection === "none") return "none";
  switch (unit) {
    case "minute": return "every_n_minutes";
    case "hour": return "every_n_hours";
    case "week": return "every_n_weeks";
    case "month": return "every_n_months";
    case "year": return "every_n_years";
    default: return "every_n_days";
  }
}
