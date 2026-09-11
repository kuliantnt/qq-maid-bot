import { describe, expect, it } from "vitest";
import {
  todoDeadlineFields,
  todoDeadlineFromParts,
  todoRecurrenceKind,
  todoTargetLabel,
} from "../src/features/todo/todo-fields.js";

describe("Todo 截止时间投影", () => {
  it("空值投影为 timePrecision none", () => {
    expect(todoDeadlineFields(null)).toEqual({ dueDate: null, dueAt: null, timePrecision: "none" });
    expect(todoDeadlineFields("   ")).toEqual({ dueDate: null, dueAt: null, timePrecision: "none" });
  });

  it("仅日期输入保持 date 精度，不补当天零点", () => {
    expect(todoDeadlineFields("2026-09-11")).toEqual({ dueDate: "2026-09-11", dueAt: null, timePrecision: "date" });
  });

  it("日期时间输入投影为 date_time 且 due_date 取同一天", () => {
    expect(todoDeadlineFields("2026-09-11 08:30")).toEqual({ dueDate: "2026-09-11", dueAt: "2026-09-11 08:30", timePrecision: "date_time" });
  });

  it("创建表单的日期+时间分组合成统一截止值", () => {
    expect(todoDeadlineFromParts("2026-09-11", "09:00")).toEqual({ dueDate: "2026-09-11", dueAt: "2026-09-11T09:00", timePrecision: "date_time" });
    expect(todoDeadlineFromParts("2026-09-11", "")).toEqual({ dueDate: "2026-09-11", dueAt: null, timePrecision: "date" });
    expect(todoDeadlineFromParts("", "09:00")).toEqual({ dueDate: null, dueAt: null, timePrecision: "none" });
  });
});

describe("Todo 重复规则投影", () => {
  it("选择不重复时始终返回 none", () => {
    expect(todoRecurrenceKind("none", "week")).toBe("none");
  });

  it("间隔重复按单位转换为后端枚举", () => {
    expect(todoRecurrenceKind("interval", "minute")).toBe("every_n_minutes");
    expect(todoRecurrenceKind("interval", "hour")).toBe("every_n_hours");
    expect(todoRecurrenceKind("interval", "day")).toBe("every_n_days");
    expect(todoRecurrenceKind("interval", "week")).toBe("every_n_weeks");
    expect(todoRecurrenceKind("interval", "month")).toBe("every_n_months");
    expect(todoRecurrenceKind("interval", "year")).toBe("every_n_years");
    expect(todoRecurrenceKind("interval", "unknown")).toBe("every_n_days");
  });
});

describe("Todo 目标标签", () => {
  it("优先展示用户 ID，其次群组，最后退化到 opaque ref", () => {
    expect(todoTargetLabel({ platform: "qq_official", scopeType: "private", userId: "u1", groupId: null, targetRef: "r" })).toBe("qq_official · private · u1");
    expect(todoTargetLabel({ platform: "onebot11", scopeType: "group", userId: null, groupId: "g1", targetRef: "r" })).toBe("onebot11 · group · g1");
    expect(todoTargetLabel({ platform: "wechat", scopeType: "private", userId: null, groupId: null, targetRef: "opaque" })).toBe("wechat · private · opaque");
  });
});
