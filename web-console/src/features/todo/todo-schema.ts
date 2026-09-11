import { z } from "zod";

/**
 * Todo 创建/编辑共用 payload schema：z.infer 类型直接作为 createTodo/updateTodo
 * 请求负载的编译期契约；表单值经表单投影函数转换为本 schema 形状。
 */
export const todoWritePayloadSchema = z.object({
  title: z.string().min(1),
  target_ref: z.string().optional(),
  detail: z.string().nullable(),
  due_date: z.string().nullable(),
  due_at: z.string().nullable(),
  reminder_at: z.string().nullable(),
  time_precision: z.enum(["none", "date", "date_time"]),
  recurrence_kind: z.string(),
  recurrence_interval: z.number().nullable(),
  recurrence_unit: z.string(),
});

export type TodoWritePayload = z.infer<typeof todoWritePayloadSchema>;

/** Todo 创建表单：日期与时间分列输入，提交前合成为统一截止值。 */
export const todoCreateFormSchema = z
  .object({
    title: z.string().trim().min(1, "标题和目标不能为空"),
    targetRef: z.string().min(1, "标题和目标不能为空"),
    detail: z.string(),
    dueDate: z.string(),
    dueTime: z.string(),
    reminderAt: z.string(),
    recurrenceKind: z.enum(["none", "interval"]),
    recurrenceInterval: z.string(),
    recurrenceUnit: z.enum(["minute", "hour", "day", "week", "month", "year"]),
  })
  .refine((value) => !(value.dueDate === "" && value.dueTime !== ""), {
    message: "设置截止时间前请先选择截止日期",
    path: ["dueTime"],
  });

export type TodoCreateFormValues = z.infer<typeof todoCreateFormSchema>;

/** Todo 编辑表单：与旧版 prompt 编辑字段一致，截止时间为单行合并输入。 */
export const todoEditFormSchema = z.object({
  title: z.string().trim().min(1, "标题不能为空"),
  detail: z.string(),
  deadline: z.string(),
  reminderAt: z.string(),
  recurrenceKind: z.string().min(1, "重复类型不能为空"),
  recurrenceInterval: z.string(),
  recurrenceUnit: z.string(),
});

export type TodoEditFormValues = z.infer<typeof todoEditFormSchema>;
