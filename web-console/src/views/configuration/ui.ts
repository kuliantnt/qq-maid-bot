import { element } from "./fields.js";
import { current } from "./state.js";

let toastTimer: number | undefined;

export function showResult(message: string, error: boolean): void {
  const target = element("configuration-result");
  target.textContent = message;
  target.className = error ? "error" : "success";
  showToast(message, error);
}

/** 右上角浮层提醒；进行中的消息不设置自动隐藏，避免转圈提示被提前关掉。 */
export function showToast(message: string, error: boolean): void {
  const toast = element("console-toast");
  toast.textContent = message;
  toast.className = `console-toast ${error ? "console-toast-error" : "console-toast-success"}`;
  toast.hidden = false;
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  if (!message.startsWith("正在")) {
    toastTimer = window.setTimeout(() => {
      toast.hidden = true;
      toastTimer = undefined;
    }, 8_000);
  }
}

export function errorMessage(cause: unknown): string { return cause instanceof Error ? cause.message : "配置操作失败"; }

export function setButtonsDisabled(disabled: boolean): void {
  for (const id of ["save-public-config", "save-secret-config", "save-agent-config", "validate-config"]) {
    element(id, HTMLButtonElement).disabled = disabled;
  }
  for (const button of document.querySelectorAll<HTMLButtonElement>(".tool-whitelist-save")) {
    button.disabled = disabled || current?.agent?.editable !== true;
  }
  for (const button of document.querySelectorAll<HTMLButtonElement>(".provider-action")) {
    button.disabled = disabled || current?.agent?.editable !== true;
  }
}
