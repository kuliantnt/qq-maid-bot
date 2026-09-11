import * as ToastPrimitive from "@radix-ui/react-toast";
import { useAtomValue, useSetAtom } from "jotai";
import { toastAtom } from "../../stores/toast.js";
import { cn } from "../../lib/utils.js";

const KIND_CLASSES = {
  info: "border-line text-ink",
  success: "border-success text-success",
  error: "border-error text-error",
} as const;

/** 全局单条 Toast：点击立即关闭，role=status 播报；替代旧 console-toast。 */
export function ConsoleToastHost() {
  const toast = useAtomValue(toastAtom);
  const setToast = useSetAtom(toastAtom);
  return (
    <ToastPrimitive.Provider swipeDirection="down" duration={5000}>
      <ToastPrimitive.Root
        key={toast?.id}
        open={toast !== null}
        onOpenChange={(open) => {
          if (!open) setToast(null);
        }}
        onClick={() => setToast(null)}
        role="status"
        className={cn(
          "fixed bottom-24 left-1/2 z-50 -translate-x-1/2 cursor-pointer border bg-surface px-4 py-3 text-sm font-semibold shadow-console",
          KIND_CLASSES[toast?.kind ?? "info"],
        )}
      >
        <ToastPrimitive.Description>{toast?.message}</ToastPrimitive.Description>
      </ToastPrimitive.Root>
      <ToastPrimitive.Viewport />
    </ToastPrimitive.Provider>
  );
}
