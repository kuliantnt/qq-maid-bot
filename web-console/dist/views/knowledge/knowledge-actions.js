import { ConsoleApiError } from "../../api.js";
export function triggerBrowserDownload(blob, filename) {
    const href = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = href;
    anchor.download = filename;
    anchor.click();
    URL.revokeObjectURL(href);
}
export function createKnowledgeActionHandlers(deps) {
    const activeButtons = new WeakSet();
    return {
        onDownload: (item) => void download(item, deps, activeButtons),
        onDelete: (item) => openDeleteDialog(item, deps, activeButtons),
        onRetry: (item) => void retry(item, deps, activeButtons),
    };
}
async function download(item, deps, active) {
    if (!item.downloadable || item.source !== "managed" || item.file_id === null)
        return;
    const button = actionButton(item.file_id, "下载");
    if (button === null || active.has(button))
        return;
    active.add(button);
    button.disabled = true;
    try {
        const result = await deps.download(item);
        deps.triggerDownload(result.blob, result.filename);
    }
    catch (cause) {
        deps.setStatus(safeMessage(cause));
    }
    finally {
        active.delete(button);
        button.disabled = false;
    }
}
async function retry(item, deps, active) {
    if (item.status !== "failed" || item.source !== "managed" || item.file_id === null)
        return;
    const button = actionButton(item.file_id, "重新处理");
    if (button === null || active.has(button))
        return;
    active.add(button);
    button.disabled = true;
    try {
        await deps.retryFile(item.file_id);
        deps.refresh("retry");
    }
    catch (cause) {
        deps.setStatus(safeMessage(cause));
    }
    finally {
        active.delete(button);
        button.disabled = false;
    }
}
function openDeleteDialog(item, deps, active) {
    if (item.source !== "managed" || item.file_id === null || item.status === "processing")
        return;
    const fileId = item.file_id;
    const opener = actionButton(item.file_id, "删除");
    if (opener === null || active.has(opener))
        return;
    const dialog = document.createElement("dialog");
    dialog.setAttribute("role", "alertdialog");
    dialog.setAttribute("aria-modal", "true");
    const title = document.createElement("h2");
    title.id = "knowledge-delete-title";
    title.textContent = `删除文件：${item.filename}`;
    const message = document.createElement("p");
    message.id = "knowledge-delete-message";
    message.textContent = "删除后，该文件及对应的知识库解析和索引数据将被移除，且无法继续被检索。此操作不可恢复。";
    dialog.setAttribute("aria-labelledby", title.id);
    dialog.setAttribute("aria-describedby", message.id);
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.textContent = "取消";
    const confirm = document.createElement("button");
    confirm.type = "button";
    confirm.className = "danger";
    confirm.textContent = "删除";
    dialog.append(title, message, cancel, confirm);
    if (document.body)
        document.body.append(dialog);
    const close = () => {
        if (typeof dialog.close === "function")
            dialog.close();
        else
            dialog.removeAttribute("open");
        opener.focus();
    };
    cancel.onclick = close;
    dialog.addEventListener("click", (event) => { if (event.target !== dialog)
        return; });
    confirm.onclick = () => void confirmDelete(fileId, confirm, close, deps, active);
    if (typeof dialog.showModal === "function")
        dialog.showModal();
    else
        dialog.setAttribute("open", "");
    cancel.focus();
}
async function confirmDelete(fileId, button, close, deps, active) {
    if (active.has(button))
        return;
    active.add(button);
    button.disabled = true;
    try {
        await deps.deleteFile(fileId);
        close();
        deps.refresh("delete");
        deps.setStatus("文件已删除");
    }
    catch (cause) {
        close();
        deps.setStatus(cause instanceof ConsoleApiError && cause.status === 409 ? "文件正在处理中，暂不能删除" : safeMessage(cause));
    }
    finally {
        active.delete(button);
        button.disabled = false;
    }
}
function actionButton(fileId, label) {
    const buttons = document.querySelectorAll("button[data-file-id]");
    for (const button of buttons) {
        if (button instanceof HTMLButtonElement && button.dataset.fileId === fileId && button.textContent === label)
            return button;
    }
    return null;
}
function safeMessage(cause) {
    return cause instanceof Error ? cause.message : "操作失败，请稍后重试";
}
