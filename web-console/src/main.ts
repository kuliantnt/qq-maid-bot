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
import { requiredElement, setText, togglePasswordReveal } from "./dom.js";
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
let autoRefreshTimer: number | undefined;

const AUTO_REFRESH_INTERVAL_MS = 30_000;

refreshButton.addEventListener("click", () => void refreshStatus());
authForm.addEventListener("submit", (event) => {
  event.preventDefault();
  void submitAuth();
});
logoutButton.addEventListener("click", () => void logout());
requiredElement("password-reset", HTMLButtonElement).addEventListener("click", () => void togglePasswordReset());
for (const [buttonId, inputId] of [["auth-password-reveal", "auth-password"], ["bootstrap-token-reveal", "bootstrap-token"]] as const) {
  const input = requiredElement(inputId, HTMLInputElement);
  requiredElement(buttonId, HTMLButtonElement).addEventListener("click", () => togglePasswordReveal(requiredElement(buttonId, HTMLButtonElement), input));
}
// Toast 支持点击立即关闭，不必等待自动隐藏计时。
requiredElement("console-toast", HTMLElement).addEventListener("click", (event) => {
  (event.currentTarget as HTMLElement).hidden = true;
});
bindAutoRefresh();
void initialize();

function bindAutoRefresh(): void {
  const toggle = requiredElement("auto-refresh", HTMLInputElement);
  toggle.addEventListener("change", () => {
    window.clearInterval(autoRefreshTimer);
    autoRefreshTimer = undefined;
    if (!toggle.checked) return;
    autoRefreshTimer = window.setInterval(() => {
      // 页面不可见或手动刷新进行中时跳过，避免叠加请求。
      if (document.visibilityState === "visible" && !refreshButton.disabled) void refreshStatus();
    }, AUTO_REFRESH_INTERVAL_MS);
  });
}

function stopAutoRefresh(): void {
  window.clearInterval(autoRefreshTimer);
  autoRefreshTimer = undefined;
  requiredElement("auto-refresh", HTMLInputElement).checked = false;
}

/** 导航 scrollspy：滚动时高亮当前视口内区块对应的导航项。 */
function bindNavSpy(): void {
  const links = [...document.querySelectorAll<HTMLAnchorElement>(".nav a[href^='#']")];
  if (links.length === 0 || !("IntersectionObserver" in window)) return;
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        for (const link of links) link.classList.toggle("active", link.hash.slice(1) === entry.target.id);
      }
    },
    { rootMargin: "-30% 0px -60% 0px" },
  );
  for (const link of links) {
    const section = document.getElementById(link.hash.slice(1));
    if (section) observer.observe(section);
  }
}

function clearCredentialInput(inputId: string, revealButtonId: string): void {
  const input = requiredElement(inputId, HTMLInputElement);
  const reveal = requiredElement(revealButtonId, HTMLButtonElement);
  input.value = "";
  input.type = "password";
  reveal.textContent = "显示";
  reveal.setAttribute("aria-pressed", "false");
}

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
  const previousLabel = submit.textContent;
  submit.textContent = "验证中…";
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
    submit.textContent = previousLabel;
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
  // 认证完成后立即清掉隐藏表单中的密码和一次性 token，避免明文显示状态残留。
  clearCredentialInput("auth-password", "auth-password-reveal");
  clearCredentialInput("bootstrap-token", "bootstrap-token-reveal");
  requiredElement("auth-shell", HTMLElement).hidden = true;
  for (const item of document.querySelectorAll<HTMLElement>("[data-authenticated]")) item.hidden = false;
  setText("admin-username", username);
  if (!appBound) {
    bindMarkdownPreview();
    bindNavSpy();
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
    stopAutoRefresh();
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
