import { getDefaultStore, atom } from "jotai";
import {
  ConsoleApiError,
  fetchBootstrap,
  fetchSession,
  issuePreAuth,
  loginAdmin,
  initializeAdmin,
  logoutAdmin,
  requestPasswordReset,
  resetAdminPassword,
  setCsrfToken,
  setUnauthorizedHandler,
} from "../api.js";
import type { AdminSession, BootstrapStatus } from "../types.js";

/** 认证门的三态：启动检查中、未认证（显示登录/初始化表单）、已认证。 */
export type AuthPhase = "checking" | "unauthenticated" | "authenticated";
export type AuthMode = "initialize" | "login" | "password-reset";
export type AuthMessage = { readonly kind: "error" | "info"; readonly text: string };

export const authPhaseAtom = atom<AuthPhase>("checking");
export const authModeAtom = atom<AuthMode>("login");
export const bootstrapStatusAtom = atom<BootstrapStatus | null>(null);
export const sessionAtom = atom<AdminSession | null>(null);
export const authMessageAtom = atom<AuthMessage | null>(null);

/** 会话过期重建流程去重：并发 401 只触发一次重新建立认证流程。 */
let sessionResetPromise: Promise<void> | null = null;

function isUnauthorized(cause: unknown): cause is ConsoleApiError {
  return cause instanceof ConsoleApiError && cause.status === 401;
}

/**
 * 启动时恢复既有会话：成功进入控制台；401 则建立 pre-auth 并展示认证表单。
 * 在 React 渲染外执行，通过 jotai 默认 store 通知 UI。
 */
export async function bootstrapAuth(): Promise<void> {
  setUnauthorizedHandler(() => {
    const phase = getDefaultStore().get(authPhaseAtom);
    if (phase === "authenticated") void handleSessionExpired();
  });
  const store = getDefaultStore();
  try {
    const session = await fetchSession();
    store.set(sessionAtom, session);
    store.set(authMessageAtom, null);
    store.set(authPhaseAtom, "authenticated");
  } catch (cause) {
    if (!isUnauthorized(cause)) {
      store.set(authMessageAtom, { kind: "error", text: cause instanceof Error ? cause.message : "认证状态加载失败" });
      store.set(authPhaseAtom, "unauthenticated");
      return;
    }
    await prepareAuthForms();
  }
}

/** 建立 pre-auth 会话并决定初始表单模式（首次初始化 or 登录）。 */
export async function prepareAuthForms(): Promise<void> {
  const store = getDefaultStore();
  try {
    const status = await fetchBootstrap();
    await issuePreAuth();
    store.set(bootstrapStatusAtom, status);
    store.set(authModeAtom, status.initialized ? "login" : "initialize");
    store.set(authPhaseAtom, "unauthenticated");
  } catch (cause) {
    store.set(authMessageAtom, { kind: "error", text: cause instanceof Error ? cause.message : "初始化认证流程失败" });
    store.set(authPhaseAtom, "unauthenticated");
  }
}

/** 登录 / 首次初始化 / 密码重置共用入口；成功后进入控制台并清理敏感输入状态。 */
export async function submitAuth(mode: AuthMode, username: string, password: string, bootstrapToken: string): Promise<void> {
  const store = getDefaultStore();
  const session = mode === "initialize"
    ? await initializeAdmin(username, password, bootstrapToken)
    : mode === "password-reset"
      ? await resetAdminPassword(password, bootstrapToken)
      : await loginAdmin(username, password);
  store.set(sessionAtom, session);
  store.set(authMessageAtom, null);
  store.set(authPhaseAtom, "authenticated");
}

/** 生成密码重置令牌并切换到重置表单。 */
export async function startPasswordReset(): Promise<void> {
  const store = getDefaultStore();
  const status = await requestPasswordReset();
  store.set(bootstrapStatusAtom, status);
  store.set(authModeAtom, "password-reset");
}

/** 从重置表单返回登录。 */
export function cancelPasswordReset(): void {
  const store = getDefaultStore();
  const status = store.get(bootstrapStatusAtom);
  store.set(authModeAtom, status?.initialized ? "login" : "initialize");
}

/** 退出登录：先发起注销请求捕获当前 CSRF，再立即失效本地会话。 */
export async function logout(): Promise<void> {
  const store = getDefaultStore();
  const logoutRequest = logoutAdmin().catch(() => undefined);
  store.set(authPhaseAtom, "unauthenticated");
  store.set(sessionAtom, null);
  store.set(authMessageAtom, { kind: "info", text: "正在退出登录…" });
  setCsrfToken("");
  await logoutRequest;
  await prepareAuthForms();
}

/** 会话过期：回到认证门并重新建立 pre-auth 表单状态。 */
export async function handleSessionExpired(): Promise<void> {
  if (sessionResetPromise) return sessionResetPromise;
  const store = getDefaultStore();
  if (store.get(authPhaseAtom) !== "authenticated") return Promise.resolve();

  store.set(sessionAtom, null);
  store.set(authPhaseAtom, "unauthenticated");
  store.set(authMessageAtom, { kind: "info", text: "登录会话已过期，请重新登录。" });

  sessionResetPromise = (async () => {
    try {
      await prepareAuthForms();
    } finally {
      sessionResetPromise = null;
    }
  })();
  return sessionResetPromise;
}
