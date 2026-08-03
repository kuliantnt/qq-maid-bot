import { ConsoleApiError } from "../../api.js";
import { requiredElement } from "../../dom.js";
export function validateKnowledgeFile(file, capabilities) {
    const extensions = capabilities.supported_extensions.map((extension) => extension.toLowerCase());
    if (!extensions.some((extension) => file.name.toLowerCase().endsWith(extension))) {
        return { ok: false, reason: `仅支持 ${capabilities.supported_extensions.join(" / ")} 文件` };
    }
    if (file.size > capabilities.max_file_bytes) {
        return { ok: false, reason: `文件大小超过上限（${capabilities.max_file_bytes / 1024 / 1024} MB）` };
    }
    if (file.name.length > capabilities.max_filename_chars)
        return { ok: false, reason: "文件名过长" };
    return { ok: true, file };
}
export function installKnowledgeUpload(deps) {
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
async function uploadSelectedFile(input, button, deps) {
    const file = input.files?.[0];
    if (file === undefined)
        return;
    const capabilities = deps.getCapabilities();
    if (capabilities !== null) {
        const validation = validateKnowledgeFile(file, capabilities);
        if (!validation.ok)
            deps.setStatus(`警告：${validation.reason}，服务端可能拒绝`);
    }
    button.disabled = true;
    deps.setStatus("上传中…");
    try {
        await deps.upload(file);
        deps.setStatus("文件已上传，正在等待处理");
        deps.onUploaded();
    }
    catch (cause) {
        deps.setStatus(cause instanceof ConsoleApiError ? `${cause.message}（${cause.code}）` : "文件上传失败");
    }
    finally {
        button.disabled = false;
        input.value = "";
    }
}
