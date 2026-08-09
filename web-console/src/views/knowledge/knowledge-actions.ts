import { ConsoleApiError } from "../../api.js";
import type { KnowledgeFileItem } from "../../types.js";

/** 知识库行操作集中处理下载、重试和删除，避免展示层绕过文件状态与权限边界。 */
type KnowledgeActionDeps = {
  readonly setStatus: (text: string) => void;
  readonly download: (item: KnowledgeFileItem) => Promise<{ blob: Blob; filename: string }>;
  readonly triggerDownload: (blob: Blob, filename: string) => void;
  readonly deleteFile: (fileId: string) => Promise<void>;
  readonly retryFile: (fileId: string) => Promise<KnowledgeFileItem>;
  readonly refresh: (reason: "delete" | "retry") => void;
  readonly getItems: () => readonly KnowledgeFileItem[];
};

type KnowledgeActionHandlers = {
  readonly onDownload: (item: KnowledgeFileItem) => void;
  readonly onDelete: (item: KnowledgeFileItem) => void;
  readonly onRetry: (item: KnowledgeFileItem) => void;
};

let deleteDialog: HTMLDialogElement | null = null;
let deleteDialogTitle: HTMLElement | null = null;
let deleteDialogMessage: HTMLElement | null = null;
let deleteDialogCancel: HTMLButtonElement | null = null;
let deleteDialogConfirm: HTMLButtonElement | null = null;
let deleteOpener: HTMLButtonElement | null = null;
let deleteFileId: string | null = null;

export function triggerBrowserDownload(blob: Blob, filename: string): void {
  if (typeof URL === "undefined" || typeof document === "undefined" || document.body === null) return;
  const href = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = href;
  anchor.download = filename;
  anchor.style.display = "none";
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => URL.revokeObjectURL(href), 60_000);
}

export function createKnowledgeActionHandlers(deps: KnowledgeActionDeps): KnowledgeActionHandlers {
  // 操作锁按按钮实例隔离，避免重复点击同一行，同时不阻塞其他文件的独立操作。
  const activeButtons = new WeakSet<HTMLButtonElement>();
  return {
    onDownload: (item) => void download(item, deps, activeButtons),
    onDelete: (item) => openDeleteDialog(item, deps, activeButtons),
    onRetry: (item) => void retry(item, deps, activeButtons),
  };
}

async function download(item: KnowledgeFileItem, deps: KnowledgeActionDeps, active: WeakSet<HTMLButtonElement>): Promise<void> {
  if (!item.downloadable || item.source !== "managed" || item.file_id === null) return;
  const button = actionButton(item.file_id, "下载");
  if (button === null || active.has(button)) return;
  active.add(button);
  button.disabled = true;
  try {
    const result = await deps.download(item);
    deps.triggerDownload(result.blob, result.filename);
  } catch (cause) {
    deps.setStatus(safeMessage(cause));
  } finally {
    active.delete(button);
    button.disabled = false;
  }
}

async function retry(item: KnowledgeFileItem, deps: KnowledgeActionDeps, active: WeakSet<HTMLButtonElement>): Promise<void> {
  if (item.status !== "failed" || item.source !== "managed" || item.file_id === null) return;
  const button = actionButton(item.file_id, "重新处理");
  if (button === null || active.has(button)) return;
  active.add(button);
  button.disabled = true;
  try {
    await deps.retryFile(item.file_id);
    deps.refresh("retry");
  } catch (cause) {
    deps.setStatus(safeMessage(cause));
  } finally {
    active.delete(button);
    button.disabled = false;
  }
}

function openDeleteDialog(item: KnowledgeFileItem, deps: KnowledgeActionDeps, active: WeakSet<HTMLButtonElement>): void {
  if (item.source !== "managed" || item.file_id === null || item.status === "processing") return;
  const fileId = item.file_id;
  const opener = actionButton(item.file_id, "删除");
  if (opener === null || active.has(opener)) return;
  const dialog = ensureDeleteDialog();
  deleteOpener = opener;
  deleteFileId = fileId;
  if (deleteDialogTitle) deleteDialogTitle.textContent = `删除文件：${item.filename}`;
  if (deleteDialogMessage) deleteDialogMessage.textContent = "删除后，该文件及对应的知识库解析和索引数据将被移除，且无法继续被检索。此操作不可恢复。";
  const close = () => closeDeleteDialog(opener, fileId);
  if (deleteDialogCancel) deleteDialogCancel.onclick = close;
  const confirmButton = deleteDialogConfirm;
  if (confirmButton) confirmButton.onclick = () => void confirmDelete(fileId, confirmButton, close, deps, active);
  deleteDialogCancel?.focus();
  if (typeof dialog.showModal === "function") dialog.showModal();
  else dialog.setAttribute("open", "");
}

async function confirmDelete(fileId: string, button: HTMLButtonElement, close: () => void, deps: KnowledgeActionDeps, active: WeakSet<HTMLButtonElement>): Promise<void> {
  // 确认按钮也单独加锁，避免删除请求尚未完成时重复提交同一文件。
  if (active.has(button)) return;
  active.add(button);
  button.disabled = true;
  try {
    await deps.deleteFile(fileId);
    close();
    deps.refresh("delete");
    deps.setStatus("文件已删除");
  } catch (cause) {
    close();
    deps.setStatus(cause instanceof ConsoleApiError && cause.status === 409 ? "文件正在处理中，暂不能删除" : safeMessage(cause));
  } finally {
    active.delete(button);
    button.disabled = false;
  }
}

function ensureDeleteDialog(): HTMLDialogElement {
  if (deleteDialog !== null && document.querySelector('[role="alertdialog"]') === deleteDialog) return deleteDialog;
  const dialog = document.createElement("dialog") as HTMLDialogElement;
  dialog.setAttribute("role", "alertdialog");
  dialog.setAttribute("aria-modal", "true");
  const title = document.createElement("h2");
  title.id = "knowledge-delete-title";
  const message = document.createElement("p");
  message.id = "knowledge-delete-message";
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.textContent = "取消";
  const confirm = document.createElement("button");
  confirm.type = "button";
  confirm.className = "danger";
  confirm.textContent = "删除";
  dialog.setAttribute("aria-labelledby", title.id);
  dialog.setAttribute("aria-describedby", message.id);
  dialog.append(title, message, cancel, confirm);
  document.body?.append(dialog);
  deleteDialog = dialog;
  deleteDialogTitle = title;
  deleteDialogMessage = message;
  deleteDialogCancel = cancel;
  deleteDialogConfirm = confirm;
  dialog.addEventListener("cancel", () => {
    if (deleteOpener !== null && deleteFileId !== null) closeDeleteDialog(deleteOpener, deleteFileId);
  });
  return dialog;
}

function closeDeleteDialog(opener: HTMLButtonElement, fileId: string): void {
  if (deleteDialog === null) return;
  if (typeof deleteDialog.close === "function") deleteDialog.close();
  else deleteDialog.removeAttribute("open");
  restoreFocus(opener, fileId);
}

function restoreFocus(opener: HTMLButtonElement, fileId: string): void {
  if (opener.isConnected !== false) {
    opener.focus();
    return;
  }
  const rowButton = actionButton(fileId, "删除") ?? actionButton(fileId, "重新处理") ?? actionButton(fileId, "下载");
  if (rowButton !== null && rowButton.isConnected !== false) {
    rowButton.focus();
    return;
  }
  const list = document.getElementById("knowledge-list");
  if (list instanceof HTMLElement) {
    list.tabIndex = -1;
    list.focus();
    return;
  }
  document.getElementById("knowledge-upload-open")?.focus();
}

function actionButton(fileId: string, label: string): HTMLButtonElement | null {
  const buttons = document.querySelectorAll("button[data-file-id]");
  for (const button of buttons) {
    if (button instanceof HTMLButtonElement && button.dataset.fileId === fileId && button.textContent === label) return button;
  }
  return null;
}

function safeMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : "操作失败，请稍后重试";
}
