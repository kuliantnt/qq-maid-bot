import { ConsoleApiError, fetchBootstrap, fetchConsoleStatus, fetchSession, issuePreAuth, initializeAdmin, loginAdmin, logoutAdmin, } from "./api.js";
import { requiredElement, setText } from "./dom.js";
import { renderDashboard } from "./views/dashboard.js";
import { bindMarkdownPreview } from "./views/markdown.js";
import { renderPlatforms } from "./views/platforms.js";
import { renderStorage } from "./views/storage.js";
import { initializeConfiguration } from "./views/configuration.js";
const refreshButton = requiredElement("refresh-status", HTMLButtonElement);
const statusError = requiredElement("status-error", HTMLElement);
const authForm = requiredElement("auth-form", HTMLFormElement);
const logoutButton = requiredElement("logout", HTMLButtonElement);
let bootstrapStatus = null;
let appBound = false;
refreshButton.addEventListener("click", () => void refreshStatus());
authForm.addEventListener("submit", (event) => {
    event.preventDefault();
    void submitAuth();
});
logoutButton.addEventListener("click", () => void logout());
void initialize();
async function initialize() {
    try {
        const session = await fetchSession();
        await showConsole(session.username);
    }
    catch (cause) {
        if (!(cause instanceof ConsoleApiError) || cause.status !== 401) {
            setText("auth-error", cause instanceof Error ? cause.message : "认证状态加载失败");
            return;
        }
        try {
            const status = await fetchBootstrap();
            await issuePreAuth();
            bootstrapStatus = status;
            renderAuth(status);
        }
        catch (bootstrapCause) {
            setText("auth-error", bootstrapCause instanceof Error ? bootstrapCause.message : "初始化认证流程失败");
        }
    }
}
function renderAuth(status) {
    requiredElement("auth-shell", HTMLElement).hidden = false;
    for (const item of document.querySelectorAll("[data-authenticated]"))
        item.hidden = true;
    const tokenGroup = requiredElement("bootstrap-token-group", HTMLElement);
    tokenGroup.hidden = status.initialized;
    setText("auth-title", status.initialized ? "部署管理员登录" : "建立首位部署管理员");
    setText("auth-submit", status.initialized ? "登录控制台" : "完成安全初始化");
    setText("bootstrap-help", status.initialized
        ? "管理员会话与聊天 session 相互独立。"
        : `请在服务器读取 ${status.tokenFile}；令牌短时有效、仅可使用一次，且不会显示在页面或日志中。`);
}
async function submitAuth() {
    const username = requiredElement("auth-username", HTMLInputElement).value;
    const password = requiredElement("auth-password", HTMLInputElement).value;
    const submit = requiredElement("auth-submit", HTMLButtonElement);
    submit.disabled = true;
    setText("auth-error", "");
    try {
        const session = bootstrapStatus?.initialized === false
            ? await initializeAdmin(username, password, requiredElement("bootstrap-token", HTMLInputElement).value)
            : await loginAdmin(username, password);
        await showConsole(session.username);
    }
    catch (cause) {
        setText("auth-error", cause instanceof Error ? cause.message : "认证失败");
    }
    finally {
        submit.disabled = false;
    }
}
async function showConsole(username) {
    requiredElement("auth-shell", HTMLElement).hidden = true;
    for (const item of document.querySelectorAll("[data-authenticated]"))
        item.hidden = false;
    setText("admin-username", username);
    if (!appBound) {
        bindMarkdownPreview();
        appBound = true;
    }
    await Promise.all([refreshStatus(), refreshConfiguration()]);
}
async function refreshConfiguration() {
    try {
        await initializeConfiguration();
    }
    catch (cause) {
        setText("configuration-result", cause instanceof Error ? cause.message : "配置加载失败");
    }
}
async function logout() {
    try {
        await logoutAdmin();
    }
    finally {
        bootstrapStatus = null;
        requiredElement("auth-password", HTMLInputElement).value = "";
        await initialize();
    }
}
async function refreshStatus() {
    refreshButton.disabled = true;
    refreshButton.textContent = "刷新中…";
    statusError.textContent = "";
    try {
        const status = await fetchConsoleStatus();
        renderDashboard(status);
        renderPlatforms(status.platforms);
        renderStorage(status.storage);
        setText("last-refresh", new Date().toLocaleString());
    }
    catch (cause) {
        statusError.textContent = cause instanceof Error ? cause.message : "状态刷新失败";
    }
    finally {
        refreshButton.disabled = false;
        refreshButton.textContent = "手动刷新";
    }
}
