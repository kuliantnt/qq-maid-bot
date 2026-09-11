import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { deleteTodo, listTodoTargets, listTodos, updateTodo } from "../../api.js";
import { Button } from "../../components/ui/button.js";
import { DataTable } from "../../components/ui/data-table.js";
import { ConfirmDialog } from "../../components/ui/dialog.js";
import { Frame, SectionHeader } from "../../components/ui/frame.js";
import { Input } from "../../components/ui/field.js";
import { QueryState } from "../../components/ui/query-state.js";
import { showToast } from "../../stores/toast.js";
import type { TodoStatus } from "../../types.js";
import { TodoCard } from "./todo-card.js";
import { TodoCreateDialog } from "./todo-create-dialog.js";
import { TodoEditDialog } from "./todo-edit-dialog.js";

const TARGET_PAGE_SIZE = 100;

/** Todo 列表过滤条件；空串与 "all" 表示不参与查询参数。 */
type TodoFilters = {
  status: "all" | TodoStatus;
  keyword: string;
  timeFilter: string;
  recurring: string;
  targetRef: string;
  platform: string;
  accountId: string;
  userId: string;
  scopeType: string;
  dateStart: string;
  dateEnd: string;
};

const DEFAULT_FILTERS: TodoFilters = {
  status: "all",
  keyword: "",
  timeFilter: "all",
  recurring: "all",
  targetRef: "",
  platform: "",
  accountId: "",
  userId: "",
  scopeType: "",
  dateStart: "",
  dateEnd: "",
};

function buildListParams(filters: TodoFilters, page: number): Record<string, unknown> {
  return {
    page,
    ...(filters.status === "all" ? {} : { status: filters.status }),
    ...(filters.keyword.trim() ? { keyword: filters.keyword.trim() } : {}),
    ...(filters.timeFilter === "all" ? {} : { time_filter: filters.timeFilter }),
    ...(filters.recurring === "all" ? {} : { recurring: filters.recurring === "true" }),
    ...(filters.targetRef ? { target_ref: filters.targetRef } : {}),
    ...(filters.platform.trim() ? { platform: filters.platform.trim() } : {}),
    ...(filters.accountId.trim() ? { account_id: filters.accountId.trim() } : {}),
    ...(filters.userId.trim() ? { user_id: filters.userId.trim() } : {}),
    ...(filters.scopeType ? { scope_type: filters.scopeType } : {}),
    ...(filters.dateStart && filters.dateEnd ? { date_start: filters.dateStart, date_end: filters.dateEnd } : {}),
  };
}

/** Todo 页：任务清单、过滤、分页与受控写入。 */
export function TodoPage() {
  const queryClient = useQueryClient();
  const [filters, setFilters] = useState<TodoFilters>(DEFAULT_FILTERS);
  const [applied, setApplied] = useState<TodoFilters>(DEFAULT_FILTERS);
  const [page, setPage] = useState(1);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<{ id: string; title: string } | null>(null);
  const [statusMessage, setStatusMessage] = useState<{ error: boolean; text: string } | null>(null);

  const listQuery = useQuery({
    queryKey: ["todos", page, applied],
    queryFn: () => listTodos(buildListParams(applied, page)),
  });

  // 删除/翻页后当前页越界时回退到最后一页（沿用旧 pageAfterDelete 语义）。
  useEffect(() => {
    const result = listQuery.data;
    if (result && page > result.totalPages && page > 1) {
      setPage(Math.max(1, result.totalPages));
    }
  }, [listQuery.data, page]);

  const targetsQuery = useInfiniteQuery({
    queryKey: ["todo-targets"],
    queryFn: ({ pageParam }) => listTodoTargets(pageParam, TARGET_PAGE_SIZE),
    initialPageParam: 1,
    getNextPageParam: (last) => (last.page < last.totalPages ? last.page + 1 : undefined),
    staleTime: 60_000,
  });
  const targets = targetsQuery.data?.pages.flatMap((result) => result.items) ?? [];

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ["todos"] });

  const statusMutation = useMutation({
    mutationFn: ({ id, status }: { id: string; status: TodoStatus }) => updateTodo(id, { status }),
    onSuccess: () => {
      invalidate();
      setStatusMessage(null);
    },
    onError: (cause) => setStatusMessage({ error: true, text: cause instanceof Error ? cause.message : "Todo 更新失败" }),
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => deleteTodo(id),
    onSuccess: () => {
      invalidate();
      setDeleting(null);
      showToast("success", "Todo 已删除");
    },
    onError: (cause) => {
      setDeleting(null);
      setStatusMessage({ error: true, text: cause instanceof Error ? cause.message : "Todo 删除失败" });
    },
  });

  const advancedActive = [
    filters.timeFilter, filters.recurring, filters.targetRef, filters.platform,
    filters.accountId, filters.userId, filters.scopeType, filters.dateStart, filters.dateEnd,
  ].some((value) => value !== "" && value !== "all");

  const applyFilters = () => {
    setPage(1);
    setApplied(filters);
  };

  return (
    <Frame variant="panel" className="animate-page-in">
      <SectionHeader
        eyebrow="TODO / TASK CONTROL"
        title="Todo 管理"
        lede="查看、创建和更新当前运行目录中的 Todo，目标与提醒状态来自管理 API。"
        meta={<span className="border border-line bg-accent-soft px-2 py-1 font-mono text-[0.64rem] font-bold tracking-wide text-accent-strong">受控写入</span>}
      />

      <section aria-labelledby="todo-board-heading" className="mt-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <h2 id="todo-board-heading" className="m-0 text-base font-bold">任务清单</h2>
          <div className="flex gap-2">
            <Button variant="secondary" onClick={() => void listQuery.refetch()} disabled={listQuery.isFetching}>刷新</Button>
            <Button onClick={() => setCreateOpen(true)}>创建 Todo</Button>
          </div>
        </div>

        <div className="mt-3 flex flex-wrap items-end gap-3">
          <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
            状态
            <select
              value={filters.status}
              onChange={(event) => setFilters({ ...filters, status: event.target.value as TodoFilters["status"] })}
              className="border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
            >
              <option value="all">全部</option>
              <option value="pending">待处理</option>
              <option value="completed">已完成</option>
            </select>
          </label>
          <label className="flex flex-1 flex-col gap-1 text-xs font-semibold text-muted sm:max-w-64">
            关键词
            <Input type="search" placeholder="搜索标题或内容" value={filters.keyword} onChange={(event) => setFilters({ ...filters, keyword: event.target.value })} className="py-2 text-sm" />
          </label>
          <Button variant="secondary" onClick={() => setAdvancedOpen((open) => !open)} aria-expanded={advancedOpen} className="px-3 py-2 text-xs">
            {advancedOpen ? "收起高级筛选" : advancedActive ? "高级筛选 · 已启用" : "高级筛选"}
          </Button>
          <Button onClick={applyFilters} className="px-3 py-2 text-xs">应用筛选</Button>
          <Button
            variant="secondary"
            onClick={() => {
              setFilters(DEFAULT_FILTERS);
              setApplied(DEFAULT_FILTERS);
              setPage(1);
            }}
            className="px-3 py-2 text-xs"
          >
            重置
          </Button>
        </div>

        {advancedOpen ? (
          <div className="mt-3 grid grid-cols-2 gap-3 border border-line bg-glass-muted p-3 md:grid-cols-3 lg:grid-cols-5">
            <FilterSelect label="时间" value={filters.timeFilter} onChange={(value) => setFilters({ ...filters, timeFilter: value })}
              options={[["all", "全部"], ["overdue", "已逾期"], ["no_due_date", "无截止日期"]]} />
            <FilterSelect label="重复" value={filters.recurring} onChange={(value) => setFilters({ ...filters, recurring: value })}
              options={[["all", "全部"], ["true", "仅重复"], ["false", "非重复"]]} />
            <FilterSelect label="目标" value={filters.targetRef} onChange={(value) => setFilters({ ...filters, targetRef: value })}
              options={[["", "全部目标"], ...targets.map((target) => [target.targetRef, `${target.platform} · ${target.scopeType}`] as const)]} />
            <FilterText label="平台" placeholder="qq_official" value={filters.platform} onChange={(value) => setFilters({ ...filters, platform: value })} />
            <FilterText label="账号" placeholder="可选" value={filters.accountId} onChange={(value) => setFilters({ ...filters, accountId: value })} />
            <FilterText label="用户/群组" placeholder="可选" value={filters.userId} onChange={(value) => setFilters({ ...filters, userId: value })} />
            <FilterSelect label="范围" value={filters.scopeType} onChange={(value) => setFilters({ ...filters, scopeType: value })}
              options={[["", "全部"], ["private", "私聊"], ["group", "群聊"]]} />
            <FilterDate label="日期起" value={filters.dateStart} onChange={(value) => setFilters({ ...filters, dateStart: value })} />
            <FilterDate label="日期止" value={filters.dateEnd} onChange={(value) => setFilters({ ...filters, dateEnd: value })} />
          </div>
        ) : null}

        <p aria-live="polite" role="status" className={`m-0 mt-3 text-sm ${statusMessage?.error ? "text-error" : "text-muted"}`}>
          {statusMessage?.text ?? (listQuery.data ? `${listQuery.data.total} 项 Todo` : "")}
        </p>

        <div aria-live="polite" className="mt-3 flex flex-col gap-3">
          <QueryState query={listQuery}>
            {listQuery.data && listQuery.data.items.length > 0 ? (
              listQuery.data.items.map((todo) => (
                <TodoCard
                  key={todo.id}
                  todo={todo}
                  busy={statusMutation.isPending || deleteMutation.isPending}
                  onChangeStatus={(item, status) => statusMutation.mutate({ id: item.id, status })}
                  onEdit={(item) => setEditingId(item.id)}
                  onDelete={(item) => setDeleting({ id: item.id, title: item.title })}
                />
              ))
            ) : listQuery.data ? (
              <p className="m-0 py-6 text-center text-sm text-muted">当前筛选没有 Todo。</p>
            ) : null}
          </QueryState>
        </div>

        {listQuery.data && listQuery.data.totalPages > 1 ? (
          <div className="mt-4 flex items-center justify-center gap-4 text-sm text-muted">
            <Button variant="secondary" disabled={page <= 1} onClick={() => setPage((current) => Math.max(1, current - 1))} className="px-3 py-1.5 text-xs">上一页</Button>
            <span className="font-mono">{listQuery.data.page} / {listQuery.data.totalPages}</span>
            <Button variant="secondary" disabled={page >= listQuery.data.totalPages} onClick={() => setPage((current) => current + 1)} className="px-3 py-1.5 text-xs">下一页</Button>
          </div>
        ) : null}
      </section>

      <TodoCreateDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        targets={targets}
        targetsLoading={targetsQuery.isLoading}
        hasMoreTargets={targetsQuery.hasNextPage ?? false}
        onLoadMoreTargets={() => void targetsQuery.fetchNextPage()}
      />
      <TodoEditDialog todoId={editingId} onClose={() => setEditingId(null)} />
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => {
          if (!open) setDeleting(null);
        }}
        title="删除 Todo"
        description={deleting ? `确定删除 Todo「${deleting.title}」吗？` : ""}
        confirmLabel="删除"
        danger
        busy={deleteMutation.isPending}
        onConfirm={() => {
          if (deleting) deleteMutation.mutate(deleting.id);
        }}
      />
    </Frame>
  );
}

function FilterSelect({ label, value, onChange, options }: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: ReadonlyArray<readonly [string, string]>;
}) {
  return (
    <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
      {label}
      <select
        value={value}
        onChange={(event) => onChange(event.target.value)}
        className="border border-line bg-input px-2 py-2 text-sm text-ink outline-none"
      >
        {options.map(([optionValue, optionLabel]) => (
          <option key={optionValue} value={optionValue}>{optionLabel}</option>
        ))}
      </select>
    </label>
  );
}

function FilterText({ label, value, onChange, placeholder }: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
}) {
  return (
    <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
      {label}
      <Input value={value} placeholder={placeholder} onChange={(event) => onChange(event.target.value)} className="py-2 text-sm" />
    </label>
  );
}

function FilterDate({ label, value, onChange }: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
      {label}
      <Input type="date" value={value} onChange={(event) => onChange(event.target.value)} className="py-2 text-sm" />
    </label>
  );
}
