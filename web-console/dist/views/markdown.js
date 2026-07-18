import { renderMarkdown } from "../api.js";
import { requiredElement } from "../dom.js";
export function bindMarkdownPreview() {
    const input = requiredElement("markdown-input", HTMLTextAreaElement);
    const button = requiredElement("markdown-render", HTMLButtonElement);
    const output = requiredElement("markdown-output", HTMLElement);
    const error = requiredElement("markdown-error", HTMLElement);
    // Ctrl / Cmd + Enter 在输入框内直接触发渲染，不用移开手去点按钮。
    input.addEventListener("keydown", (event) => {
        if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
            event.preventDefault();
            button.click();
        }
    });
    button.addEventListener("click", async () => {
        button.disabled = true;
        error.textContent = "";
        try {
            const sanitizedHtml = await renderMarkdown(input.value);
            // 只有 Rust 后端 ammonia 清理后的 Markdown HTML 允许进入 innerHTML。
            output.innerHTML = sanitizedHtml;
        }
        catch (cause) {
            output.replaceChildren();
            error.textContent = cause instanceof Error ? cause.message : "Markdown 渲染失败";
        }
        finally {
            button.disabled = false;
        }
    });
}
