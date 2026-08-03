import { formatBytes, formatDateTime, renderKnowledgeStatus } from "./knowledge-status.js";
import type { KnowledgeFileItem } from "../../types.js";

export type KnowledgeListCallbacks = {
  onDownload?: (item: KnowledgeFileItem) => void;
  onDelete?: (item: KnowledgeFileItem) => void;
  onRetry?: (item: KnowledgeFileItem) => void;
};

const HEADERS = ["文件名", "类型", "大小", "上传时间", "最近处理", "状态", "失败原因", "操作"] as const;

export function renderKnowledgeList(target: HTMLElement, items: readonly KnowledgeFileItem[], callbacks: KnowledgeListCallbacks): void {
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
  for (const item of items) body.append(knowledgeRow(item, callbacks));
  table.append(head, body);
  target.replaceChildren(table);
}

export function renderKnowledgeEmpty(target: HTMLElement): void {
  const hint = document.createElement("p");
  hint.className = "hint";
  hint.textContent = "暂无知识库文件";
  target.replaceChildren(hint);
}

function knowledgeRow(item: KnowledgeFileItem, callbacks: KnowledgeListCallbacks): HTMLTableRowElement {
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
  renderKnowledgeStatus(status, item);
  row.append(status, errorCell(item.error_summary), actionsCell(item, callbacks));
  return row;
}

function textCell(value: string): HTMLTableCellElement {
  const cell = document.createElement("td");
  cell.textContent = value;
  return cell;
}

function errorCell(error: string | null | undefined): HTMLTableCellElement {
  const cell = textCell(error || "—");
  cell.className = "knowledge-error-summary";
  if (error) cell.title = error;
  return cell;
}

function actionsCell(item: KnowledgeFileItem, callbacks: KnowledgeListCallbacks): HTMLTableCellElement {
  const cell = document.createElement("td");
  cell.className = "knowledge-actions";
  if (item.source === "directory") {
    cell.textContent = "—";
    return cell;
  }
  if (item.downloadable) cell.append(actionButton("下载", item, "knowledge-action secondary", callbacks.onDownload));
  if (item.status === "failed") cell.append(actionButton("重新处理", item, "knowledge-action secondary", callbacks.onRetry));
  if (item.status !== "processing") cell.append(actionButton("删除", item, "knowledge-action knowledge-action--danger danger", callbacks.onDelete));
  return cell;
}

function actionButton(label: string, item: KnowledgeFileItem, className: string, callback: ((item: KnowledgeFileItem) => void) | undefined): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = className;
  button.textContent = label;
  if (item.file_id !== null) button.dataset.fileId = item.file_id;
  button.onclick = () => callback?.(item);
  return button;
}

function shortContentType(contentType: string): string {
  return contentType.startsWith(".") ? contentType.slice(1) : contentType;
}

function latestProcessingTime(item: KnowledgeFileItem): string | null {
  if (item.processing_started_at === null) return item.processed_at;
  if (item.processed_at === null) return item.processing_started_at;
  return item.processing_started_at > item.processed_at ? item.processing_started_at : item.processed_at;
}
