import { renderMarkdown } from "../api.js";
import { requiredElement } from "../dom.js";

export function bindMarkdownPreview(): void {
  const input = requiredElement("markdown-input", HTMLTextAreaElement);
  const button = requiredElement("markdown-render", HTMLButtonElement);
  const output = requiredElement("markdown-output", HTMLElement);
  const error = requiredElement("markdown-error", HTMLElement);

  button.addEventListener("click", async () => {
    button.disabled = true;
    error.textContent = "";
    try {
      const sanitizedHtml = await renderMarkdown(input.value);
      // 只有 Rust 后端 ammonia 清理后的 Markdown HTML 允许进入 innerHTML。
      output.innerHTML = sanitizedHtml;
    } catch (cause: unknown) {
      output.replaceChildren();
      error.textContent = cause instanceof Error ? cause.message : "Markdown 渲染失败";
    } finally {
      button.disabled = false;
    }
  });
}
