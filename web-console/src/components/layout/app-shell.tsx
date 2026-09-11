import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router";
import { useAtomValue, useSetAtom } from "jotai";
import { useCallback, useRef, useState } from "react";
import { requestRestart } from "../../api.js";
import { autoRefreshAtom, useConsoleStatusQuery } from "../../queries/console-status.js";
import { logout, sessionAtom } from "../../stores/auth.js";
import { showToast } from "../../stores/toast.js";
import { cn } from "../../lib/utils.js";
import { Button } from "../ui/button.js";
import { ConfirmDialog } from "../ui/dialog.js";
import { ConsoleIcon, type ConsoleIconName } from "../ui/icons.js";
import { ConsoleToastHost } from "../ui/toast.js";

/** 控制台信息架构：导航顺序与旧版 CONSOLE_PAGES 保持一致。 */
const CONSOLE_PAGES: ReadonlyArray<{ id: string; label: string; icon: ConsoleIconName; to: string }> = [
  { id: "overview", label: "总览", icon: "overview", to: "/" },
  { id: "platforms", label: "平台", icon: "platforms", to: "/platforms" },
  { id: "configuration", label: "配置", icon: "configuration", to: "/configuration" },
  { id: "storage", label: "存储", icon: "storage", to: "/storage" },
  { id: "memory", label: "Memory", icon: "memory", to: "/memory" },
  { id: "todo", label: "Todo", icon: "todo", to: "/todo" },
  { id: "knowledge", label: "知识库", icon: "knowledge", to: "/knowledge" },
  { id: "tools", label: "工具", icon: "tools", to: "/tools" },
];

const COVER_DURATION_MS = 784;
const WASHOUT_DURATION_MS = 896;

function wait(duration: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, duration));
}

/** 已认证的应用壳：全宽状态栏 + 滚动内容区 + 悬浮底部导航。
 * 页面切换沿用旧版中心扩散过渡：784ms 遮蔽后换页，再 896ms 清洗淡出；
 * prefers-reduced-motion 下直接切换。转换中到达的请求只保留最新一个。 */
export function AppShell() {
  const [restartOpen, setRestartOpen] = useState(false);

  return (
    <>
      <header className="relative z-40 flex flex-wrap items-center justify-between gap-3 border-b border-line bg-glass px-4 py-2.5">
        <div className="min-w-0">
          <p className="console-mono-tag m-0">QQ MAID BOT · LOCAL OPS</p>
          <p className="m-0 text-xs leading-relaxed text-muted">
            默认同源并受部署管理员会话与 CSRF 保护；公网访问仍必须使用受信 TLS 反向代理。
          </p>
        </div>
        <StatusBarActions onRequestRestart={() => setRestartOpen(true)} />
      </header>

      <main className="relative z-1 mx-auto w-full max-w-6xl px-4 pt-4 pb-32">
        <Outlet />
      </main>

      <nav
        aria-label="控制台页面"
        className="fixed bottom-4 left-1/2 z-50 w-[min(410px,calc(100vw-2rem))] -translate-x-1/2 border border-line bg-glass-raised p-1.5 shadow-console"
      >
        <NavList />
      </nav>

      <RestartConfirmDialog open={restartOpen} onOpenChange={setRestartOpen} />
      <PageTransition />
      <ConsoleToastHost />
    </>
  );
}

function NavList() {
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const { navigateTo } = usePageTransition();
  return (
    <ul className="m-0 flex list-none justify-between gap-1 p-0">
      {CONSOLE_PAGES.map((page) => {
        const active = page.to === "/" ? pathname === "/" : pathname.startsWith(page.to);
        return (
          <li key={page.id} className="flex-1">
            <Link
              to={page.to}
              aria-current={active ? "page" : undefined}
              aria-label={page.label}
              onClick={(event) => {
                event.preventDefault();
                if (active) return;
                void navigateTo(page.to);
              }}
              className={cn(
                "flex flex-col items-center gap-0.5 px-1 py-2 text-[0.68rem] font-bold no-underline transition-colors",
                active ? "bg-accent-soft text-accent" : "text-muted hover:bg-accent-soft hover:text-ink",
              )}
            >
              <ConsoleIcon name={page.icon} className="h-5 w-5" />
              <span>{page.label}</span>
            </Link>
          </li>
        );
      })}
    </ul>
  );
}

/** 状态栏右侧操作区：重启、自动刷新、管理员标识、退出登录、最后刷新时间。 */
function StatusBarActions({ onRequestRestart }: { onRequestRestart: () => void }) {
  const session = useAtomValue(sessionAtom);
  const setAutoRefresh = useSetAtom(autoRefreshAtom);
  const statusQuery = useConsoleStatusQuery();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const logoutMutation = useMutation({
    mutationFn: logout,
    onSuccess: () => {
      // 退出登录后清理运行状态缓存，避免下一会话复用旧数据。
      queryClient.clear();
      void navigate({ to: "/" });
    },
    onError: (cause) => {
      showToast("error", cause instanceof Error ? cause.message : "退出请求失败，本地会话已清理。");
    },
  });

  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-xs text-muted">
      <Button variant="danger" onClick={onRequestRestart} className="px-3 py-1.5 text-xs">
        重启服务
      </Button>
      <label className="flex items-center gap-1.5 font-semibold">
        <input
          type="checkbox"
          defaultChecked
          onChange={(event) => setAutoRefresh(event.target.checked)}
          className="size-3.5 accent-[var(--console-accent)]"
        />
        自动刷新
      </label>
      <span>
        管理员：<strong className="font-mono text-ink">{session?.username ?? "—"}</strong>
      </span>
      <Button
        variant="secondary"
        disabled={logoutMutation.isPending}
        onClick={() => logoutMutation.mutate()}
        className="px-3 py-1.5 text-xs"
      >
        退出登录
      </Button>
      <span className="font-mono">
        最后刷新：
        {statusQuery.dataUpdatedAt ? new Date(statusQuery.dataUpdatedAt).toLocaleString() : "尚未刷新"}
      </span>
    </div>
  );
}

/** 受控重启：确认对话框 + 真实请求结果以 toast 展示。 */
function RestartConfirmDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const restartMutation = useMutation({
    mutationFn: requestRestart,
    onSuccess: (message) => {
      onOpenChange(false);
      showToast("info", message);
    },
    onError: (cause) => {
      showToast("error", cause instanceof Error ? cause.message : "重启请求失败");
    },
  });
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title="重启服务"
      description="受控重启会结束当前进程并由守护脚本重新拉起；重启会使内存中的管理员 session 失效，需要重新登录。"
      confirmLabel="确认重启"
      danger
      busy={restartMutation.isPending}
      onConfirm={() => restartMutation.mutate()}
    />
  );
}

/** 页面切换过渡：只在点击导航时播放；背景图接入 background controller 后填充切片。 */
function usePageTransition() {
  const navigate = useNavigate();
  const [running, setRunning] = useState(false);
  const busyRef = useRef(false);
  const pendingRef = useRef<string | null>(null);

  const navigateTo = useCallback(
    async (to: string) => {
      if (busyRef.current) {
        pendingRef.current = to;
        return;
      }
      busyRef.current = true;
      const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      if (!reducedMotion) {
        setRunning(true);
        await wait(COVER_DURATION_MS);
      }
      await navigate({ to });
      if (!reducedMotion) {
        await wait(WASHOUT_DURATION_MS);
        setRunning(false);
      }
      busyRef.current = false;
      const next = pendingRef.current;
      if (next !== null) {
        pendingRef.current = null;
        void navigateTo(next);
      }
    },
    [navigate],
  );

  const transitionOverlay = (
    <div className={cn("console-transition", running && "is-running")} aria-hidden="true" hidden={!running}>
      <div className="console-transition-wash" />
      <div className="console-transition-image" style={{ backgroundImage: "none" }} />
    </div>
  );

  return { navigateTo, transitionOverlay };
}

function PageTransition() {
  const { transitionOverlay } = usePageTransition();
  return transitionOverlay;
}
