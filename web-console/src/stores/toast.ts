import { atom, getDefaultStore } from "jotai";

export type ToastKind = "info" | "success" | "error";

export type ConsoleToast = {
  readonly id: number;
  readonly kind: ToastKind;
  readonly message: string;
};

export const toastAtom = atom<ConsoleToast | null>(null);

let nextToastId = 1;

/** 展示单条全局提示；沿用旧行为：同一时刻只有一条，点击立即关闭。 */
export function showToast(kind: ToastKind, message: string): void {
  // showToast 需要在 React 外（错误处理器等）可用，走默认 store。
  getDefaultStore().set(toastAtom, { id: nextToastId++, kind, message });
}

