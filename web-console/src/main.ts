import {
  ConsoleApiError,
  fetchBootstrap,
  fetchConsoleStatus,
  fetchSession,
  fetchUserPreferences,
  listUserFiles,
  readUserFile,
  updateUserPreferences,
  uploadUserFile,
  deleteUserFile,
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
import { initializeConfiguration } from "./views/configuration/configuration.js";
import type { BootstrapStatus } from "./types.js";
import { createThemeController } from "./theme.js";
import { bindConsoleNavigation } from "./console-shell.js";
import { initializeTodo } from "./views/todo/todo.js";
import { createBackgroundController, installBackgroundConsoleUnlock, unlockPreferencePatch, type BackgroundFile } from "./background.js";
import { cacheFileBlob, clearFileBlobCache, deleteCachedFileBlob, readCachedFileBlob } from "./file-cache.js";

let localStorage: Storage | null = null;
try {
  localStorage = window.localStorage;
} catch (cause) {
  if (!(cause instanceof Error)) throw cause;
}
const themeController = createThemeController(localStorage, document.documentElement);
const backgroundController = createBackgroundController(document.documentElement, document, (file, forceRefresh) => readFileWithCache(file, forceRefresh), async () => {
  if (!userDataController) return;
  try {
    // 解锁与切换特殊背景一次提交：kuliantnt、backgroundMode、activeBackgroundFileId 三个字段
    // 同一次写入，避免刷新后只保留解锁标记而背景仍是旧的自定义背景。
    await userDataController.updatePreferences(unlockPreferencePatch());
  } catch (cause) {
    setText("configuration-result", cause instanceof Error ? cause.message : "特殊背景解锁状态保存失败");
    // 持久化失败必须回滚本地 special，控制器据此恢复解锁前的背景，避免服务端与本地分裂。
    throw cause;
  }
});
installBackgroundConsoleUnlock(window, backgroundController);

/** 优先读本地缓存；未命中或强制刷新时走现有 POST 读取，并把结果尽力写回缓存。 */
async function readFileWithCache(file: BackgroundFile, forceRefresh?: boolean): Promise<Blob> {
  if (!forceRefresh) {
    const cached = await readCachedFileBlob(file.url);
    if (cached) return cached;
  }
  const blob = await readUserFile({
    fileId: file.fileId,
    filename: file.filename,
    contentType: "image/*",
    size: 0,
    createdAt: "",
    url: file.url,
  });
  void cacheFileBlob(file.url, blob);
  return blob;
}

const statusError = requiredElement("status-error", HTMLElement);
const authForm = requiredElement("auth-form", HTMLFormElement);
const logoutButton = requiredElement("logout", HTMLButtonElement);
let bootstrapStatus: BootstrapStatus | null = null;
let authMode: "initialize" | "login" | "password-reset" = "login";
let appBound = false;
let autoRefreshTimer: number | undefined;
let refreshInFlight = false;
let userDataController: import("./views/configuration/configuration.js").UserDataController | null = null;

const AUTO_REFRESH_INTERVAL_MS = 30_000;

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
 bindConsoleNavigation(backgroundController);
void initialize();

function bindAutoRefresh(): void {
  const toggle = requiredElement("auto-refresh", HTMLInputElement);
  toggle.addEventListener("change", () => {
    toggle.checked ? startAutoRefresh() : stopAutoRefresh();
  });
}

function startAutoRefresh(): void {
  window.clearInterval(autoRefreshTimer);
  autoRefreshTimer = window.setInterval(() => {
    if (document.visibilityState === "visible") void refreshStatus();
  }, AUTO_REFRESH_INTERVAL_MS);
}

function stopAutoRefresh(): void {
  window.clearInterval(autoRefreshTimer);
  autoRefreshTimer = undefined;
  requiredElement("auto-refresh", HTMLInputElement).checked = false;
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
      ? `请在运行目录读取 ${status.tokenFile}；可粘贴完整令牌字符串或仅粘贴 token。同一个短时单次重置令牌也只在新生成时输出一次到控制台。重置成功后令牌与旧管理员会话全部失效。`
      : status.initialized
      ? "管理员会话与聊天 session 相互独立。"
      : `请在运行目录读取 ${status.tokenFile}；可粘贴完整令牌字符串或仅粘贴 token。同一个短时单次令牌只在新生成时输出一次到控制台，使用成功后立即失效。`,
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
  requiredElement("auto-refresh", HTMLInputElement).checked = true;
  startAutoRefresh();
  if (!appBound) {
    bindMarkdownPreview();
    appBound = true;
  }
  await Promise.all([refreshStatus(), hydrateUserData()]);
  await refreshConfiguration();
  await initializeTodo();
}

async function hydrateUserData(): Promise<void> {
  try {
    const [preferences, files] = await Promise.all([fetchUserPreferences(), listUserFiles()]);
    // 服务端仅保存自定义色；主题预设继续沿用认证前从 localStorage 恢复的选择。
    themeController.hydrate({
      preset: themeController.current().preset,
      customColors: preferences.customColors,
    });
    await backgroundController.hydrate({
      fileIds: preferences.backgroundFileIds,
      activeFileId: preferences.activeBackgroundFileId,
      mode: preferences.backgroundMode,
      kuliantnt: preferences.kuliantnt,
    }, files);
    let currentPreferences = preferences;
    let currentFiles = files;
    const dataController: import("./views/configuration/configuration.js").UserDataController = {
      get preferences() { return currentPreferences; },
      get files() { return currentFiles; },
      updatePreferences: async (patch) => {
        currentPreferences = await updateUserPreferences(patch);
        return currentPreferences;
      },
      uploadFile: async (file) => {
        const uploaded = await uploadUserFile(file);
        currentFiles = [...currentFiles, uploaded];
        return uploaded;
      },
      deleteFile: async (file) => {
        await deleteUserFile(file.fileId);
        currentFiles = currentFiles.filter((candidate) => candidate.fileId !== file.fileId);
        void deleteCachedFileBlob(file.url);
        // 删除后以服务端返回的完整偏好为准，避免乐观更新与真实状态不一致。
        currentPreferences = await fetchUserPreferences();
      },
    };
    userDataController = dataController;
    try {
      // 认证成功后服务端偏好是唯一权威：把旧 cookie 一次性迁移进服务端偏好并清理。
      await backgroundController.migrateFromLegacy({
        kuliantnt: preferences.kuliantnt,
        backgroundMode: preferences.backgroundMode,
      }, async (patch) => {
        currentPreferences = await dataController.updatePreferences(patch);
      });
    } catch (cause) {
      setText("configuration-result", cause instanceof Error ? cause.message : "背景解锁状态迁移失败");
    }
  } catch (cause) {
    userDataController = null;
    backgroundController.dispose();
    setText("configuration-result", cause instanceof Error ? cause.message : "用户界面偏好加载失败");
  }
}

async function refreshConfiguration(): Promise<void> {
  try {
    await initializeConfiguration(themeController, backgroundController, userDataController);
  } catch (cause) {
    setText("configuration-result", cause instanceof Error ? cause.message : "配置加载失败");
  }
}

async function logout(): Promise<void> {
  try {
    await logoutAdmin();
  } finally {
    backgroundController.dispose();
    void clearFileBlobCache();
    userDataController = null;
    stopAutoRefresh();
    bootstrapStatus = null;
    authMode = "login";
    requiredElement("auth-password", HTMLInputElement).value = "";
    await initialize();
  }
}

async function refreshStatus(): Promise<void> {
  if (refreshInFlight) return;
  refreshInFlight = true;
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
    refreshInFlight = false;
  }
}
