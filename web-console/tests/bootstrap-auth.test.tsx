import { QueryClient } from "@tanstack/react-query";
import { createHashHistory, createMemoryHistory, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { useEffect } from "react";
import { getDefaultStore } from "jotai";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ConsoleApiError, fetchBootstrap, fetchSession, issuePreAuth } from "../src/api.js";
import { AppProviders } from "../src/app-providers.js";
import { Route } from "../src/routes/__root.js";
import { authMessageAtom, authPhaseAtom, bootstrapAuth, bootstrapStatusAtom, sessionAtom } from "../src/stores/auth.js";
import { showToast, toastAtom } from "../src/stores/toast.js";
import type { AdminSession } from "../src/types.js";

vi.mock("../src/api.js", async (importOriginal) => ({
  ...await importOriginal<typeof import("../src/api.js")>(),
  fetchSession: vi.fn(),
  fetchBootstrap: vi.fn(),
  issuePreAuth: vi.fn(),
  fetchConsoleStatus: vi.fn(() => new Promise(() => undefined)),
}));
vi.mock("../src/stores/user-data.js", () => ({ hydrateUserData: vi.fn() }));

const store = getDefaultStore();
let client: QueryClient;
beforeEach(() => {
  vi.clearAllMocks();
  // 路由切换会调用 scrollTo；jsdom 没有布局滚动实现。
  vi.spyOn(window, "scrollTo").mockImplementation(() => undefined);
  store.set(authPhaseAtom, "checking");
  store.set(sessionAtom, null);
  store.set(authMessageAtom, null);
  store.set(bootstrapStatusAtom, null);
  store.set(toastAtom, null);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
});
afterEach(() => { cleanup(); client.clear(); store.set(toastAtom, null); vi.restoreAllMocks(); });

async function mountApp() {
  const index = createRoute({ getParentRoute: () => Route, path: "/", component: () => <div>页面内容</div> });
  const router = createRouter({ routeTree: Route.addChildren([index]), history: createMemoryHistory({ initialEntries: ["/"] }) });
  // 复用生产入口 Provider，并使用真实 RootLayout、AppShell、AuthGate 和 Toast Host。
  render(<AppProviders client={client}><RouterProvider router={router} /></AppProviders>);
  await screen.findByText("正在恢复管理员会话…");
  expect(store.get(authPhaseAtom)).toBe("checking");
  expect(screen.getAllByRole("region", { name: "Notifications (F8)" })).toHaveLength(1);
}

async function expectSingleToast() {
  act(() => showToast("info", "启动测试通知"));
  await waitFor(() => {
    expect(screen.getAllByRole("region", { name: "Notifications (F8)" })).toHaveLength(1);
    expect(screen.getAllByRole("status").filter((element) => element.textContent?.includes("启动测试通知"))).toHaveLength(1);
  });
}

describe("bootstrapAuth → React 启动状态", () => {
  it("已有 Session 从 checking 进入真实 AppShell，只有一个 Toast Host", async () => {
    let resolve!: (session: AdminSession) => void;
    vi.mocked(fetchSession).mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    await mountApp();
    let boot!: Promise<void>;
    act(() => { boot = bootstrapAuth(); });
    expect(screen.getByText("正在恢复管理员会话…")).toBeInTheDocument();
    await act(async () => {
      resolve({ username: "test-admin", capabilities: [], csrfToken: "test-csrf", expiresAt: 1 });
      await boot;
    });
    expect(store.get(authPhaseAtom)).toBe("authenticated");
    expect(await screen.findByRole("navigation", { name: "控制台页面" })).toBeInTheDocument();
    expect(screen.getByText("test-admin")).toBeInTheDocument();
    expect(screen.queryByText("正在恢复管理员会话…")).not.toBeInTheDocument();
    expect(fetchBootstrap).not.toHaveBeenCalled();
    await expectSingleToast();
  });

  it("checking 不挂载业务页面，恢复成功后在原 hash 地址挂载", async () => {
    const mounted = vi.fn();
    function ChildPage() {
      useEffect(() => { mounted(); }, []);
      return <div>受保护的配置页面</div>;
    }
    let resolve!: (session: AdminSession) => void;
    vi.mocked(fetchSession).mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const previousUrl = window.location.href;
    window.history.replaceState(null, "", "#/configuration");
    const history = createHashHistory();
    try {
      const child = createRoute({ getParentRoute: () => Route, path: "/configuration", component: ChildPage });
      const router = createRouter({ routeTree: Route.addChildren([child]), history });
      render(<AppProviders client={client}><RouterProvider router={router} /></AppProviders>);
      await screen.findByText("正在恢复管理员会话…");
      let boot!: Promise<void>;
      act(() => { boot = bootstrapAuth(); });
      expect(screen.queryByText("受保护的配置页面")).not.toBeInTheDocument();
      expect(mounted).not.toHaveBeenCalled();
      expect(window.location.hash).toBe("#/configuration");
      await act(async () => {
        resolve({ username: "test-admin", capabilities: [], csrfToken: "test-csrf", expiresAt: 1 });
        await boot;
      });
      expect(await screen.findByText("受保护的配置页面")).toBeInTheDocument();
      expect(mounted).toHaveBeenCalledTimes(1);
      expect(window.location.hash).toBe("#/configuration");
    } finally {
      cleanup();
      history.destroy();
      window.history.replaceState(null, "", previousUrl);
    }
  });

  it("401 建立 pre-auth 后显示真实 AuthGate，只有一个 Toast Host", async () => {
    vi.mocked(fetchSession).mockRejectedValueOnce(new ConsoleApiError("未登录", "unauthorized", 401));
    vi.mocked(fetchBootstrap).mockResolvedValueOnce({ initialized: true, setupRequired: false, passwordResetPending: false, tokenFile: "", expiresAt: null });
    vi.mocked(issuePreAuth).mockResolvedValueOnce("test-preauth");
    await mountApp();
    await act(async () => { await bootstrapAuth(); });
    expect(fetchBootstrap).toHaveBeenCalledTimes(1);
    expect(issuePreAuth).toHaveBeenCalledTimes(1);
    expect(store.get(authPhaseAtom)).toBe("unauthenticated");
    expect(await screen.findByLabelText("管理员密码")).toBeInTheDocument();
    expect(screen.queryByText("页面内容")).not.toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "控制台页面" })).not.toBeInTheDocument();
    expect(screen.queryByText("正在恢复管理员会话…")).not.toBeInTheDocument();
    await expectSingleToast();
  });
});
