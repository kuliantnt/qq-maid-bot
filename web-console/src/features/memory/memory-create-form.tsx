import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useForm } from "@tanstack/react-form";
import { useMemo, useState } from "react";
import { createMemory } from "../../memory-api.js";
import { Button } from "../../components/ui/button.js";
import { Field, Input } from "../../components/ui/field.js";
import { showToast } from "../../stores/toast.js";
import type { MemoryCategory, MemoryTargetView, MemoryVisibility } from "../../types.js";
import { CATEGORY_LABELS, VISIBILITY_OPTIONS, canCreateMemory, memoryTargetLabel } from "./memory-labels.js";
import { z } from "zod";

/** Memory 创建表单 schema：范围与内容必填；可见性是否属于所选范围在提交时校验。 */
export const memoryCreateFormSchema = z.object({
  targetRef: z.string().min(1, "创建需要选择范围、有效可见性并填写内容"),
  content: z.string().trim().min(1, "创建需要选择范围、有效可见性并填写内容"),
  category: z.string(),
  visibility: z.string(),
  pinned: z.boolean(),
});

type MemoryCreateFormValues = z.infer<typeof memoryCreateFormSchema>;

type Props = {
  targets: readonly MemoryTargetView[];
  targetsLoading: boolean;
  disabled: boolean;
};

/** 创建 Memory 表单（嵌入页面主面板，沿用旧版非弹窗布局）。 */
export function MemoryCreateForm({ targets, targetsLoading, disabled }: Props) {
  const queryClient = useQueryClient();
  const [submitError, setSubmitError] = useState<string | null>(null);
  const creatableTargets = useMemo(() => targets.filter(canCreateMemory), [targets]);

  const form = useForm({
    defaultValues: {
      targetRef: "",
      content: "",
      category: "note",
      visibility: "",
      pinned: false,
    } as MemoryCreateFormValues,
    validators: { onSubmit: memoryCreateFormSchema },
    onSubmit: async ({ value }) => {
      setSubmitError(null);
      const target = targets.find((option) => option.targetRef === value.targetRef);
      const visibility = value.visibility === "" ? null : (value.visibility as MemoryVisibility);
      const category = value.category === "" ? null : (value.category as MemoryCategory);
      if (!target || !visibility || !category || !VISIBILITY_OPTIONS[target.scope].some(([option]) => option === visibility)) {
        setSubmitError("创建需要选择范围、有效可见性并填写内容");
        return;
      }
      try {
        await createMutation.mutateAsync({ targetRef: value.targetRef, content: value.content, category, visibility, pinned: value.pinned });
        form.reset();
      } catch {
        // 错误由 mutation onError 展示。
      }
    },
  });

  const createMutation = useMutation({
    mutationFn: createMemory,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["memories"] });
      showToast("success", "Memory 已由服务端确认创建");
    },
    onError: (cause) => setSubmitError(cause instanceof Error ? cause.message : "Memory 创建失败"),
  });

  const selectedScope = targets.find((option) => option.targetRef === form.state.values.targetRef)?.scope;
  const visibilityChoices = selectedScope ? VISIBILITY_OPTIONS[selectedScope] : [];

  return (
    <form
      aria-label="创建 Memory"
      className="mt-6 flex flex-col gap-3 border border-line bg-glass-muted p-4"
      onSubmit={(event) => {
        event.preventDefault();
        event.stopPropagation();
        void form.handleSubmit();
      }}
    >
      <p className="console-mono-tag m-0">CREATE / CONTROLLED WRITE</p>
      <div className="grid gap-3 md:grid-cols-2">
        <Field label="已授权范围" id="memory-create-target" hint={targetsLoading ? "加载范围中…" : undefined}>
          {(props) => (
            <select
              {...props}
              disabled={disabled || creatableTargets.length === 0}
              value={form.state.values.targetRef}
              onChange={(event) => form.setFieldValue("targetRef", event.target.value)}
              className="border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
            >
              <option value="">
                {targetsLoading && targets.length === 0
                  ? "加载范围中…"
                  : creatableTargets.length > 0
                    ? "选择已授权范围…"
                    : "暂无可用范围"}
              </option>
              {creatableTargets.map((target) => (
                <option key={target.targetRef} value={target.targetRef}>
                  {memoryTargetLabel(target)}
                </option>
              ))}
            </select>
          )}
        </Field>
        <Field label="分类" id="memory-create-type">
          {(props) => (
            <select
              {...props}
              disabled={disabled}
              value={form.state.values.category}
              onChange={(event) => form.setFieldValue("category", event.target.value)}
              className="border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
            >
              {Object.entries(CATEGORY_LABELS).map(([value, label]) => (
                <option key={value} value={value}>{label}</option>
              ))}
            </select>
          )}
        </Field>
        <Field label="可见性" id="memory-create-visibility" hint="可见性随所选范围联动。">
          {(props) => (
            <select
              {...props}
              disabled={disabled || visibilityChoices.length === 0}
              value={form.state.values.visibility}
              onChange={(event) => form.setFieldValue("visibility", event.target.value)}
              className="border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
            >
              <option value="">{visibilityChoices.length === 0 ? "先选择范围" : "选择可见性"}</option>
              {visibilityChoices.map(([value, label]) => (
                <option key={value} value={value}>{label}</option>
              ))}
            </select>
          )}
        </Field>
        <label className="flex items-center gap-2 self-end text-sm text-ink">
          <input
            type="checkbox"
            disabled={disabled}
            checked={form.state.values.pinned}
            onChange={(event) => form.setFieldValue("pinned", event.target.checked)}
            className="size-4 accent-[var(--console-accent)]"
          />
          固定此记忆
        </label>
      </div>
      <Field label="内容" id="memory-create-content">
        {(props) => (
          <textarea
            {...props}
            rows={3}
            disabled={disabled}
            value={form.state.values.content}
            onChange={(event) => form.setFieldValue("content", event.target.value)}
            className="border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
          />
        )}
      </Field>
      {submitError ? (
        <p role="alert" className="m-0 text-xs font-semibold text-error">{submitError}</p>
      ) : null}
      <Button type="submit" disabled={disabled || createMutation.isPending} className="self-start">
        {createMutation.isPending ? "提交中…" : "创建 Memory"}
      </Button>
    </form>
  );
}
