/** Todo 截止时间与重复规则的后端投影逻辑，从旧实现原样迁移（纯函数，保持可测）。 */

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

/** 创建表单把同一字段组内的日期与可选时间合成统一截止值。 */
export function todoDeadlineFromParts(dateValue: string, timeValue: string): TodoDeadlineFields {
  const dueDate = dateValue.trim();
  const dueTime = timeValue.trim();
  if (!dueDate) return todoDeadlineFields(null);
  return todoDeadlineFields(dueTime ? `${dueDate}T${dueTime}` : dueDate);
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

/** 目标下拉的展示文案。 */
export function todoTargetLabel(target: {
  platform: string;
  scopeType: string;
  userId: string | null;
  groupId: string | null;
  targetRef: string;
}): string {
  return `${target.platform} · ${target.scopeType} · ${target.userId ?? target.groupId ?? target.targetRef}`;
}
