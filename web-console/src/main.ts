import {
  ConsoleApiError,
  fetchBootstrap,
  fetchConsoleStatus,
  fetchSession,
  issuePreAuth,
  initializeAdmin,
  loginAdmin,
  logoutAdmin,
  requestPasswordReset,
  resetAdminPassword,
} from "./api.js";
import { requiredElement, setText } from "./dom.js";
import { renderDashboard } from "./views/dashboard.js";
import { bindMarkdownPreview } from "./views/markdown.js";
import { renderPlatforms } from "./views/platforms.js";
import { renderStorage } from "./views/storage.js";
import { initializeConfiguration } from "./views/configuration.js";
import type { BootstrapStatus } from "./types.js";

const refreshButton = requiredElement("refresh-status", HTMLButtonElement);
const statusError = requiredElement("status-error", HTMLElement);
const authForm = requiredElement("auth-form", HTMLFormElement);
const logoutButton = requiredElement("logout", HTMLButtonElement);
let bootstrapStatus: BootstrapStatus | null = null;
let authMode: "initialize" | "login" | "password-reset" = "login";
let appBound = false;

refreshButton.addEventListener("click", () => void refreshStatus());
authForm.addEventListener("submit", (event) => {
  event.preventDefault();
  void submitAuth();
});
logoutButton.addEventListener("click", () => void logout());
requiredElement("password-reset", HTMLButtonElement).addEventListener("click", () => void togglePasswordReset());
void initialize();

async function initialize(): Promise<void> {
  try {
    const session = await fetchSession();
    await showConsole(session.username);
  } catch (cause) {
    if (!(cause instanceof ConsoleApiError) || cause.status !== 401) {
      setText("auth-error", cause instanceof Error ? cause.message : "认证状态加载失败");
      return;
    }
    try {
      const status = await fetchBootstrap();
      await issuePreAuth();
      bootstrapStatus = status;
      authMode = status.initialized ? "login" : "initialize";
      renderAuth(status);
    } catch (bootstrapCause) {
      setText("auth-error", bootstrapCause instanceof Error ? bootstrapCause.message : "初始化认证流程失败");
    }
  }
}

function renderAuth(status: BootstrapStatus): void {
  requiredElement("auth-shell", HTMLElement).hidden = false;
  for (const item of document.querySelectorAll<HTMLElement>("[data-authenticated]")) item.hidden = true;
  const tokenGroup = requiredElement("bootstrap-token-group", HTMLElement);
  const resetting = authMode === "password-reset";
  tokenGroup.hidden = authMode === "login";
  requiredElement("auth-username-group", HTMLElement).hidden = resetting;
  const username = requiredElement("auth-username", HTMLInputElement);
  username.required = !resetting;
  const password = requiredElement("auth-password", HTMLInputElement);
  password.autocomplete = resetting || authMode === "initialize" ? "new-password" : "current-password";
  setText("auth-password-label", resetting ? "新管理员密码" : "管理员密码");
  setText("auth-title", resetting ? "重置部署管理员密码" : status.initialized ? "部署管理员登录" : "建立首位部署管理员");
  setText("auth-submit", resetting ? "完成密码重置" : status.initialized ? "登录控制台" : "完成安全初始化");
  const reset = requiredElement("password-reset", HTMLButtonElement);
  reset.hidden = !status.initialized;
  reset.textContent = resetting ? "返回密码登录" : "重置管理员密码";
  setText(
    "bootstrap-help",
    resetting
      ? `请在运行目录读取 ${status.tokenFile}；同一个短时单次重置令牌也只在新生成时输出一次到控制台。重置成功后令牌与旧管理员会话全部失效。`
      : status.initialized
      ? "管理员会话与聊天 session 相互独立。"
      : `请在运行目录读取 ${status.tokenFile}；同一个短时单次令牌只在新生成时输出一次到控制台，使用成功后立即失效。`,
  );
}

async function submitAuth(): Promise<void> {
  const username = requiredElement("auth-username", HTMLInputElement).value;
  const password = requiredElement("auth-password", HTMLInputElement).value;
  const submit = requiredElement("auth-submit", HTMLButtonElement);
  submit.disabled = true;
  setText("auth-error", "");
  try {
    const bootstrapToken = requiredElement("bootstrap-token", HTMLInputElement).value;
    const session = authMode === "initialize"
      ? await initializeAdmin(username, password, bootstrapToken)
      : authMode === "password-reset"
        ? await resetAdminPassword(password, bootstrapToken)
        : await loginAdmin(username, password);
    await showConsole(session.username);
  } catch (cause) {
    setText("auth-error", cause instanceof Error ? cause.message : "认证失败");
  } finally {
    submit.disabled = false;
  }
}

async function togglePasswordReset(): Promise<void> {
  if (!bootstrapStatus?.initialized) return;
  const button = requiredElement("password-reset", HTMLButtonElement);
  setText("auth-error", "");
  if (authMode === "password-reset") {
    authMode = "login";
    renderAuth(bootstrapStatus);
    return;
  }
  button.disabled = true;
  try {
    bootstrapStatus = await requestPasswordReset();
    authMode = "password-reset";
    requiredElement("auth-password", HTMLInputElement).value = "";
    requiredElement("bootstrap-token", HTMLInputElement).value = "";
    renderAuth(bootstrapStatus);
  } catch (cause) {
    setText("auth-error", cause instanceof Error ? cause.message : "密码重置令牌生成失败");
  } finally {
    button.disabled = false;
  }
}

async function showConsole(username: string): Promise<void> {
  requiredElement("auth-shell", HTMLElement).hidden = true;
  for (const item of document.querySelectorAll<HTMLElement>("[data-authenticated]")) item.hidden = false;
  setText("admin-username", username);
  if (!appBound) {
    bindMarkdownPreview();
    appBound = true;
  }
  await Promise.all([refreshStatus(), refreshConfiguration()]);
}

async function refreshConfiguration(): Promise<void> {
  try {
    await initializeConfiguration();
  } catch (cause) {
    setText("configuration-result", cause instanceof Error ? cause.message : "配置加载失败");
  }
}

async function logout(): Promise<void> {
  try {
    await logoutAdmin();
  } finally {
    bootstrapStatus = null;
    authMode = "login";
    requiredElement("auth-password", HTMLInputElement).value = "";
    await initialize();
  }
}

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
