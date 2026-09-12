import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import {
  archiveMemory,
  commitMemoryOperation,
  listMemories,
  listMemoryTargets,
  prepareMemoryOperation,
  restoreMemory,
} from "../../memory-api.js";
import { Button } from "../../components/ui/button.js";
import { ConfirmDialog } from "../../components/ui/dialog.js";
import { Frame, SectionHeader } from "../../components/ui/frame.js";
import { Input } from "../../components/ui/field.js";
import { QueryState } from "../../components/ui/query-state.js";
import { showToast } from "../../stores/toast.js";
import type { MemoryCategory, MemoryItem, MemoryKind, MemoryListParams, MemoryTargetView, MemoryVisibility } from "../../types.js";
import { MemoryCreateForm } from "./memory-create-form.js";
import { MemoryEditDialog } from "./memory-edit-dialog.js";
import {
  CATEGORY_LABELS,
  CATEGORY_OPTIONS,
  KIND_LABELS,
  VISIBILITY_LABELS,
  canClearTarget,
  canDisableGroupProfile,
  memoryTargetLabel,
  uniqueRefs,
} from "./memory-labels.js";

const PAGE_SIZE = 20;
const TARGET_PAGE_SIZE = 100;

type Filters = {
  scope: MemoryKind | "all";
  status: "active" | "archived" | "all";
  category: MemoryCategory | "";
  visibility: MemoryVisibility | "all";
  pinned: "true" | "false" | "all";
  keyword: string;
  platform: string;
  accountRef: string;
  groupRef: string;
  subjectRef: string;
};

const DEFAULT_FILTERS: Filters = {
  scope: "all",
  status: "active",
  category: "",
  visibility: "all",
  pinned: "all",
  keyword: "",
  platform: "",
  accountRef: "",
  groupRef: "",
  subjectRef: "",
};

function buildParams(filters: Filters, page: number): MemoryListParams {
  return {
    page,
    pageSize: PAGE_SIZE,
    scope: filters.scope,
    status: filters.status,
    category: filters.category === "" ? "all" : filters.category,
    visibility: filters.visibility,
    pinned: filters.pinned,
    keyword: filters.keyword.trim(),
    platform: filters.platform.trim(),
    accountRef: filters.accountRef.trim(),
    groupRef: filters.groupRef.trim(),
    subjectRef: filters.subjectRef.trim(),
  };
}

/** Memory 页：授权范围 discovery、筛选列表、受控写入与两阶段确认的范围操作。 */
export function MemoryPage() {
  const queryClient = useQueryClient();
  const [filters, setFilters] = useState<Filters>(DEFAULT_FILTERS);
  const [applied, setApplied] = useState<Filters>(DEFAULT_FILTERS);
  const [page, setPage] = useState(1);
  const [statusMessage, setStatusMessage] = useState<{ error: boolean; text: string } | null>(null);
  const [editing, setEditing] = useState<MemoryItem | null>(null);
  const [archiving, setArchiving] = useState<MemoryItem | null>(null);
  const [deleting, setDeleting] = useState<MemoryItem | null>(null);
  const [targetOp, setTargetOp] = useState<{ operation: "clear_target" | "disable_group_profile"; target: MemoryTargetView } | null>(null);

  const listQuery = useQuery({
    queryKey: ["memories", page, applied],
    queryFn: () => listMemories(buildParams(applied, page)),
  });

  // 归档/恢复可能让当前页失效；回到最后有效页，避免空列表把分页控件一起隐藏。
  useEffect(() => {
    const result = listQuery.data;
    if (result && page > Math.max(result.totalPages, 1) && page > 1) {
      setPage(Math.max(1, result.totalPages));
    }
  }, [listQuery.data, page]);

  // 目标列表是独立的渐进式资源：列表先可用，后续 target 页失败只影响目标控件。
  const targetsQuery = useInfiniteQuery({
    queryKey: ["memory-targets"],
    queryFn: ({ pageParam }) => listMemoryTargets(pageParam, TARGET_PAGE_SIZE),
    initialPageParam: 1,
    getNextPageParam: (last) => (last.page < last.totalPages ? last.page + 1 : undefined),
    staleTime: 30_000,
  });
  const targets = targetsQuery.data?.pages.flatMap((result) => result.items) ?? [];

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ["memories"] });
  const refreshTargets = () => void queryClient.invalidateQueries({ queryKey: ["memory-targets"] });

  const twoPhase = useMutation({
    // 两阶段确认：prepare 拿一次性 confirmationToken，用户确认后 commit。
    mutationFn: async (input: {
      operation: "clear_target" | "disable_group_profile" | "delete_memory";
      targetRef: string;
      memoryRef?: string;
      expectedVersion?: number;
    }) => {
      const confirmation = await prepareMemoryOperation(input);
      return commitMemoryOperation({
        operation: confirmation.operation,
        targetRef: input.targetRef,
        confirmationToken: confirmation.confirmationToken,
      });
    },
  });

  const archiveMutation = useMutation({
    mutationFn: (item: MemoryItem) => archiveMemory({ targetRef: item.target.targetRef, memoryRef: item.memoryRef, expectedVersion: item.version }),
    onSuccess: () => {
      setArchiving(null);
      showToast("success", "Memory 已由服务端确认归档");
      invalidate();
    },
    onError: (cause) => setStatusMessage({ error: true, text: cause instanceof Error ? cause.message : "Memory 归档失败" }),
  });

  const restoreMutation = useMutation({
    mutationFn: (item: MemoryItem) => restoreMemory({ targetRef: item.target.targetRef, memoryRef: item.memoryRef, expectedVersion: item.version }),
    onSuccess: () => {
      showToast("success", "Memory 已由服务端确认恢复");
      invalidate();
    },
    onError: (cause) => setStatusMessage({ error: true, text: cause instanceof Error ? cause.message : "Memory 恢复失败" }),
  });

  const busy = archiveMutation.isPending || restoreMutation.isPending || twoPhase.isPending;

  const applyFilters = () => {
    setPage(1);
    setApplied(filters);
  };

  const accountRefs = uniqueRefs(targets.map((target) => target.accountRef));
  const groupRefs = uniqueRefs(targets.flatMap((target) => (target.groupRef ? [target.groupRef] : [])));
  const subjectRefs = uniqueRefs(targets.flatMap((target) => (target.subjectRef ? [target.subjectRef] : [])));

  return (
    <Frame variant="panel" className="animate-page-in">
      <SectionHeader
        eyebrow="MEMORY / CONTROLLED"
        title="Memory 管理"
        lede="按授权范围查看、创建与维护长期记忆；清空范围与停止画像需要服务端确认。"
        meta={<span className="border border-line bg-accent-soft px-2 py-1 font-mono text-[0.64rem] font-bold tracking-wide text-accent-strong">受控写入</span>}
      />

      <div className="mt-4 flex flex-wrap items-end gap-3">
        <FilterSelect label="范围" value={filters.scope} onChange={(value) => setFilters({ ...filters, scope: value as Filters["scope"] })}
          options={[["all", "全部范围"], ["personal", "个人记忆"], ["group_profile", "群内用户画像"], ["group", "群组记忆"]]} />
        <FilterSelect label="状态" value={filters.status} onChange={(value) => setFilters({ ...filters, status: value as Filters["status"] })}
          options={[["active", "生效中"], ["archived", "已归档"], ["all", "全部"]]} />
        <FilterSelect label="分类" value={filters.category} onChange={(value) => setFilters({ ...filters, category: value as Filters["category"] })}
          options={CATEGORY_OPTIONS} />
        <FilterSelect label="可见性" value={filters.visibility} onChange={(value) => setFilters({ ...filters, visibility: value as Filters["visibility"] })}
          options={[["all", "全部"], ...Object.entries(VISIBILITY_LABELS)]} />
        <FilterSelect label="固定" value={filters.pinned} onChange={(value) => setFilters({ ...filters, pinned: value as Filters["pinned"] })}
          options={[["all", "全部"], ["true", "已固定"], ["false", "未固定"]]} />
        <FilterText label="关键词" value={filters.keyword} onChange={(value) => setFilters({ ...filters, keyword: value })} />
        <FilterText label="平台" value={filters.platform} onChange={(value) => setFilters({ ...filters, platform: value })} />
        <FilterSelect label="账号" value={filters.accountRef} onChange={(value) => setFilters({ ...filters, accountRef: value })}
          options={[["", "全部账号"], ...accountRefs.map((ref) => [ref, ref] as const)]} />
        <FilterSelect label="群组" value={filters.groupRef} onChange={(value) => setFilters({ ...filters, groupRef: value })}
          options={[["", "全部群组"], ...groupRefs.map((ref) => [ref, ref] as const)]} />
        <FilterSelect label="用户" value={filters.subjectRef} onChange={(value) => setFilters({ ...filters, subjectRef: value })}
          options={[["", "全部用户"], ...subjectRefs.map((ref) => [ref, ref] as const)]} />
        <Button onClick={applyFilters} className="px-3 py-2 text-xs">应用筛选</Button>
        <Button variant="secondary" onClick={() => { setFilters(DEFAULT_FILTERS); setApplied(DEFAULT_FILTERS); setPage(1); }} className="px-3 py-2 text-xs">重置</Button>
        <Button variant="secondary" onClick={() => { refreshTargets(); void listQuery.refetch(); }} disabled={listQuery.isFetching} className="px-3 py-2 text-xs">刷新</Button>
      </div>

      <p aria-live="polite" role="status" className={`m-0 mt-3 text-sm ${statusMessage?.error ? "text-error" : "text-muted"}`}>
        {statusMessage?.text ?? (listQuery.data ? `${listQuery.data.total} 条记忆` : "")}
      </p>

      <div aria-live="polite" className="mt-3 flex flex-col gap-3">
        <QueryState query={listQuery}>
          {listQuery.data && listQuery.data.items.length > 0 ? (
            listQuery.data.items.map((item) => (
              <MemoryCard
                key={`${item.target.targetRef}/${item.memoryRef}`}
                item={item}
                busy={busy}
                onEdit={setEditing}
                onArchive={setArchiving}
                onRestore={(target) => restoreMutation.mutate(target)}
                onDelete={setDeleting}
              />
            ))
          ) : listQuery.data ? (
            <p className="m-0 py-6 text-center text-sm text-muted">当前筛选没有可展示的记忆。</p>
          ) : null}
        </QueryState>
      </div>

      {listQuery.data && listQuery.data.totalPages > 1 ? (
        <div className="mt-4 flex items-center justify-center gap-4 text-sm text-muted">
          <Button variant="secondary" disabled={page <= 1} onClick={() => setPage((current) => Math.max(1, current - 1))} className="px-3 py-1.5 text-xs">上一页</Button>
          <span className="font-mono">第 {listQuery.data.page} / {listQuery.data.totalPages} 页 · {listQuery.data.total} 条</span>
          <Button variant="secondary" disabled={page >= listQuery.data.totalPages} onClick={() => setPage((current) => current + 1)} className="px-3 py-1.5 text-xs">下一页</Button>
        </div>
      ) : null}

      <section aria-label="授权范围操作" className="mt-6 border border-line bg-glass-muted p-4">
        <p className="console-mono-tag m-0 mb-2">SCOPES / RANGE OPERATIONS</p>
        {targets.length === 0 ? (
          <p className="m-0 text-sm text-muted">
            {targetsQuery.isLoading ? "正在加载授权范围…" : targetsQuery.isError ? "授权范围加载失败，请重试。" : "暂无可用授权范围。"}
          </p>
        ) : (
          <div className="flex flex-col gap-2">
            {targets.map((target) => (
              <div key={target.targetRef} className="flex flex-wrap items-center justify-between gap-2 text-sm">
                <span className="font-mono text-xs text-ink">{memoryTargetLabel(target)}</span>
                <div className="flex gap-2">
                  {canClearTarget(target) ? (
                    <Button variant="danger" disabled={busy} onClick={() => setTargetOp({ operation: "clear_target", target })} className="px-2.5 py-1.5 text-xs">清空此范围</Button>
                  ) : null}
                  {canDisableGroupProfile(target) ? (
                    <Button variant="danger" disabled={busy} onClick={() => setTargetOp({ operation: "disable_group_profile", target })} className="px-2.5 py-1.5 text-xs">停止画像</Button>
                  ) : null}
                </div>
              </div>
            ))}
            {targetsQuery.hasNextPage ? (
              <Button variant="secondary" disabled={targetsQuery.isFetchingNextPage} onClick={() => void targetsQuery.fetchNextPage()} className="self-start px-3 py-1.5 text-xs">
                {targetsQuery.isFetchingNextPage ? "正在加载更多范围…" : `加载更多范围（已加载 ${targets.length}）`}
              </Button>
            ) : null}
          </div>
        )}
      </section>

      <MemoryCreateForm targets={targets} targetsLoading={targetsQuery.isLoading} disabled={listQuery.isPending || listQuery.isError} />

      <MemoryEditDialog item={editing} onClose={() => setEditing(null)} />

      <ConfirmDialog
        open={archiving !== null}
        onOpenChange={(open) => { if (!open) setArchiving(null); }}
        title="归档 Memory"
        description="确定归档这条 Memory 吗？归档后仍可恢复。"
        confirmLabel="归档"
        busy={archiveMutation.isPending}
        onConfirm={() => { if (archiving) archiveMutation.mutate(archiving); }}
      />

      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => { if (!open) setDeleting(null); }}
        title="永久删除 Memory"
        description="确定永久删除这条 Memory 吗？删除后无法恢复。确认提交后服务端完成校验并执行删除。"
        confirmLabel="永久删除"
        danger
        busy={twoPhase.isPending}
        onConfirm={() => {
          if (!deleting) return;
          twoPhase.mutate(
            { operation: "delete_memory", targetRef: deleting.target.targetRef, memoryRef: deleting.memoryRef, expectedVersion: deleting.version },
            {
              onSuccess: (result) => {
                if (result.deleted !== true || result.memoryRef !== deleting.memoryRef) {
                  setStatusMessage({ error: true, text: "Memory 删除结果无法与原记录确认" });
                  return;
                }
                setDeleting(null);
                setStatusMessage({ error: false, text: "Memory 已由服务端确认删除" });
                // 删除最后一条记录后 target 可能从服务端 discovery 消失，及时刷新范围。
                refreshTargets();
                invalidate();
              },
              onError: (cause) => setStatusMessage({ error: true, text: cause instanceof Error ? cause.message : "Memory 删除失败" }),
            },
          );
        }}
      />

      <ConfirmDialog
        open={targetOp !== null}
        onOpenChange={(open) => { if (!open) setTargetOp(null); }}
        title={targetOp?.operation === "disable_group_profile" ? "停止群画像" : "清空范围"}
        description={targetOp
          ? `此操作需要服务端确认；确认后将${targetOp.operation === "disable_group_profile" ? "停止画像并归档" : "清空"}该范围内的 Memory。`
          : ""}
        confirmLabel="确认执行"
        danger
        busy={twoPhase.isPending}
        onConfirm={() => {
          if (!targetOp) return;
          const { operation, target } = targetOp;
          twoPhase.mutate(
            { operation, targetRef: target.targetRef },
            {
              onSuccess: (result) => {
                const noun = operation === "disable_group_profile" ? "停止画像并归档" : "清空";
                setTargetOp(null);
                setStatusMessage({ error: false, text: `服务端已完成${noun}：${result.affectedCount} 条` });
                refreshTargets();
                invalidate();
              },
              onError: (cause) => {
                setTargetOp(null);
                setStatusMessage({ error: true, text: cause instanceof Error ? cause.message : "Memory 操作失败" });
              },
            },
          );
        }}
      />
    </Frame>
  );
}

function MemoryCard({ item, busy, onEdit, onArchive, onRestore, onDelete }: {
  item: MemoryItem;
  busy: boolean;
  onEdit: (item: MemoryItem) => void;
  onArchive: (item: MemoryItem) => void;
  onRestore: (item: MemoryItem) => void;
  onDelete: (item: MemoryItem) => void;
}) {
  const archived = item.status === "archived";
  return (
    <article className={`console-frame flex flex-col gap-2 p-4 ${archived ? "opacity-70" : ""}`}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="m-0 text-base font-bold text-ink">
          {KIND_LABELS[item.kind]} · {CATEGORY_LABELS[item.category]}
        </h3>
        <span className={`font-mono text-[0.66rem] font-bold tracking-widest ${archived ? "text-muted" : "text-success"}`}>
          {archived ? "ARCHIVED" : "ACTIVE"}
        </span>
      </div>
      <p className="m-0 font-mono text-xs text-muted">{memoryTargetLabel(item.target)}</p>
      <p className="m-0 text-sm leading-relaxed text-ink">{item.content}</p>
      <p className="m-0 font-mono text-xs text-muted">
        {item.pinned ? "已固定 · " : ""}版本 v{item.version} · {VISIBILITY_LABELS[item.visibility]} · {item.createdAt}
        {item.updatedAt ? ` · 更新于 ${item.updatedAt}` : ""}
      </p>
      <div className="flex flex-wrap gap-2">
        {item.status === "active" ? (
          <>
            {item.capabilities.canUpdate ? (
              <Button variant="secondary" disabled={busy} onClick={() => onEdit(item)} className="px-3 py-1.5 text-xs">纠正内容</Button>
            ) : null}
            {item.capabilities.canArchive ? (
              <Button variant="secondary" disabled={busy} onClick={() => onArchive(item)} className="px-3 py-1.5 text-xs">归档</Button>
            ) : null}
            {item.capabilities.canDelete ? (
              <Button variant="danger" disabled={busy} onClick={() => onDelete(item)} className="px-3 py-1.5 text-xs">永久删除</Button>
            ) : null}
          </>
        ) : item.capabilities.canRestore ? (
          <Button variant="secondary" disabled={busy} onClick={() => onRestore(item)} className="px-3 py-1.5 text-xs">恢复</Button>
        ) : null}
      </div>
    </article>
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
      <select value={value} onChange={(event) => onChange(event.target.value)} className="border border-line bg-input px-2 py-2 text-sm text-ink outline-none">
        {options.map(([optionValue, optionLabel]) => (
          <option key={optionValue} value={optionValue}>{optionLabel}</option>
        ))}
      </select>
    </label>
  );
}

function FilterText({ label, value, onChange }: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
      {label}
      <Input value={value} onChange={(event) => onChange(event.target.value)} className="py-2 text-sm" />
    </label>
  );
}
