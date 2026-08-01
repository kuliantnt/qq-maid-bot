import { element, integerInput, positiveNumber, record, string } from "./fields.js";
export const DEFAULT_WEB_SEARCH_CONFIG = {
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
/** 只识别统一的 tools.web_search；旧顶层 search_routes 不参与页面读取。 */
export function readAgentWebSearchConfig(documentValue) {
    const webSearch = record(record(record(documentValue).tools).web_search);
    const routes = Object.fromEntries(Object.entries(record(webSearch.routes))
        .map(([name, value]) => [name, string(record(value).model)])
        .filter(([, model]) => model.length > 0));
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
export function webSearchConfigChange(config) {
    if (!Number.isInteger(config.maxResults) || config.maxResults < 1 || config.maxResults > 10) {
        throw new Error("Tavily 结果数必须是 1 到 10 之间的整数");
    }
    for (const [label, value] of [
        ["连接超时", config.connectTimeoutSeconds],
        ["首响应超时", config.firstResponseTimeoutSeconds],
        ["总超时", config.totalTimeoutSeconds],
    ]) {
        if (!Number.isInteger(value) || value < 1)
            throw new Error(`${label}必须是大于 0 的整数秒数`);
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
export function tavilyCredentialNotice(backend, configured) {
    return backend === "tavily" && !configured
        ? "已选择 Tavily，但 Tavily API Key 尚未配置。请先在“联网与工具”中保存 Key，重启后搜索才可用。"
        : "";
}
export function webSearchRouteChanges(savedRoutes, formRoutes) {
    const changes = [];
    for (const name of ["private_search", "group_search"]) {
        const model = (formRoutes[name] ?? "").trim();
        // 后端切换只更新联网搜索参数；空输入或未改动路线都保留当前 agent.toml 内容。
        if (model.length > 0 && model !== (savedRoutes[name] ?? "")) {
            changes.push({ action: "set_search_route", name, model });
        }
    }
    return changes;
}
export function webSearchFormConfig() {
    const backend = element("agent-web-search-backend", HTMLSelectElement).value;
    const searchDepth = element("agent-web-search-depth", HTMLSelectElement).value;
    const topic = element("agent-web-search-topic", HTMLSelectElement).value;
    const timeRange = element("agent-web-search-time-range", HTMLSelectElement).value;
    if (!isWebSearchBackend(backend))
        throw new Error("联网搜索后端无效");
    return {
        backend,
        maxResults: integerInput("agent-web-search-max-results"),
        searchDepth: searchDepth === "advanced" ? "advanced" : "basic",
        topic: topic === "news" || topic === "finance" ? topic : "general",
        timeRange: isWebSearchTimeRange(timeRange) ? timeRange : null,
        connectTimeoutSeconds: integerInput("agent-web-search-connect-timeout"),
        firstResponseTimeoutSeconds: integerInput("agent-web-search-first-response-timeout"),
        totalTimeoutSeconds: integerInput("agent-web-search-total-timeout"),
        routes: {},
    };
}
export function agentWebSearchKey(id) {
    return {
        "agent-web-search-backend": "backend",
        "agent-web-search-max-results": "maxResults",
        "agent-web-search-depth": "searchDepth",
        "agent-web-search-topic": "topic",
        "agent-web-search-time-range": "timeRange",
        "agent-web-search-connect-timeout": "connectTimeoutSeconds",
        "agent-web-search-first-response-timeout": "firstResponseTimeoutSeconds",
        "agent-web-search-total-timeout": "totalTimeoutSeconds",
    }[id] ?? "backend";
}
export function agentWebSearchInputValue(id) {
    const key = agentWebSearchKey(id);
    if (key === "backend" || key === "searchDepth" || key === "topic" || key === "timeRange") {
        return element(id, HTMLSelectElement).value;
    }
    return Number(element(id, HTMLInputElement).value);
}
export function updateTavilyCredentialStatus(backend, configured) {
    const row = element("agent-web-search-credential-status");
    const summary = row.querySelector("strong");
    const detail = row.querySelector(".field-meta");
    if (!summary || !detail)
        throw new Error("缺少 Tavily 凭据状态元素");
    const notice = tavilyCredentialNotice(backend, configured);
    summary.textContent = configured ? "Tavily API Key：已配置" : "Tavily API Key：未配置";
    detail.textContent = notice || (configured
        ? "密钥保存在安全配置中心，不会写入 agent.toml 或回传浏览器。"
        : "可在“联网与工具”中配置；未选择 Tavily 时不影响其他搜索后端。");
    row.classList.toggle("config-row-warning", notice.length > 0);
}
export function isWebSearchBackend(value) {
    return value === "provider_native" || value === "tavily" || value === "disabled";
}
export function isWebSearchTimeRange(value) {
    return value === "day" || value === "week" || value === "month" || value === "year";
}
export function webSearchBackendLabel(value) {
    return { provider_native: "Provider 原生搜索", tavily: "Tavily", disabled: "已关闭" }[value];
}
