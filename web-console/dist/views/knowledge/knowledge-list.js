import { formatBytes, formatDateTime, renderKnowledgeStatus } from "./knowledge-status.js";
const HEADERS = ["文件名", "类型", "大小", "上传时间", "最近处理", "状态", "失败原因", "操作"];
export function renderKnowledgeList(target, items, callbacks) {
    const table = document.createElement("table");
    table.className = "knowledge-table";
    const head = document.createElement("thead");
    const headRow = document.createElement("tr");
    for (const label of HEADERS) {
        const cell = document.createElement("th");
        cell.textContent = label;
        headRow.append(cell);
    }
    head.append(headRow);
    const body = document.createElement("tbody");
    for (const item of items)
        body.append(knowledgeRow(item, callbacks));
    table.append(head, body);
    target.replaceChildren(table);
}
export function renderKnowledgeEmpty(target) {
    const hint = document.createElement("p");
    hint.className = "hint";
    hint.textContent = "暂无知识库文件";
    target.replaceChildren(hint);
}
function knowledgeRow(item, callbacks) {
    const row = document.createElement("tr");
    const filename = document.createElement("td");
    filename.textContent = item.filename;
    const badge = document.createElement("span");
    badge.className = "knowledge-source-badge";
    badge.textContent = item.source === "managed" ? "托管" : "目录";
    filename.append(document.createTextNode(" "), badge);
    row.append(filename, textCell(shortContentType(item.content_type)), textCell(formatBytes(item.size)));
    row.append(textCell(formatDateTime(item.uploaded_at)), textCell(formatDateTime(latestProcessingTime(item))));
    const status = document.createElement("td");
    const statusBadge = document.createElement("span");
    renderKnowledgeStatus(statusBadge, item);
    status.append(statusBadge);
    row.append(status, errorCell(item.error_summary), actionsCell(item, callbacks));
    return row;
}
function textCell(value) {
    const cell = document.createElement("td");
    cell.textContent = value;
    return cell;
}
function errorCell(error) {
    const cell = textCell(error || "—");
    cell.className = "knowledge-error-summary";
    if (error)
        cell.title = error;
    return cell;
}
function actionsCell(item, callbacks) {
    const cell = document.createElement("td");
    cell.className = "knowledge-actions";
    if (item.source === "directory") {
        cell.textContent = "—";
        return cell;
    }
    if (item.downloadable)
        cell.append(actionButton("下载", item, "knowledge-action secondary", callbacks.onDownload));
    if (item.status === "failed")
        cell.append(actionButton("重新处理", item, "knowledge-action secondary", callbacks.onRetry));
    if (item.status !== "processing")
        cell.append(actionButton("删除", item, "knowledge-action knowledge-action--danger danger", callbacks.onDelete));
    return cell;
}
function actionButton(label, item, className, callback) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = className;
    button.textContent = label;
    if (item.file_id !== null)
        button.dataset.fileId = item.file_id;
    button.onclick = () => callback?.(item);
    return button;
}
function shortContentType(contentType) {
    return contentType.startsWith(".") ? contentType.slice(1) : contentType;
}
function latestProcessingTime(item) {
    if (item.processing_started_at === null)
        return item.processed_at;
    if (item.processed_at === null)
        return item.processing_started_at;
    return item.processing_started_at > item.processed_at ? item.processing_started_at : item.processed_at;
}
