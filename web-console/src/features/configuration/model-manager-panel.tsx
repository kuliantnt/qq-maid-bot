import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import {
  ConsoleApiError,
  discoverConnectionModels,
  fetchModelMetadata,
  testProviderConnection,
  updateAgentConfiguration,
  updateModelOverride,
} from "../../api.js";
import { Button } from "../../components/ui/button.js";
import { Input } from "../../components/ui/field.js";
import { StatusBadge } from "../../components/ui/status-badge.js";
import { Switch } from "../../components/ui/switch.js";
import type { ConfigurationSnapshot } from "../../types.js";
import {
  CAPABILITY_KEYS,
  CAPABILITY_LABELS,
  MODALITY_OPTIONS,
  buildModelOverride,
  draftFromOverride,
  emptyModelOverrideDraft,
  type ModelOverrideDraft,
} from "./model-override-form.js";
import { discoveryLabel, filterModels, mergeModels } from "./provider-model-data.js";
// 候选路线纯操作复用 Agent 编辑器的 model-route-editor，保持 set_model_route 值语义单一来源。
import { addCandidate, normalizeCandidates } from "./model-route-editor.js";

const MODEL_FILTERS = [
  ["all", "全部状态"],
  ["enabled", "本地启用"],
  ["disabled", "本地禁用"],
  ["active", "Active"],
  ["beta", "Beta"],
  ["deprecated", "Deprecated"],
] as const;

/** 行内展示的来源徽标；文案与 provider-model-data 的 sources 字符串一一对应。 */
const SOURCE_BADGES: Record<string, string> = {
  "connection discovery": "发现",
  catalog: "目录",
  "local override": "本地覆盖",
};

const CHECK_LABELS: Record<string, string> = { success: "成功", failed: "失败", unknown: "无法确认", not_tested: "未测试" };

/** 目录状态徽标色调：仅映射目录声明的 status 值，未知值回落 neutral。 */
const STATUS_TONES: Record<string, "success" | "warning" | "error" | "neutral"> = {
  active: "success",
  beta: "warning",
  deprecated: "neutral",
};

type ModelManagerPanelProps = {
  snapshot: ConfigurationSnapshot;
  /** 目标 Connection ID。 */
  connection: string;
  /** 停用 Connection 仍可管理本地元数据；同步模型与加入 Route 仅在启用时可用。 */
  enabled: boolean;
  /** 测试 / 发现使用的 revision：内置连接用 runtime revision，自定义连接用 agent revision。 */
  discoveryRevision: string;
  /** 密钥独立于连接配置更新，诊断必须同时绑定两者版本。 */
  credentialRevision: string;
};

/** Connection 模型管理面板（Cherry Studio 风格，内联在供应商详情栏）：
 * 发现（同步模型）、本地 metadata/目录合并展示、逐模型连通性检查、启停开关与加入 Route。
 * 模型与价格仅供参考：Advertised 为声明，Verified 能力未知，同步成功不等于真实调用成功。 */
export function ModelManagerPanel({ snapshot, connection, enabled, discoveryRevision, credentialRevision }: ModelManagerPanelProps) {
  const queryClient = useQueryClient();
  const metadataQuery = useQuery({
    queryKey: ["provider-model-metadata", connection],
    queryFn: () => fetchModelMetadata(connection),
    staleTime: 0,
  });
  const discoveryKey = JSON.stringify([connection, discoveryRevision, credentialRevision]);
  const [discoveryState, setDiscoveryState] = useState<{
    scope: { key: string; connection: string; revision: string };
    result: Record<string, unknown> | null;
  }>(() => ({ scope: { key: discoveryKey, connection, revision: discoveryRevision }, result: null }));
  // 仅重置发现状态，不重建面板或编辑器；scope 对象也区分切走后返回同一 revision 的请求。
  if (discoveryState.scope.key !== discoveryKey) {
    setDiscoveryState({ scope: { key: discoveryKey, connection, revision: discoveryRevision }, result: null });
  }
  const discovery = discoveryState.scope.key === discoveryKey ? discoveryState.result : null;
  const [search, setSearch] = useState("");
  const [statusFilter, setStatusFilter] = useState<string>("all");
  const [visibleLimit, setVisibleLimit] = useState(50);
  const [draft, setDraft] = useState<ModelOverrideDraft | null>(null);
  const [draftRevision, setDraftRevision] = useState("missing");
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");

  const metadata = metadataQuery.data ?? {};
  const rows = useMemo(() => mergeModels(discovery, metadata), [discovery, metadata]);
  const visibleRows = useMemo(() => filterModels(rows, search, statusFilter), [rows, search, statusFilter]);
  const enabledCount = useMemo(
    () => rows.filter((row) => row.metadata.enabled !== false).length,
    [rows],
  );

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ["configuration"] });
    void queryClient.invalidateQueries({ queryKey: ["provider-model-metadata", connection] });
  };

  const discoveryMutation = useMutation({
    mutationFn: (scope: typeof discoveryState.scope) => discoverConnectionModels(scope.connection, scope.revision),
    onSuccess: (result, scope) => {
      // 用请求发起时的 scope 校验当前状态，迟到的成功和失败都不能跨版本回写。
      setDiscoveryState((current) => current.scope === scope ? { ...current, result } : current);
    },
    onError: (cause, scope) => {
      const result = { state: "failed", category: cause instanceof Error ? cause.message : "获取失败" };
      setDiscoveryState((current) => current.scope === scope ? { ...current, result } : current);
    },
  });
  const discoveryPending = discoveryMutation.isPending && discoveryMutation.variables === discoveryState.scope;

  const overrideMutation = useMutation({
    mutationFn: ({ expectedRevision, model }: { expectedRevision: string; model: unknown }) =>
      updateModelOverride(connection, expectedRevision, model),
    onSuccess: () => {
      invalidate();
      setStatus("本地模型已保存；重启后生效。");
      setError("");
    },
    onError: (cause) => setError(overrideErrorMessage(cause)),
  });

  const routeMutation = useMutation({
    mutationFn: ({ revision, changes }: { revision: string; changes: unknown[] }) => updateAgentConfiguration(revision, changes),
    onSuccess: (_data, variables) => {
      invalidate();
      const name = typeof variables.changes[0] === "object" && variables.changes[0] !== null
        ? (variables.changes[0] as Record<string, unknown>).name
        : undefined;
      setStatus(`已加入 Route ${String(name ?? "")}；重启后生效。`);
      setError("");
    },
    onError: (cause) => setError(cause instanceof Error ? cause.message : "加入 Route 失败"),
  });

  const modelRoutes = useMemo(() => {
    const saved = snapshot.agent?.savedValue;
    const routes = typeof saved === "object" && saved !== null ? (saved as Record<string, unknown>).model_routes : undefined;
    return typeof routes === "object" && routes !== null && !Array.isArray(routes) ? Object.keys(routes) : [];
  }, [snapshot]);

  const editable = snapshot.agent?.editable === true;
  const metadataRevision = typeof metadata.revision === "string" ? metadata.revision : "missing";
  const catalogSource = typeof metadata.catalog_source === "object" && metadata.catalog_source !== null
    ? metadata.catalog_source as Record<string, unknown>
    : {};

  const saveOverride = (model: Record<string, unknown>, expectedRevision: string) => {
    overrideMutation.mutate({ expectedRevision, model });
  };

  const joinRoute = (modelId: string, routeName: string) => {
    // 现有 Route 语法以逗号分隔候选；携带逗号的模型 ID 无法安全加入。
    if (modelId.includes(",")) {
      setError("该模型 ID 含有路线分隔符，无法按现有语法加入 Route");
      return;
    }
    const routesValue = snapshot.agent?.savedValue;
    const routesTable = typeof routesValue === "object" && routesValue !== null
      ? (routesValue as Record<string, unknown>).model_routes
      : undefined;
    const route = typeof routesTable === "object" && routesTable !== null && !Array.isArray(routesTable)
      ? (routesTable as Record<string, unknown>)[routeName]
      : undefined;
    const candidatesRaw = typeof route === "object" && route !== null && Array.isArray((route as Record<string, unknown>).candidates)
      ? (route as Record<string, unknown>).candidates as unknown[]
      : [];
    const current = normalizeCandidates(candidatesRaw.map((value) => (typeof value === "string" ? value : "")));
    const result = addCandidate(current, `${connection.toLowerCase()}:${modelId}`);
    if (result.error) {
      setError(result.error);
      return;
    }
    routeMutation.mutate({
      revision: snapshot.agent!.revision,
      changes: [{ action: "set_model_route", name: routeName, candidates: result.list }],
    });
  };

  const openEditor = (draftValue: ModelOverrideDraft) => {
    setDraftRevision(metadataRevision);
    setDraft(draftValue);
  };

  return (
    <section aria-label={`模型管理 ${connection}`} className="flex flex-col gap-3 border-t border-line-inner pt-4">
      <div className="flex flex-wrap items-center gap-2">
        <h4 className="m-0 text-sm font-bold">
          模型
          <span className="ml-2 font-mono text-xs font-normal text-muted">共 {rows.length} 个 · 启用 {enabledCount}</span>
        </h4>
        <div className="ml-auto flex flex-wrap gap-2">
          <Button variant="secondary" className="px-2.5 py-1 text-xs" disabled={!enabled || discoveryPending} onClick={() => {
            setStatus("");
            discoveryMutation.mutate(discoveryState.scope);
          }}>
            {discoveryPending ? "正在同步…" : "同步模型"}
          </Button>
          <Button variant="secondary" className="px-2.5 py-1 text-xs" disabled={metadataQuery.isPending} onClick={() => void metadataQuery.refetch()}>
            刷新模型信息
          </Button>
          <Button className="px-2.5 py-1 text-xs" disabled={!editable || !metadataQuery.isSuccess} onClick={() => openEditor(emptyModelOverrideDraft())}>
            + 添加模型
          </Button>
        </div>
      </div>
      <p role="status" className="m-0 text-xs text-muted">
        {error
          ? error
          : status
            ? status
            : discovery
              ? discoveryLabel(discovery)
              : metadataQuery.isPending
                ? "正在读取本地模型与目录…"
                : "模型信息已加载；尚未同步 Connection 模型列表（unknown）"}
      </p>

      <div className="flex flex-wrap items-center gap-2">
        <Input
          aria-label="搜索模型 ID / 显示名称"
          placeholder="搜索模型 ID / 显示名称…"
          value={search}
          onChange={(event) => {
            setSearch(event.target.value);
            setVisibleLimit(50);
          }}
          className="min-w-0 flex-1 py-1.5 text-sm"
        />
        <select
          aria-label="模型状态筛选"
          value={statusFilter}
          onChange={(event) => {
            setStatusFilter(event.target.value);
            setVisibleLimit(50);
          }}
          className="rounded-console border border-line bg-input px-2 py-1.5 text-sm text-ink outline-none"
        >
          {MODEL_FILTERS.map(([value, label]) => (
            <option key={value} value={value}>{label}</option>
          ))}
        </select>
      </div>

      {metadataQuery.isError ? (
        <p role="alert" className="m-0 text-sm font-semibold text-error">
          {metadataQuery.error instanceof Error ? metadataQuery.error.message : "模型信息读取失败"}
        </p>
      ) : null}

      <div className="flex flex-col divide-y divide-line-inner rounded-console-lg border border-line">
        {visibleRows.length === 0 && !metadataQuery.isPending ? (
          <p className="m-0 px-3 py-6 text-sm text-muted">当前没有匹配的展示条目；可先「同步模型」或「添加模型」。</p>
        ) : null}
        {visibleRows.slice(0, visibleLimit).map((row) => (
          <ModelRow
            // 只重建诊断所在行，保留面板内尚未保存的编辑草稿；旧请求无法回写新行。
            key={JSON.stringify([connection, row.id, discoveryRevision, credentialRevision])}
            row={row}
            connection={connection}
            discoveryRevision={discoveryRevision}
            catalogSource={catalogSource}
            editable={editable}
            enabled={enabled}
            routes={modelRoutes}
            busy={overrideMutation.isPending || routeMutation.isPending}
            onEdit={() => openEditor(draftFromOverride(row.id, row.override))}
            onToggleEnabled={() => {
              saveOverride(
                { ...row.override, provider: metadata.provider, id: row.id, enabled: row.metadata.enabled === false },
                metadataRevision,
              );
            }}
            onJoinRoute={(routeName) => joinRoute(row.id, routeName)}
          />
        ))}
        {visibleRows.length > visibleLimit ? (
          <div className="p-2">
            <Button variant="secondary" onClick={() => setVisibleLimit((limit) => limit + 50)}>
              显示更多模型（{visibleRows.length - visibleLimit}）
            </Button>
          </div>
        ) : null}
      </div>

      {draft ? (
        <ModelOverrideEditor
          draft={draft}
          revision={draftRevision}
          disabled={!editable || !metadataQuery.isSuccess}
          busy={overrideMutation.isPending}
          onChange={setDraft}
          onCancel={() => setDraft(null)}
          onSave={() => {
            if (!metadataQuery.isSuccess) {
              setError("模型信息尚未加载成功");
              return;
            }
            try {
              const model = buildModelOverride(draft);
              // 仅本次编辑保存成功后关闭；失败或已切换的草稿继续保留。
              overrideMutation.mutate(
                { model: { ...model, provider: metadata.provider }, expectedRevision: draftRevision },
                { onSuccess: () => setDraft((current) => current === draft ? null : current) },
              );
            } catch (cause) {
              setError(cause instanceof Error ? cause.message : "模型信息无效");
            }
          }}
        />
      ) : null}
    </section>
  );
}

type ModelRowProps = {
  row: ReturnType<typeof mergeModels>[number];
  connection: string;
  discoveryRevision: string;
  catalogSource: Record<string, unknown>;
  editable: boolean;
  enabled: boolean;
  routes: string[];
  busy: boolean;
  onEdit: () => void;
  onToggleEnabled: () => void;
  onJoinRoute: (route: string) => void;
};

function ModelRow({ row, connection, discoveryRevision, catalogSource, editable, enabled, routes, busy, onEdit, onToggleEnabled, onJoinRoute }: ModelRowProps) {
  const [selectedRoute, setSelectedRoute] = useState(routes[0] ?? "");
  const [check, setCheck] = useState<CheckState>({ kind: "idle" });
  const status = typeof row.metadata.status === "string" ? row.metadata.status : "unknown";
  const displayName = typeof row.metadata.display_name === "string" ? row.metadata.display_name : "";
  const contextWindow = row.metadata.context_window ?? "unknown";
  const maxOutput = row.metadata.max_output_tokens ?? "unknown";
  const capabilities = typeof row.metadata.capabilities === "object" && row.metadata.capabilities !== null
    ? row.metadata.capabilities as Record<string, unknown>
    : {};
  const declared = CAPABILITY_KEYS.filter((key) => {
    const claim = capabilities[key];
    return typeof claim === "object" && claim !== null && (claim as Record<string, unknown>).advertised === true;
  });
  const enabledLocally = row.metadata.enabled !== false;

  const checkMutation = useMutation({
    mutationFn: () => testProviderConnection(connection, discoveryRevision, row.id),
    onMutate: () => setCheck({ kind: "pending" }),
    onSuccess: (diagnostic) => {
      if (diagnostic.model_call === "success") {
        setCheck({ kind: "ok", summary: `可用 · ${String(diagnostic.elapsed_ms ?? "?")} ms` });
        return;
      }
      setCheck({
        kind: "failed",
        message: `模型调用未成功：${CHECK_LABELS[String(diagnostic.model_call)] ?? String(diagnostic.model_call)} · 网络 ${CHECK_LABELS[String(diagnostic.network)] ?? String(diagnostic.network)} · 认证 ${CHECK_LABELS[String(diagnostic.authentication)] ?? String(diagnostic.authentication)}`,
      });
    },
    onError: (cause) => setCheck({ kind: "failed", message: cause instanceof Error ? cause.message : "连接测试失败" }),
  });

  return (
    <article className="flex flex-col gap-3 px-3 py-2.5 sm:flex-row sm:items-start sm:gap-3">
      <Switch
        checked={enabledLocally}
        disabled={busy || !editable}
        aria-label={`${enabledLocally ? "禁用模型" : "启用模型"} ${row.id}`}
        onChange={onToggleEnabled}
      />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span className="font-mono text-sm font-bold break-all text-ink">{row.id}</span>
          {displayName && displayName !== row.id ? <span className="text-xs text-muted">{displayName}</span> : null}
          {status !== "unknown" ? (
            <StatusBadge tone={STATUS_TONES[status] ?? "neutral"} label={status} className="px-1.5 py-0.5 font-mono text-[0.64rem]" />
          ) : null}
          {!enabledLocally ? (
            <span className="border border-line bg-input px-1.5 py-0.5 text-[0.64rem] font-semibold text-warning">已禁用</span>
          ) : null}
          {declared.map((key) => (
            <span
              key={key}
              title={`声明能力（Advertised）：${CAPABILITY_LABELS[key]}`}
              className="border border-line bg-accent-soft px-1.5 py-0.5 text-[0.64rem] font-semibold text-accent-strong"
            >
              {CAPABILITY_LABELS[key]}
            </span>
          ))}
          {row.sources.map((source) => (
            <span key={source} className="border border-line px-1.5 py-0.5 font-mono text-[0.64rem] text-muted">
              {SOURCE_BADGES[source] ?? source}
            </span>
          ))}
        </div>
        <p className="m-0 mt-1 text-[0.7rem] leading-relaxed text-muted">
          <span>模型 ID：{row.id}</span>
          <span> · Context：{String(contextWindow)} · Max output：{String(maxOutput)}</span>
          {check.kind === "pending" ? <span> · 正在检查连通性…</span> : ""}
          {check.kind === "ok" ? <span className="font-semibold text-success"> · ✓ {check.summary}</span> : ""}
          {check.kind === "failed" ? <span className="font-semibold text-error"> · ✗ {check.message}</span> : ""}
        </p>
        <details className="mt-1">
          <summary className="cursor-pointer text-[0.7rem] font-semibold text-muted">模型信息 / Advertised / Provenance</summary>
          <ModelDetails metadata={row.metadata} catalogSource={catalogSource} />
        </details>
      </div>
      <div className="flex shrink-0 flex-col gap-1.5 sm:items-end">
        <div className="flex flex-1 gap-1.5 sm:flex-none">
          <Button
            variant="secondary"
            className="px-2.5 py-1 text-xs"
            disabled={!enabled || checkMutation.isPending}
            onClick={() => checkMutation.mutate()}
          >
            {check.kind === "pending" ? "检查中…" : "检查"}
          </Button>
          <Button variant="secondary" className="px-2.5 py-1 text-xs" onClick={onEdit}>编辑</Button>
        </div>
        {routes.length > 0 ? (
          <div className="flex flex-wrap gap-1.5">
            <select
              aria-label={`为 ${row.id} 选择 Route`}
              value={selectedRoute}
              onChange={(event) => setSelectedRoute(event.target.value)}
              className="rounded-console border border-line bg-input px-2 py-1 text-xs text-ink outline-none"
            >
              {routes.map((name) => (
                <option key={name} value={name}>{name}</option>
              ))}
            </select>
            <Button
              variant="secondary"
              className="px-2.5 py-1 text-xs"
              // 停用 Connection 或本地禁用的模型不允许进入候选链。
              disabled={!enabled || !enabledLocally || !editable || routes.length === 0 || busy}
              onClick={() => onJoinRoute(selectedRoute)}
            >
              加入 Route
            </Button>
          </div>
        ) : null}
      </div>
    </article>
  );
}

/** 单条模型的连通性检查结果；不把诊断成功以外的状态伪装成可用。 */
type CheckState =
  | { kind: "idle" }
  | { kind: "pending" }
  | { kind: "ok"; summary: string }
  | { kind: "failed"; message: string };

function ModelDetails({ metadata, catalogSource }: { metadata: Record<string, unknown>; catalogSource: Record<string, unknown> }) {
  const recordOf = (value: unknown) => (typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {});
  const modalities = recordOf(metadata.modalities);
  const price = recordOf(metadata.price);
  const capabilities = recordOf(metadata.capabilities);
  const provenance = recordOf(metadata.provenance);
  const arrayText = (value: unknown) => (Array.isArray(value) ? value.map(String).join("、") : "");
  const row = (label: string, value: unknown, origin: unknown) => (
    <p className="m-0 text-xs text-muted">
      {label}：{value === null || value === undefined || value === "" ? "未知" : String(value)}
      {typeof origin === "string" ? ` · 来源：${PROVENANCE_LABELS[origin] ?? origin}` : ""}
    </p>
  );
  return (
    <div className="mt-1 flex flex-col gap-0.5">
      {row("显示名称", metadata.display_name, provenance.display_name)}
      {row("上下文窗口", metadata.context_window, provenance.context_window)}
      {row("最大输出", metadata.max_output_tokens, provenance.max_output_tokens)}
      {row("状态", metadata.status, provenance.status)}
      {row("输入模态", arrayText(modalities.input) || "未知", provenance.modalities)}
      {row("输出模态", arrayText(modalities.output) || "未知", provenance.modalities)}
      {row("输入价格（美元 / 百万 token）", price.input_per_million_usd, provenance.price)}
      {row("输出价格（美元 / 百万 token）", price.output_per_million_usd, provenance.price)}
      {CAPABILITY_KEYS.map((key) => {
        const claim = recordOf(capabilities[key]).advertised;
        return (
          <span key={key} className="contents">
            {row(`${CAPABILITY_LABELS[key]} · 声明能力（Advertised）`, claim === true ? "声明支持" : claim === false ? "未声明支持" : "未知", recordOf(provenance.capabilities)[key])}
            {row(`${CAPABILITY_LABELS[key]} · 已验证能力（Verified）`, "未知", null)}
          </span>
        );
      })}
      {Object.keys(catalogSource).length > 0 ? (
        <details>
          <summary className="cursor-pointer text-xs font-semibold">目录出处</summary>
          <div className="mt-1 flex flex-col gap-0.5">
            {(Object.entries(CATALOG_SOURCE_LABELS) as Array<[string, string]>).map(([key, label]) => (
              <p key={key} className="m-0 text-xs text-muted">{label}：{catalogSource[key] == null ? "未知" : String(catalogSource[key])}</p>
            ))}
          </div>
        </details>
      ) : null}
    </div>
  );
}

const PROVENANCE_LABELS: Record<string, string> = { catalog: "目录", official_patch: "官方补丁", local_override: "本地覆盖" };

const CATALOG_SOURCE_LABELS: Record<string, string> = {
  name: "数据源",
  source_url: "上游地址",
  source_version: "版本",
  fetched_at: "快照时间",
  upstream_license: "许可",
  source_hash: "校验摘要",
  converter_version: "转换版本",
};

type ModelOverrideEditorProps = {
  draft: ModelOverrideDraft;
  revision: string;
  disabled: boolean;
  busy: boolean;
  onChange: (draft: ModelOverrideDraft) => void;
  onCancel: () => void;
  onSave: () => void;
};

/** 本地新增 / 编辑模型表单；字段留空表示继承目录或保持未知。 */
function ModelOverrideEditor({ draft, revision, disabled, busy, onChange, onCancel, onSave }: ModelOverrideEditorProps) {
  const patch = (values: Partial<ModelOverrideDraft>) => onChange({ ...draft, ...values });
  const toggleModality = (direction: "inputModalities" | "outputModalities", value: string) => {
    const current = draft[direction];
    patch({
      [direction]: current.includes(value) ? current.filter((item) => item !== value) : [...current, value],
    } as Partial<ModelOverrideDraft>);
  };
  return (
    <div className="rounded-console-lg border border-line bg-surface p-3">
      <div className="flex items-center justify-between gap-2">
        <p className="m-0 text-sm font-bold">本地新增 / 编辑模型</p>
        <Button variant="secondary" className="px-2 py-0.5 text-xs" onClick={onCancel}>收起表单</Button>
      </div>
      <p className="m-0 mt-1 text-xs text-muted">留空或选择“继承”会使用目录信息。没有目录信息时保持未知。能力声明仅用于展示。</p>
      <div className="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2">
        <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
          模型 ID
          <input
            aria-label="模型 ID"
            className="rounded-console border border-line bg-input px-3 py-2 text-sm font-normal text-ink outline-none"
            value={draft.id}
            readOnly={Boolean(draft.id)}
            placeholder="例如 my-model-v2"
            onChange={(event) => patch({ id: event.target.value })}
          />
          {draft.id ? <span className="text-[0.7rem] font-normal">已有条目不可修改 ID</span> : null}
        </label>
        <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
          显示名称（留空继承）
          <input
            aria-label="显示名称（留空继承）"
            className="rounded-console border border-line bg-input px-3 py-2 text-sm font-normal text-ink outline-none"
            value={draft.display_name}
            onChange={(event) => patch({ display_name: event.target.value })}
          />
        </label>
        <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
          上下文窗口（token，留空继承）
          <input
            aria-label="上下文窗口（token，留空继承）"
            type="number"
            min={0}
            className="rounded-console border border-line bg-input px-3 py-2 text-sm font-normal text-ink outline-none"
            value={draft.context_window}
            onChange={(event) => patch({ context_window: event.target.value })}
          />
        </label>
        <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
          最大输出（token，留空继承）
          <input
            aria-label="最大输出（token，留空继承）"
            type="number"
            min={0}
            className="rounded-console border border-line bg-input px-3 py-2 text-sm font-normal text-ink outline-none"
            value={draft.max_output_tokens}
            onChange={(event) => patch({ max_output_tokens: event.target.value })}
          />
        </label>
        <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
          模型状态
          <select
            aria-label="模型状态"
            className="rounded-console border border-line bg-input px-3 py-2 text-sm font-normal text-ink outline-none"
            value={draft.status}
            onChange={(event) => patch({ status: event.target.value })}
          >
            <option value="">继承</option>
            <option value="active">正式</option>
            <option value="beta">测试版</option>
            <option value="deprecated">已弃用</option>
          </select>
        </label>
        <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
          本地启用状态
          <select
            aria-label="本地启用状态"
            className="rounded-console border border-line bg-input px-3 py-2 text-sm font-normal text-ink outline-none"
            value={draft.enabled}
            onChange={(event) => patch({ enabled: event.target.value })}
          >
            <option value="">继承（启用）</option>
            <option value="true">启用</option>
            <option value="false">禁用</option>
          </select>
        </label>
        <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
          输入价格（美元 / 百万 token）
          <input
            aria-label="输入价格（美元 / 百万 token）"
            type="number"
            min={0}
            step="any"
            className="rounded-console border border-line bg-input px-3 py-2 text-sm font-normal text-ink outline-none"
            value={draft.inputPrice}
            onChange={(event) => patch({ inputPrice: event.target.value })}
          />
        </label>
        <label className="flex flex-col gap-1 text-xs font-semibold text-muted">
          输出价格（美元 / 百万 token）
          <input
            aria-label="输出价格（美元 / 百万 token）"
            type="number"
            min={0}
            step="any"
            className="rounded-console border border-line bg-input px-3 py-2 text-sm font-normal text-ink outline-none"
            value={draft.outputPrice}
            onChange={(event) => patch({ outputPrice: event.target.value })}
          />
        </label>
      </div>
      <fieldset className="mt-3 border border-line p-2">
        <legend className="px-1 text-xs font-bold">输入 / 输出模态</legend>
        <label className="flex items-center gap-2 text-xs text-muted">
          <input
            type="checkbox"
            checked={draft.modalitiesInherit}
            onChange={(event) => patch({ modalitiesInherit: event.target.checked })}
          />
          继承目录模态
        </label>
        <div className="mt-2 grid grid-cols-2 gap-3">
          {(["inputModalities", "outputModalities"] as const).map((direction) => (
            <div key={direction}>
              <p className="m-0 text-xs font-semibold">{direction === "inputModalities" ? "输入" : "输出"}</p>
              {MODALITY_OPTIONS.map((value, index) => (
                <label key={value} className="flex items-center gap-1.5 text-xs text-muted">
                  <input
                    type="checkbox"
                    value={value}
                    disabled={draft.modalitiesInherit}
                    checked={draft[direction].includes(value)}
                    onChange={() => toggleModality(direction, value)}
                  />
                  {["文本", "图像", "音频", "视频", "PDF"][index]}
                </label>
              ))}
            </div>
          ))}
        </div>
      </fieldset>
      <div className="mt-3 grid grid-cols-1 gap-3 md:grid-cols-3">
        {CAPABILITY_KEYS.map((key) => (
          <label key={key} className="flex flex-col gap-1 text-xs font-semibold text-muted">
            {`${CAPABILITY_LABELS[key]} · 声明能力`}
            <select
              aria-label={`${CAPABILITY_LABELS[key]} · 声明能力`}
              className="rounded-console border border-line bg-input px-3 py-2 text-sm font-normal text-ink outline-none"
              value={draft.claims[key]}
              onChange={(event) => patch({ claims: { ...draft.claims, [key]: event.target.value } })}
            >
              <option value="">继承</option>
              <option value="true">声明支持</option>
              <option value="false">未声明支持</option>
            </select>
          </label>
        ))}
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <Button disabled={disabled || busy} onClick={onSave}>{busy ? "保存中…" : "保存本地模型"}</Button>
        <Button variant="secondary" disabled={disabled || busy} onClick={() => onChange(emptyModelOverrideDraft())}>新增另一模型</Button>
        <span className="text-xs text-muted">基于 revision {revision} 提交（CAS）</span>
      </div>
    </div>
  );
}

/** revision 冲突不覆盖服务器版本：保留本地表单并提示刷新比较。 */
function overrideErrorMessage(cause: unknown): string {
  if (cause instanceof ConsoleApiError && (cause.code === "config_conflict" || cause.status === 409)) {
    return "模型信息已被其他操作修改，未覆盖服务器版本。请刷新模型信息后重试；本地输入已保留。";
  }
  return cause instanceof Error ? cause.message : "模型保存失败";
}
