import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useForm } from "@tanstack/react-form";
import { useEffect, useState } from "react";
import { getTodo, updateTodo } from "../../api.js";
import { Button } from "../../components/ui/button.js";
import { Field, Input } from "../../components/ui/field.js";
import { FormDialog } from "../../components/ui/form-dialog.js";
import { showToast } from "../../stores/toast.js";
import { todoDeadlineFields } from "./todo-fields.js";
import { todoEditFormSchema, type TodoEditFormValues } from "./todo-schema.js";

type TodoEditDialogProps = {
  /** 待编辑的 Todo id；null 表示关闭。 */
  todoId: string | null;
  onClose: () => void;
};

const EMPTY: TodoEditFormValues = {
  title: "",
  detail: "",
  deadline: "",
  reminderAt: "",
  recurrenceKind: "none",
  recurrenceInterval: "",
  recurrenceUnit: "",
};

/** 编辑 Todo 对话框：打开时从服务端加载最新数据预填，替代旧版连续 window.prompt。 */
export function TodoEditDialog({ todoId, onClose }: TodoEditDialogProps) {
  const open = todoId !== null;
  const [submitError, setSubmitError] = useState<string | null>(null);
  const detailQuery = useQuery({
    queryKey: ["todo", todoId],
    queryFn: () => getTodo(todoId!),
    enabled: open,
    staleTime: 0,
  });

  const form = useForm({
    defaultValues: EMPTY,
    validators: { onSubmit: todoEditFormSchema },
    onSubmit: async ({ value }) => {
      if (todoId === null) return;
      setSubmitError(null);
      const deadline = todoDeadlineFields(value.deadline);
      const interval = value.recurrenceInterval.trim();
      try {
        await updateTodoMutation.mutateAsync({
          id: todoId,
          changes: {
            title: value.title.trim(),
            detail: value.detail.trim() || null,
            due_date: deadline.dueDate,
            due_at: deadline.dueAt,
            reminder_at: value.reminderAt.trim() || null,
            time_precision: deadline.timePrecision,
            recurrence_kind: value.recurrenceKind,
            recurrence_interval: interval ? Number(interval) : null,
            recurrence_unit: value.recurrenceUnit,
          },
        });
      } catch {
        // 错误已由 mutation onError 展示；保持弹窗打开。
      }
    },
  });

  const queryClient = useQueryClient();
  const updateTodoMutation = useMutation({
    mutationFn: ({ id, changes }: { id: string; changes: Record<string, unknown> }) => updateTodo(id, changes),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["todos"] });
      showToast("success", "Todo 已更新");
      onClose();
    },
    onError: (cause) => {
      setSubmitError(cause instanceof Error ? cause.message : "Todo 更新失败");
    },
  });

  // 服务端最新数据到达后预填表单（每次打开都覆盖上一次的输入）。
  useEffect(() => {
    const todo = detailQuery.data;
    if (!open || !todo) return;
    form.reset({
      title: todo.title,
      detail: todo.detail ?? "",
      deadline: todo.dueAt ?? todo.dueDate ?? "",
      reminderAt: todo.reminderAt ?? "",
      recurrenceKind: todo.recurrenceKind,
      recurrenceInterval: todo.recurrenceInterval ? String(todo.recurrenceInterval) : "",
      recurrenceUnit: todo.recurrenceUnit,
    });
  }, [open, detailQuery.data, form]);

  return (
    <FormDialog
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
      title="查看 / 编辑 Todo"
      description="表单以服务端最新数据预填；留空的截止或提醒表示清除。"
      busy={updateTodoMutation.isPending}
      footer={
        submitError ? (
          <p role="alert" className="m-0 text-xs font-semibold text-error">{submitError}</p>
        ) : null
      }
    >
      {detailQuery.isPending ? (
        <p className="console-mono-tag m-0" role="status">正在加载 Todo…</p>
      ) : detailQuery.isError ? (
        <p role="alert" className="m-0 text-sm font-semibold text-error">
          {detailQuery.error instanceof Error ? detailQuery.error.message : "Todo 加载失败"}
        </p>
      ) : (
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            event.stopPropagation();
            void form.handleSubmit();
          }}
        >
          <form.Field name="title">
            {(field) => (
              <Field label="标题" id="todo-edit-title">
                {(props) => (
                  <Input {...props} value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
                )}
              </Field>
            )}
          </form.Field>
          <form.Field name="detail">
            {(field) => (
              <Field label="详情（留空清除）" id="todo-edit-detail">
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
          <form.Field name="deadline">
            {(field) => (
              <Field label="截止日期时间" id="todo-edit-deadline" hint="YYYY-MM-DD HH:MM 或 YYYY-MM-DD，留空清除。">
                {(props) => (
                  <Input {...props} value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
                )}
              </Field>
            )}
          </form.Field>
          <form.Field name="reminderAt">
            {(field) => (
              <Field label="提醒时间（留空清除）" id="todo-edit-reminder" hint="RFC3339 或本地时间格式。">
                {(props) => (
                  <Input {...props} value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
                )}
              </Field>
            )}
          </form.Field>
          <div className="grid grid-cols-3 gap-3">
            <form.Field name="recurrenceKind">
              {(field) => (
                <Field label="重复类型" id="todo-edit-recurrence-kind">
                  {(props) => (
                    <Input {...props} value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
                  )}
                </Field>
              )}
            </form.Field>
            <form.Field name="recurrenceInterval">
              {(field) => (
                <Field label="间隔" id="todo-edit-recurrence-interval">
                  {(props) => (
                    <Input {...props} inputMode="numeric" value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
                  )}
                </Field>
              )}
            </form.Field>
            <form.Field name="recurrenceUnit">
              {(field) => (
                <Field label="单位" id="todo-edit-recurrence-unit" hint="day/week/month">
                  {(props) => (
                    <Input {...props} value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} />
                  )}
                </Field>
              )}
            </form.Field>
          </div>
          <Button type="submit" disabled={updateTodoMutation.isPending}>
            {updateTodoMutation.isPending ? "保存中…" : "保存修改"}
          </Button>
        </form>
      )}
    </FormDialog>
  );
}
