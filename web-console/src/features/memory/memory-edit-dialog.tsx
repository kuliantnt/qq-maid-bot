import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useForm } from "@tanstack/react-form";
import { useEffect, useState } from "react";
import { getMemory, updateMemory } from "../../memory-api.js";
import { Button } from "../../components/ui/button.js";
import { Field } from "../../components/ui/field.js";
import { FormDialog } from "../../components/ui/form-dialog.js";
import { showToast } from "../../stores/toast.js";
import type { MemoryItem } from "../../types.js";

type Props = {
  /** 待纠正的 Memory；null 表示关闭。 */
  item: MemoryItem | null;
  onClose: () => void;
};

/** 纠正记忆内容对话框：以服务端最新数据预填并携带 expectedVersion 乐观锁。 */
export function MemoryEditDialog({ item, onClose }: Props) {
  const open = item !== null;
  const [submitError, setSubmitError] = useState<string | null>(null);
  const detailQuery = useQuery({
    queryKey: ["memory", item?.target.targetRef, item?.memoryRef],
    queryFn: () => getMemory(item!.target.targetRef, item!.memoryRef),
    enabled: open,
    staleTime: 0,
  });

  const form = useForm({
    defaultValues: { content: "" },
    onSubmit: async ({ value }) => {
      if (!detailQuery.data) return;
      setSubmitError(null);
      if (value.content.trim() === detailQuery.data.content) {
        onClose();
        return;
      }
      try {
        await updateMutation.mutateAsync({
          targetRef: detailQuery.data.target.targetRef,
          memoryRef: detailQuery.data.memoryRef,
          expectedVersion: detailQuery.data.version,
          patch: { content: value.content.trim() },
        });
      } catch {
        // 错误由 mutation onError 展示。
      }
    },
  });

  const queryClient = useQueryClient();
  const updateMutation = useMutation({
    mutationFn: (input: Parameters<typeof updateMemory>[0]) => updateMemory(input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["memories"] });
      showToast("success", "Memory 已由服务端确认更新");
      onClose();
    },
    onError: (cause) => setSubmitError(cause instanceof Error ? cause.message : "Memory 更新失败"),
  });

  useEffect(() => {
    const latest = detailQuery.data;
    if (!open || !latest) return;
    if (!latest.capabilities.canUpdate) {
      setSubmitError("该 Memory 当前不可编辑，请刷新后重试");
      return;
    }
    form.reset({ content: latest.content });
  }, [open, detailQuery.data, form]);

  return (
    <FormDialog
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
      title="纠正记忆内容"
      description="服务端按版本号确认更新；保存以真实持久化结果为准。"
      busy={updateMutation.isPending}
      footer={submitError ? <p role="alert" className="m-0 text-xs font-semibold text-error">{submitError}</p> : null}
    >
      {detailQuery.isPending ? (
        <p className="console-mono-tag m-0" role="status">正在加载 Memory…</p>
      ) : (
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            event.stopPropagation();
            void form.handleSubmit();
          }}
        >
          <Field label="内容" id="memory-edit-content">
            {(props) => (
              <textarea
                {...props}
                rows={5}
                value={form.state.values.content}
                onChange={(event) => form.setFieldValue("content", event.target.value)}
                className="border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
              />
            )}
          </Field>
          <Button type="submit" disabled={updateMutation.isPending || detailQuery.isPending}>
            {updateMutation.isPending ? "保存中…" : "保存修改"}
          </Button>
        </form>
      )}
    </FormDialog>
  );
}
