import type { KnowledgeFileItem, KnowledgeFileStatus } from "../../types.js";

export function knowledgeStatusMeta(status: KnowledgeFileStatus): { label: string; className: string } {
  switch (status) {
    case "pending": return { label: "等待处理", className: "knowledge-status--pending" };
    case "processing": return { label: "处理中", className: "knowledge-status--processing" };
    case "ready": return { label: "已完成", className: "knowledge-status--ready" };
    case "failed": return { label: "处理失败", className: "knowledge-status--failed" };
  }
}

export function renderKnowledgeStatus(target: HTMLElement, item: KnowledgeFileItem): void {
  const meta = knowledgeStatusMeta(item.status);
  target.className = `knowledge-status ${meta.className}`;
  target.replaceChildren();
  const dot = document.createElement("span");
  dot.className = "knowledge-status-dot";
  dot.setAttribute("aria-hidden", "true");
  target.append(dot, document.createTextNode(meta.label));
}

export function formatBytes(size: number | null): string {
  if (size === null || size === 0) return "—";
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${formatDecimal(size / 1024)} KB`;
  return `${formatDecimal(size / (1024 * 1024))} MB`;
}

export function formatDateTime(value: string | null): string {
  return value ? value.replace("T", " ").slice(0, 16) : "—";
}

function formatDecimal(value: number): string {
  return Number(value.toFixed(1)).toString();
}
