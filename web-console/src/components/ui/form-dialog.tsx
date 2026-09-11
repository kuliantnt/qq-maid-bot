import * as DialogPrimitive from "@radix-ui/react-dialog";
import type { ReactNode } from "react";
import { Button } from "./button.js";

type FormDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  children: ReactNode;
  footer: ReactNode;
  /** 提交进行中时锁定关闭，避免丢失表单状态。 */
  busy?: boolean;
};

/** 承载表单内容的对话框：Radix 提供焦点陷阱与 Esc 关闭，表单区由调用方组合。 */
export function FormDialog({ open, onOpenChange, title, description, children, footer, busy = false }: FormDialogProps) {
  return (
    <DialogPrimitive.Root open={open} onOpenChange={(next) => { if (!busy) onOpenChange(next); }}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-40 bg-[color-mix(in_srgb,var(--console-background)_78%,transparent)] animate-fade-in" />
        <DialogPrimitive.Content className="fixed top-1/2 left-1/2 z-50 flex max-h-[90dvh] w-[min(34rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 flex-col overflow-auto border border-line bg-surface p-6 shadow-console animate-page-in">
          <DialogPrimitive.Title className="m-0 text-lg font-bold">{title}</DialogPrimitive.Title>
          {description ? (
            <DialogPrimitive.Description className="mt-1 text-sm leading-relaxed text-muted">
              {description}
            </DialogPrimitive.Description>
          ) : null}
          <div className="mt-4 flex flex-1 flex-col gap-4">{children}</div>
          <div className="mt-5 flex items-center justify-between gap-3">
            <div className="min-w-0 flex-1">{footer}</div>
            <div className="flex gap-3">
              <DialogPrimitive.Close asChild>
                <Button variant="secondary" disabled={busy}>取消</Button>
              </DialogPrimitive.Close>
            </div>
          </div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
