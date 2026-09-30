/**
 * Agent 联网搜索配置的读取与保存投影。
 *
 * 从旧版 views/configuration/web-search.ts 平移：只识别统一的 tools.web_search，
 * 旧顶层 search_routes 不参与读取；保存契约仍是单个 set_web_search 操作，
 * 不携带 route 或 secret，搜索路线单独走 set_search_route。
 */

export type AgentWebSearchBackend = "provider_native" | "tavily" | "disabled";

export interface AgentWebSearchConfig {
  backend: AgentWebSearchBackend;
  maxResults: number;
  searchDepth: "basic" | "advanced";
  topic: "general" | "news" | "finance";
  timeRange: "day" | "week" | "month" | "year" | null;
  connectTimeoutSeconds: number;
  firstResponseTimeoutSeconds: number;
  totalTimeoutSeconds: number;
  routes: Record<string, string>;
}

export const DEFAULT_WEB_SEARCH_CONFIG: AgentWebSearchConfig = {
  backend: "provider_native",
  maxResults: 5,
  searchDepth: "basic",
  topic: "general",
  timeRange: null,
  connectTimeoutSeconds: 10,
  firstResponseTimeoutSeconds: 30,
  totalTimeoutSeconds: 60,
  routes: {},
};

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null;
}

function record(value: unknown): Record<string, unknown> {
  return isRecord(value) ? { ...value } : {};
}

function string(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function positiveNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback;
}

/** 只识别统一的 tools.web_search；旧顶层 search_routes 不参与页面读取。 */
export function readAgentWebSearchConfig(documentValue: unknown): AgentWebSearchConfig {
  const webSearch = record(record(record(documentValue).tools).web_search);
  const routes = Object.fromEntries(
    Object.entries(record(webSearch.routes))
      .map(([name, value]) => [name, string(record(value).model)] as const)
      .filter(([, model]) => model.length > 0),
  );
  const backend = string(webSearch.backend);
  const searchDepth = string(webSearch.search_depth);
  const topic = string(webSearch.topic);
  const timeRange = string(webSearch.time_range);
  return {
    backend: isWebSearchBackend(backend) ? backend : DEFAULT_WEB_SEARCH_CONFIG.backend,
    maxResults: positiveNumber(webSearch.max_results, DEFAULT_WEB_SEARCH_CONFIG.maxResults),
    searchDepth: searchDepth === "advanced" ? "advanced" : "basic",
    topic: topic === "news" || topic === "finance" ? topic : "general",
    timeRange: isWebSearchTimeRange(timeRange) ? timeRange : null,
    connectTimeoutSeconds: positiveNumber(webSearch.connect_timeout_seconds, DEFAULT_WEB_SEARCH_CONFIG.connectTimeoutSeconds),
    firstResponseTimeoutSeconds: positiveNumber(webSearch.first_response_timeout_seconds, DEFAULT_WEB_SEARCH_CONFIG.firstResponseTimeoutSeconds),
    totalTimeoutSeconds: positiveNumber(webSearch.total_timeout_seconds, DEFAULT_WEB_SEARCH_CONFIG.totalTimeoutSeconds),
    routes,
  };
}

/** 保存前在页面侧完成结果数边界与超时顺序校验，错误必须阻塞提交而不是等后端拒绝。 */
export function webSearchConfigChange(config: AgentWebSearchConfig): Record<string, unknown> {
  if (!Number.isInteger(config.maxResults) || config.maxResults < 1 || config.maxResults > 10) {
    throw new Error("Tavily 结果数必须是 1 到 10 之间的整数");
  }
  for (const [label, value] of [
    ["连接超时", config.connectTimeoutSeconds],
    ["首响应超时", config.firstResponseTimeoutSeconds],
    ["总超时", config.totalTimeoutSeconds],
  ] as const) {
    if (!Number.isInteger(value) || value < 1) throw new Error(`${label}必须是大于 0 的整数秒数`);
  }
  if (config.connectTimeoutSeconds > config.firstResponseTimeoutSeconds) {
    throw new Error("连接超时不能大于首响应超时");
  }
  if (config.firstResponseTimeoutSeconds > config.totalTimeoutSeconds) {
    throw new Error("首响应超时不能大于总超时");
  }
  return {
    action: "set_web_search",
    backend: config.backend,
    max_results: config.maxResults,
    search_depth: config.searchDepth,
    topic: config.topic,
    time_range: config.timeRange,
    connect_timeout_seconds: config.connectTimeoutSeconds,
    first_response_timeout_seconds: config.firstResponseTimeoutSeconds,
    total_timeout_seconds: config.totalTimeoutSeconds,
  };
}

export function tavilyCredentialNotice(backend: AgentWebSearchBackend, configured: boolean): string {
  return backend === "tavily" && !configured
    ? "已选择 Tavily，但 Tavily API Key 尚未配置。请先在“联网与工具”中保存 Key，重启后搜索才可用。"
    : "";
}

export function webSearchRouteChanges(
  savedRoutes: Record<string, string>,
  formRoutes: Record<string, string>,
): Array<Record<string, unknown>> {
  const changes: Array<Record<string, unknown>> = [];
  for (const name of ["private_search", "group_search"]) {
    const model = (formRoutes[name] ?? "").trim();
    // 后端切换只更新联网搜索参数；空输入或未改动路线都保留当前 agent.toml 内容。
    if (model.length > 0 && model !== (savedRoutes[name] ?? "")) {
      changes.push({ action: "set_search_route", name, model });
    }
  }
  return changes;
}

export function isWebSearchBackend(value: string): value is AgentWebSearchBackend {
  return value === "provider_native" || value === "tavily" || value === "disabled";
}

export function isWebSearchTimeRange(value: string): value is "day" | "week" | "month" | "year" {
  return value === "day" || value === "week" || value === "month" || value === "year";
}

export function webSearchBackendLabel(value: AgentWebSearchBackend): string {
  return ({ provider_native: "Provider 原生搜索", tavily: "Tavily", disabled: "已关闭" } as const)[value];
}
