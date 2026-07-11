import { fetchConsoleStatus } from "./api.js";
import { requiredElement, setText } from "./dom.js";
import { renderDashboard } from "./views/dashboard.js";
import { bindMarkdownPreview } from "./views/markdown.js";
import { renderPlatforms } from "./views/platforms.js";
import { renderStorage } from "./views/storage.js";

const refreshButton = requiredElement("refresh-status", HTMLButtonElement);
const statusError = requiredElement("status-error", HTMLElement);

refreshButton.addEventListener("click", () => void refreshStatus());
bindMarkdownPreview();
void refreshStatus();

async function refreshStatus(): Promise<void> {
  refreshButton.disabled = true;
  refreshButton.textContent = "刷新中…";
  statusError.textContent = "";
  try {
    const status = await fetchConsoleStatus();
    renderDashboard(status);
    renderPlatforms(status.platforms);
    renderStorage(status.storage);
    setText("last-refresh", new Date().toLocaleString());
  } catch (cause: unknown) {
    statusError.textContent = cause instanceof Error ? cause.message : "状态刷新失败";
  } finally {
    refreshButton.disabled = false;
    refreshButton.textContent = "手动刷新";
  }
}
