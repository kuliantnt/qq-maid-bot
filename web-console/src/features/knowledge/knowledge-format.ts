import type { KnowledgeFileCapabilities, KnowledgeFileItem, KnowledgeFileStatus } from "../../types.js";

/** 知识库状态 → 展示文案与信号 tone 的映射（DESIGN.md §5：pending/processing 警告、ready 成功、failed 错误）。 */
export function knowledgeStatusMeta(status: KnowledgeFileStatus): { label: string; tone: "success" | "warning" | "error" } {
  switch (status) {
    case "pending": return { label: "等待处理", tone: "warning" };
    case "processing": return { label: "处理中", tone: "warning" };
    case "ready": return { label: "已完成", tone: "success" };
    case "failed": return { label: "处理失败", tone: "error" };
  }
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

export function formatFileSizeLimit(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${Number((bytes / (1024 * 1024)).toFixed(1))} MB`;
  if (bytes >= 1024) return `${Number((bytes / 1024).toFixed(1))} KB`;
  return `${bytes} B`;
}

export function shortContentType(contentType: string): string {
  return contentType.startsWith(".") ? contentType.slice(1) : contentType;
}

export function latestProcessingTime(item: KnowledgeFileItem): string | null {
  if (item.processing_started_at === null) return item.processed_at;
  if (item.processed_at === null) return item.processing_started_at;
  return item.processing_started_at > item.processed_at ? item.processing_started_at : item.processed_at;
}

export type UploadValidation = { ok: true; file: File } | { ok: false; reason: string };

/** 上传前置校验：扩展名、大小上限与文件名长度，均以后端能力快照为准。 */
export function validateKnowledgeFile(file: File, capabilities: KnowledgeFileCapabilities): UploadValidation {
  const extensions = capabilities.supported_extensions.map((extension) => extension.toLowerCase());
  if (!extensions.some((extension) => file.name.toLowerCase().endsWith(extension))) {
    return { ok: false, reason: `仅支持 ${capabilities.supported_extensions.join(" / ")} 文件` };
  }
  if (file.size > capabilities.max_file_bytes) {
    return { ok: false, reason: `文件大小超过上限（${formatFileSizeLimit(capabilities.max_file_bytes)}）` };
  }
  if (file.name.length > capabilities.max_filename_chars) return { ok: false, reason: "文件名过长" };
  return { ok: true, file };
}

function formatDecimal(value: number): string {
  return Number(value.toFixed(1)).toString();
}
