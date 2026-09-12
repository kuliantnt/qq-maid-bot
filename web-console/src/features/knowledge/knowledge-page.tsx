import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import {
  ConsoleApiError,
  deleteKnowledgeFile,
  downloadKnowledgeFile,
  fetchKnowledgeCapabilities,
  listKnowledgeFiles,
  retryKnowledgeFile,
  uploadKnowledgeFile,
} from "../../api.js";
import { Button } from "../../components/ui/button.js";
import { ConfirmDialog } from "../../components/ui/dialog.js";
import { Frame, SectionHeader } from "../../components/ui/frame.js";
import { Input } from "../../components/ui/field.js";
import { QueryState } from "../../components/ui/query-state.js";
import { StatusBadge } from "../../components/ui/status-badge.js";
import type { KnowledgeFileItem, KnowledgeFileStatus } from "../../types.js";
import {
  formatBytes,
  formatDateTime,
  knowledgeStatusMeta,
  latestProcessingTime,
  shortContentType,
  validateKnowledgeFile,
} from "./knowledge-format.js";

const PAGE_SIZE = 20;
const POLL_INTERVAL_MS = 5_000;
const MAX_POLL_FAILURES = 3;

type StatusFilter = KnowledgeFileStatus | "all";

/** 知识库文件管理：查看、筛选、上传、下载、删除与失败重试。
 * 轮询语义沿用旧契约：仅页面可见且存在非终态行时每 5s 刷新，全部终态即停止，
 * 连续失败达到上限后停止并提示手动刷新；终态转换以提示行报告。 */
export function KnowledgePage() {
  const queryClient = useQueryClient();
  const [searchInput, setSearchInput] = useState("");
  const [statusInput, setStatusInput] = useState<StatusFilter>("all");
  const [filters, setFilters] = useState<{ search: string; status: StatusFilter }>({ search: "", status: "all" });
  const [statusMessage, setStatusMessage] = useState<{ error: boolean; text: string } | null>(null);
  const [deleting, setDeleting] = useState<KnowledgeFileItem | null>(null);
  const previousStatusesRef = useRef(new Map<string, KnowledgeFileStatus>());
  const pollStoppedRef = useRef(false);

  const capabilitiesQuery = useQuery({
    queryKey: ["knowledge-capabilities"],
    queryFn: fetchKnowledgeCapabilities,
    staleTime: Infinity,
  });

  const listQuery = useInfiniteQuery({
    queryKey: ["knowledge", filters],
    queryFn: ({ pageParam }) => listKnowledgeFiles({ page_size: PAGE_SIZE, search: filters.search, status: filters.status, sort: "updated_at", order: "desc", page: pageParam }),
    initialPageParam: 1,
    getNextPageParam: (last) => (last.page < last.total_pages ? last.page + 1 : undefined),
    // 有非终态行时轮询；连续失败达到上限即停止（沿用 KnowledgePollingController 语义）。
    refetchInterval: (query) => {
      const items = query.state.data?.pages.flatMap((page) => page.items) ?? [];
      const hasActive = items.some((item) => item.status === "pending" || item.status === "processing");
      if (!hasActive) return false;
      if (query.state.fetchFailureCount >= MAX_POLL_FAILURES) {
        pollStoppedRef.current = true;
        return false;
      }
      return POLL_INTERVAL_MS;
    },
    refetchIntervalInBackground: false,
  });

  const items = listQuery.data?.pages.flatMap((page) => page.items) ?? [];

  // 轮询结果的状态对比：pending/processing → ready/failed 时报告终态转换。
  useEffect(() => {
    if (listQuery.data === undefined) return;
    const previous = previousStatusesRef.current;
    for (const item of items) {
      const before = previous.get(item.file_id ?? item.filename);
      if ((before === "pending" || before === "processing") && item.status === "ready") {
        setStatusMessage({ error: false, text: "文件处理完成" });
      }
      if ((before === "pending" || before === "processing") && item.status === "failed") {
        setStatusMessage({ error: true, text: "文件处理失败" });
      }
    }
    previousStatusesRef.current = new Map(items.map((item) => [item.file_id ?? item.filename, item.status]));
  }, [listQuery.data, items]);

  useEffect(() => {
    if (pollStoppedRef.current) {
      setStatusMessage({ error: true, text: "状态刷新多次失败，请手动刷新" });
    }
  }, [pollStoppedRef.current]);

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ["knowledge"] });

  const uploadMutation = useMutation({
    mutationFn: async (file: File) => {
      const capabilities = capabilitiesQuery.data;
      if (capabilities) {
        const validation = validateKnowledgeFile(file, capabilities);
        if (!validation.ok) throw new Error(`上传已阻止：${validation.reason}`);
      }
      return uploadKnowledgeFile(file);
    },
    onSuccess: () => {
      setStatusMessage({ error: false, text: "文件已上传，正在等待处理" });
      invalidate();
    },
    onError: (cause) => {
      setStatusMessage({ error: true, text: cause instanceof Error ? cause.message : "文件上传失败" });
    },
  });

  const downloadMutation = useMutation({
    mutationFn: (item: KnowledgeFileItem) => downloadKnowledgeFile(item),
    onSuccess: ({ blob, filename }) => {
      const href = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = href;
      anchor.download = filename;
      anchor.style.display = "none";
      document.body.append(anchor);
      anchor.click();
      anchor.remove();
      window.setTimeout(() => URL.revokeObjectURL(href), 60_000);
    },
    onError: (cause) => setStatusMessage({ error: true, text: cause instanceof Error ? cause.message : "操作失败，请稍后重试" }),
  });

  const retryMutation = useMutation({
    mutationFn: (fileId: string) => retryKnowledgeFile(fileId),
    onSuccess: () => {
      setStatusMessage({ error: false, text: "已重新提交处理" });
      invalidate();
    },
    onError: (cause) => setStatusMessage({ error: true, text: cause instanceof Error ? cause.message : "操作失败，请稍后重试" }),
  });

  const deleteMutation = useMutation({
    mutationFn: (fileId: string) => deleteKnowledgeFile(fileId),
    onSuccess: () => {
      setDeleting(null);
      setStatusMessage({ error: false, text: "文件已删除" });
      invalidate();
    },
    onError: (cause) => {
      setDeleting(null);
      setStatusMessage({
        error: true,
        text: cause instanceof ConsoleApiError && cause.status === 409 ? "文件正在处理中，暂不能删除" : cause instanceof Error ? cause.message : "操作失败，请稍后重试",
      });
    },
  });

  const busy = uploadMutation.isPending || downloadMutation.isPending || retryMutation.isPending || deleteMutation.isPending;

  return (
    <Frame variant="panel" className="animate-page-in">
      <SectionHeader
        eyebrow="KNOWLEDGE / FILES"
        title="知识库文件"
        lede="查看、筛选和上传由后端处理的知识库文件；目录来源文件只读，托管文件可操作。"
        meta={<span className="border border-line bg-accent-soft px-2 py-1 font-mono text-[0.64rem] font-bold tracking-wide text-accent-strong">受控写入</span>}
      />

      <div className="mt-4 flex flex-wrap items-center gap-3">
        <Input
          type="search"
          placeholder="搜索文件名"
          value={searchInput}
          onChange={(event) => setSearchInput(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              setFilters({ search: searchInput.trim(), status: statusInput });
            }
          }}
          className="w-64 py-2 text-sm"
        />
        <select
          value={statusInput}
          onChange={(event) => setStatusInput(event.target.value as StatusFilter)}
          className="border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
        >
          <option value="all">全部状态</option>
          <option value="pending">等待处理</option>
          <option value="processing">处理中</option>
          <option value="ready">已完成</option>
          <option value="failed">处理失败</option>
        </select>
        <Button onClick={() => setFilters({ search: searchInput.trim(), status: statusInput })} className="px-3 py-2 text-xs">应用筛选</Button>
        <Button
          variant="secondary"
          onClick={() => {
            setSearchInput("");
            setStatusInput("all");
            setFilters({ search: "", status: "all" });
          }}
          className="px-3 py-2 text-xs"
        >
          重置
        </Button>
        <Button variant="secondary" onClick={() => void listQuery.refetch()} disabled={listQuery.isFetching} className="px-3 py-2 text-xs">刷新</Button>
        <label className="ml-auto">
          <input
            type="file"
            hidden
            accept={capabilitiesQuery.data?.supported_extensions.join(",")}
            onChange={(event) => {
              const file = event.target.files?.[0];
              event.target.value = "";
              if (file) uploadMutation.mutate(file);
            }}
          />
          <Button
            variant="secondary"
            disabled={busy || capabilitiesQuery.isLoading}
            onClick={(event) => {
              const input = event.currentTarget.parentElement?.querySelector<HTMLInputElement>("input[type=file]");
              input?.click();
            }}
            className="px-3 py-2 text-xs"
          >
            {uploadMutation.isPending ? "上传中…" : "上传文件"}
          </Button>
        </label>
      </div>

      <p aria-live="polite" role="status" className={`m-0 mt-3 text-sm ${statusMessage?.error ? "text-error" : "text-muted"}`}>
        {statusMessage?.text ?? (capabilitiesQuery.data ? `支持 ${capabilitiesQuery.data.supported_extensions.join(" / ")}，单文件上限 ${formatBytes(capabilitiesQuery.data.max_file_bytes)}` : "")}
      </p>

      <div className="mt-3 overflow-x-auto">
        <QueryState query={listQuery}>
          {items.length > 0 ? (
            <table className="w-full border-collapse text-sm">
              <thead>
                <tr>
                  {["文件名", "类型", "大小", "上传时间", "最近处理", "状态", "失败原因", "操作"].map((label) => (
                    <th key={label} scope="col" className="border-b border-line px-2 py-2 text-left font-mono text-[0.66rem] font-bold tracking-widest text-muted uppercase whitespace-nowrap">
                      {label}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {items.map((item) => {
                  const meta = knowledgeStatusMeta(item.status);
                  const managed = item.source === "managed" && item.file_id !== null;
                  return (
                    <tr key={item.file_id ?? item.filename} className="border-b border-line-inner last:border-b-0">
                      <td className="px-2 py-2.5 whitespace-nowrap">
                        <span className="text-ink">{item.filename}</span>
                        <span className={`ml-2 border px-1.5 py-0.5 font-mono text-[0.6rem] ${item.source === "managed" ? "border-line text-accent-strong" : "border-line text-muted"}`}>
                          {item.source === "managed" ? "托管" : "目录"}
                        </span>
                      </td>
                      <td className="px-2 py-2.5 font-mono text-xs whitespace-nowrap">{shortContentType(item.content_type)}</td>
                      <td className="px-2 py-2.5 font-mono text-xs whitespace-nowrap">{formatBytes(item.size)}</td>
                      <td className="px-2 py-2.5 font-mono text-xs whitespace-nowrap">{formatDateTime(item.uploaded_at)}</td>
                      <td className="px-2 py-2.5 font-mono text-xs whitespace-nowrap">{formatDateTime(latestProcessingTime(item))}</td>
                      <td className="px-2 py-2.5 whitespace-nowrap">
                        <StatusBadge tone={meta.tone} label={meta.label} />
                      </td>
                      <td className="max-w-52 truncate px-2 py-2.5 text-xs text-error" title={item.error_summary ?? undefined}>
                        {item.error_summary ?? "—"}
                      </td>
                      <td className="px-2 py-2.5 whitespace-nowrap">
                        {item.source === "directory" ? (
                          <span className="text-muted">—</span>
                        ) : (
                          <div className="flex gap-2">
                            {item.downloadable ? (
                              <Button variant="secondary" disabled={busy} onClick={() => downloadMutation.mutate(item)} className="px-2.5 py-1.5 text-xs">下载</Button>
                            ) : null}
                            {item.status === "failed" ? (
                              <Button variant="secondary" disabled={busy} onClick={() => item.file_id !== null && retryMutation.mutate(item.file_id)} className="px-2.5 py-1.5 text-xs">重新处理</Button>
                            ) : null}
                            {item.status !== "processing" ? (
                              <Button variant="danger" disabled={busy} onClick={() => setDeleting(item)} className="px-2.5 py-1.5 text-xs">删除</Button>
                            ) : null}
                          </div>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          ) : listQuery.data ? (
            <p className="m-0 py-6 text-center text-sm text-muted">暂无知识库文件</p>
          ) : null}
        </QueryState>
      </div>

      {listQuery.hasNextPage ? (
        <div className="mt-4 text-center">
          <Button variant="secondary" disabled={listQuery.isFetchingNextPage} onClick={() => void listQuery.fetchNextPage()} className="px-4 py-2 text-xs">
            {listQuery.isFetchingNextPage ? "加载中…" : "加载更多"}
          </Button>
        </div>
      ) : null}

      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => {
          if (!open) setDeleting(null);
        }}
        title={deleting ? `删除文件：${deleting.filename}` : "删除文件"}
        description="删除后，该文件及对应的知识库解析和索引数据将被移除，且无法继续被检索。此操作不可恢复。"
        confirmLabel="删除"
        danger
        busy={deleteMutation.isPending}
        onConfirm={() => {
          if (deleting?.file_id !== null && deleting?.file_id !== undefined) deleteMutation.mutate(deleting.file_id);
        }}
      />
    </Frame>
  );
}
