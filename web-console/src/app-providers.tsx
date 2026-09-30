import { QueryClientProvider, type QueryClient } from "@tanstack/react-query";
import { getDefaultStore, Provider as JotaiProvider } from "jotai";
import type { ReactNode } from "react";

/** 命令式认证、主题和 Toast 动作写入默认 store，React 必须订阅同一实例。
 * 裸 Provider 会创建私有 store，导致启动 UI 卡在“正在恢复管理员会话”。 */
export function AppProviders({ client, children }: { client: QueryClient; children: ReactNode }) {
  return (
    <JotaiProvider store={getDefaultStore()}>
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    </JotaiProvider>
  );
}
