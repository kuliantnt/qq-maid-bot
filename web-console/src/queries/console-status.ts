import { useQuery } from "@tanstack/react-query";
import { atom, useAtomValue } from "jotai";
import { fetchConsoleStatus } from "../api.js";
import { authPhaseAtom } from "../stores/auth.js";

export const CONSOLE_STATUS_QUERY_KEY = ["console-status"] as const;

/** 自动刷新开关：沿用旧行为默认开启，30s 间隔且仅在页面可见时刷新。 */
export const autoRefreshAtom = atom(true);

/**
 * 控制台运行状态查询：总览、平台、存储三个页面共享同一缓存。
 * 仅在已认证时启用；未认证时静默停用避免无谓请求。
 */
export function useConsoleStatusQuery() {
  const authenticated = useAtomValue(authPhaseAtom) === "authenticated";
  const autoRefresh = useAtomValue(autoRefreshAtom);
  return useQuery({
    queryKey: CONSOLE_STATUS_QUERY_KEY,
    queryFn: fetchConsoleStatus,
    enabled: authenticated,
    refetchInterval: autoRefresh ? 30_000 : false,
    refetchIntervalInBackground: false,
  });
}
