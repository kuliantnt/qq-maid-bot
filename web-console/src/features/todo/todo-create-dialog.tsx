import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useForm } from "@tanstack/react-form";
import { useState } from "react";
import { createTodo } from "../../api.js";
import { Button } from "../../components/ui/button.js";
import { Field, Input } from "../../components/ui/field.js";
import { FormDialog } from "../../components/ui/form-dialog.js";
import { showToast } from "../../stores/toast.js";
import type { TodoTargetOption } from "../../types.js";
import { todoDeadlineFromParts, todoRecurrenceKind, todoTargetLabel } from "./todo-fields.js";
import { todoCreateFormSchema, type TodoCreateFormValues } from "./todo-schema.js";

type TodoCreateDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  targets: readonly TodoTargetOption[];
  targetsLoading: boolean;
  hasMoreTargets: boolean;
  onLoadMoreTargets: () => void;
};

const EMPTY_VALUES: TodoCreateFormValues = {
  title: "",
  targetRef: "",
  detail: "",
  dueDate: "",
  dueTime: "",
  reminderAt: "",
  recurrenceKind: "none",
  recurrenceInterval: "",
  recurrenceUnit: "day",
};

/** 创建 Todo 对话框：失败时保留已填写内容并展示错误，不关闭弹窗（沿用旧交互契约）。 */
export function TodoCreateDialog({
  open,
  onOpenChange,
  targets,
  targetsLoading,
  hasMoreTargets,
  onLoadMoreTargets,
}: TodoCreateDialogProps) {
  const queryClient = useQueryClient();
  const [submitError, setSubmitError] = useState<string | null>(null);

  const createMutation = useMutation({
    mutationFn: createTodo,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["todos"] });
      showToast("success", "Todo 已创建");
      form.reset();
      onOpenChange(false);
    },
    onError: (cause) => {
      // 创建失败保留用户已填写内容，仅展示明确错误，不关闭弹窗。
      setSubmitError(cause instanceof Error ? cause.message : "Todo 创建失败");
    },
  });

  const form = useForm({
    defaultValues: EMPTY_VALUES,
    validators: { onSubmit: todoCreateFormSchema },
    onSubmit: async ({ value }) => {
      setSubmitError(null);
      // “不重复”必须同时丢弃可能残留的间隔，避免后端按 interval/unit 推导出重复规则。
      const recurring = value.recurrenceKind === "interval";
      const deadline = todoDeadlineFromParts(value.dueDate, value.dueTime);
      await createMutation.mutateAsync({
        title: value.title.trim(),
        target_ref: value.targetRef,
        detail: value.detail.trim() || null,
        due_date: deadline.dueDate,
        due_at: deadline.dueAt,
        reminder_at: value.reminderAt.trim() || null,
        time_precision: deadline.timePrecision,
        recurrence_kind: todoRecurrenceKind(value.recurrenceKind, value.recurrenceUnit),
        recurrence_interval: recurring && value.recurrenceInterval.trim() ? Number(value.recurrenceInterval) : null,
        recurrence_unit: value.recurrenceUnit,
      });
    },
  });

  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      title="创建 Todo"
      busy={createMutation.isPending}
      footer={
        submitError ? (
          <p role="alert" className="m-0 text-xs font-semibold text-error">{submitError}</p>
        ) : null
      }
    >
      <form
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          event.stopPropagation();
          void form.handleSubmit();
        }}
      >
        <form.Field name="title" validators={{ onChange: ({ value }) => (value.trim() ? undefined : ({ message: "标题和目标不能为空" } as const)) }}>
          {(field) => (
            <Field label="标题" id="todo-create-title" error={fieldErrorText(field.state.meta.errors)}>
              {(props) => (
                <Input {...props} value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
              )}
            </Field>
          )}
        </form.Field>

        <Field label="目标" id="todo-create-target" hint="目标与提醒状态来自管理 API；没有可用目标时无法创建。">
          {(props) => (
            <select
              {...props}
              value={form.state.values.targetRef}
              onChange={(event) => form.setFieldValue("targetRef", event.target.value)}
              className="border border-line bg-input px-3 py-2.5 text-ink outline-none"
            >
              <option value="">{targetsLoading ? "正在加载目标…" : "选择目标…"}</option>
              {targets.map((target) => (
                <option key={target.targetRef} value={target.targetRef}>
                  {todoTargetLabel(target)}
                </option>
              ))}
            </select>
          )}
        </Field>
        {hasMoreTargets ? (
          <Button variant="secondary" onClick={onLoadMoreTargets} className="px-3 py-1.5 text-xs">
            加载更多目标…
          </Button>
        ) : null}

        <form.Field name="detail">
          {(field) => (
            <Field label="详情（可选）" id="todo-create-detail">
              {(props) => (
                <textarea
                  {...props}
                  rows={3}
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => field.handleChange(event.target.value)}
                  className="border border-line bg-input px-3 py-2.5 text-ink outline-none"
                />
              )}
            </Field>
          )}
        </form.Field>

        <div className="grid grid-cols-2 gap-4">
          <form.Field
            name="dueDate"
            validators={{
              onChange: ({ value, fieldApi }) =>
                value === "" && fieldApi.form.state.values.dueTime !== ""
                  ? ({ message: "设置截止时间前请先选择截止日期" } as const)
                  : undefined,
            }}
          >
            {(field) => (
              <Field label="截止日期（可选）" id="todo-create-due-date" error={fieldErrorText(field.state.meta.errors)}>
                {(props) => (
                  <Input {...props} type="date" value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
                )}
              </Field>
            )}
          </form.Field>
          <form.Field name="dueTime">
            {(field) => (
              <Field label="截止时间（可选）" id="todo-create-due-time">
                {(props) => (
                  <Input {...props} type="time" value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
                )}
              </Field>
            )}
          </form.Field>
        </div>

        <form.Field name="reminderAt">
          {(field) => (
            <Field label="提醒时间（可选）" id="todo-create-reminder-at" hint="RFC3339 或本地时间格式。">
              {(props) => (
                <Input {...props} value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
              )}
            </Field>
          )}
        </form.Field>

        <div className="grid grid-cols-3 gap-4">
          <form.Field name="recurrenceKind">
            {(field) => (
              <Field label="重复" id="todo-create-recurrence-kind">
                {(props) => (
                  <select
                    {...props}
                    value={field.state.value}
                    onChange={(event) => field.handleChange(event.target.value as TodoCreateFormValues["recurrenceKind"])}
                    className="border border-line bg-input px-3 py-2.5 text-ink outline-none"
                  >
                    <option value="none">不重复</option>
                    <option value="interval">间隔重复</option>
                  </select>
                )}
              </Field>
            )}
          </form.Field>
          <form.Subscribe selector={(state) => state.values.recurrenceKind}>
            {(kind) =>
              kind === "interval" ? (
                <form.Field name="recurrenceInterval">
                  {(field) => (
                    <Field label="间隔" id="todo-create-recurrence-interval">
                      {(props) => (
                        <Input {...props} inputMode="numeric" value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
                      )}
                    </Field>
                  )}
                </form.Field>
              ) : null
            }
          </form.Subscribe>
          <form.Subscribe selector={(state) => state.values.recurrenceKind}>
            {(kind) =>
              kind === "interval" ? (
                <form.Field name="recurrenceUnit">
                  {(field) => (
                    <Field label="单位" id="todo-create-recurrence-unit">
                      {(props) => (
                        <select
                          {...props}
                          value={field.state.value}
                          onChange={(event) => field.handleChange(event.target.value as TodoCreateFormValues["recurrenceUnit"])}
                          className="border border-line bg-input px-3 py-2.5 text-ink outline-none"
                        >
                          <option value="minute">分钟</option>
                          <option value="hour">小时</option>
                          <option value="day">天</option>
                          <option value="week">周</option>
                          <option value="month">月</option>
                          <option value="year">年</option>
                        </select>
                      )}
                    </Field>
                  )}
                </form.Field>
              ) : null
            }
          </form.Subscribe>
        </div>

        <form.Subscribe selector={(state) => [state.canSubmit, state.isSubmitting] as const}>
          {([canSubmit, isSubmitting]) => (
            <Button type="submit" disabled={!canSubmit || isSubmitting}>
              {isSubmitting ? "提交中…" : "创建"}
            </Button>
          )}
        </form.Subscribe>
      </form>
    </FormDialog>
  );
}

/** Standard Schema 校验错误 → 文本。 */
export function fieldErrorText(errors: ReadonlyArray<unknown>): string | undefined {
  const first = errors[0];
  if (first === undefined) return undefined;
  if (typeof first === "string") return first;
  if (typeof first === "object" && first !== null && "message" in first) return String(first.message);
  return String(first);
}
