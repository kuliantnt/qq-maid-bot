import { Outlet, createRootRoute } from "@tanstack/react-router";
import { useAtomValue } from "jotai";
import { AppShell } from "../components/layout/app-shell.js";
import { ConsoleBackground } from "../components/layout/console-background.js";
import { AuthGate } from "../features/auth/auth-gate.js";
import { authPhaseAtom } from "../stores/auth.js";
import { ConsoleToastHost } from "../components/ui/toast.js";

export const Route = createRootRoute({
  component: RootLayout,
});

function RootLayout() {
  const phase = useAtomValue(authPhaseAtom);
  return (
    <>
      <ConsoleBackground />
      <ConsoleToastHost />
      {phase === "checking" ? <AuthChecking /> : phase === "unauthenticated" ? <AuthGate /> : <AppShell />}
    </>
  );
}

function AuthChecking() {
  return (
    <div className="relative z-1 grid min-h-dvh place-items-center bg-canvas p-4">
      <p className="console-mono-tag" role="status">
        正在恢复管理员会话…
      </p>
      <Outlet />
    </div>
  );
}
