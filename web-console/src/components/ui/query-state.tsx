import { useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { Button } from "./button.js";

type QueryStateProps = {
  query: UseQueryResult;
  children: React.ReactNode;
};

/** 查询三态包装：加载中 / 错误（含重试）/ 正常渲染子内容（DESIGN.md §8 异步状态要求）。 */
export function QueryState({ query, children }: QueryStateProps) {
  if (query.isPending) {
    return (
      <p className="console-mono-tag m-0 px-2 py-6" role="status">
        正在加载运行状态…
      </p>
    );
  }
  if (query.isError) {
    return (
      <div className="flex flex-col items-start gap-3 px-2 py-6">
        <p role="alert" className="m-0 text-sm font-semibold text-error">
          {query.error instanceof Error ? query.error.message : "状态刷新失败"}
        </p>
        <Button variant="secondary" onClick={() => void query.refetch()}>
          重试
        </Button>
      </div>
    );
  }
  return <>{children}</>;
}

/** 登出或会话切换后手动失效运行状态缓存时使用。 */
export function useInvalidateConsoleStatus() {
  const queryClient = useQueryClient();
  return () => void queryClient.invalidateQueries({ queryKey: ["console-status"] });
}
