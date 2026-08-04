import { ConsoleApiError } from "../../api.js";
import { requiredElement } from "../../dom.js";
import type { KnowledgeFileCapabilities, KnowledgeFileItem } from "../../types.js";

export type UploadValidation = { ok: true; file: File } | { ok: false; reason: string };

export function formatFileSizeLimit(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${Number((bytes / (1024 * 1024)).toFixed(1))} MB`;
  if (bytes >= 1024) return `${Number((bytes / 1024).toFixed(1))} KB`;
  return `${bytes} B`;
}

type KnowledgeUploadDependencies = {
  readonly inputId: string;
  readonly buttonId: string;
  readonly setStatus: (text: string) => void;
  readonly getCapabilities: () => KnowledgeFileCapabilities | null;
  readonly upload: (file: File) => Promise<KnowledgeFileItem>;
  readonly onUploaded: () => void;
};

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

export function installKnowledgeUpload(deps: KnowledgeUploadDependencies): void {
  const button = requiredElement(deps.buttonId, HTMLButtonElement);
  const input = document.createElement("input");
  input.id = deps.inputId;
  input.type = "file";
  input.hidden = true;
  input.accept = deps.getCapabilities()?.supported_extensions.join(",") ?? "";
  document.body.append(input);
  button.onclick = () => input.click();
  input.addEventListener("change", () => { void uploadSelectedFile(input, button, deps); });
}

async function uploadSelectedFile(
  input: HTMLInputElement,
  button: HTMLButtonElement,
  deps: KnowledgeUploadDependencies,
): Promise<void> {
  const file = input.files?.[0];
  if (file === undefined) return;
  const capabilities = deps.getCapabilities();
  if (capabilities !== null) {
    const validation = validateKnowledgeFile(file, capabilities);
    if (!validation.ok) {
      deps.setStatus(`上传已阻止：${validation.reason}`);
      input.value = "";
      return;
    }
  }
  button.disabled = true;
  deps.setStatus("上传中…");
  try {
    await deps.upload(file);
    deps.setStatus("文件已上传，正在等待处理");
    deps.onUploaded();
  } catch (cause) {
    deps.setStatus(cause instanceof ConsoleApiError ? `${cause.message}（${cause.code}）` : "文件上传失败");
  } finally {
    button.disabled = false;
    input.value = "";
  }
}
