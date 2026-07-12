class ConsoleApiError extends Error {
    constructor(message) {
        super(message);
        this.name = "ConsoleApiError";
    }
}
export async function fetchConsoleStatus() {
    const value = await fetchJson("/api/v1/console/status", { headers: { Accept: "application/json" } });
    const root = record(value);
    return {
        runtime: parseRuntime(root.runtime),
        provider: parseProvider(root.provider),
        platforms: array(root.platforms).map(parsePlatform),
        storage: array(root.storage).map(parseStorage),
        configuration: parseConfiguration(root.configuration),
    };
}
export async function renderMarkdown(markdown) {
    const value = await fetchJson("/api/v1/markdown/render", {
        method: "POST",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({ markdown }),
    });
    const payload = record(value);
    if (payload.ok !== true || typeof payload.html !== "string") {
        throw new ConsoleApiError("Markdown 渲染服务返回了无法识别的结果");
    }
    return payload.html;
}
async function fetchJson(input, init) {
    let response;
    try {
        response = await fetch(input, init);
    }
    catch {
        throw new ConsoleApiError("无法连接本地管理接口，请检查服务是否仍在运行");
    }
    if (!response.ok) {
        throw new ConsoleApiError(`管理接口请求失败（HTTP ${response.status}）`);
    }
    try {
        return await response.json();
    }
    catch {
        throw new ConsoleApiError("管理接口返回了无效 JSON");
    }
}
function parseRuntime(value) {
    const item = record(value);
    return {
        ok: item.ok === true,
        version: string(item.version, "unknown"),
        startedAt: nullableString(item.started_at),
        uptimeSeconds: finiteNumber(item.uptime_seconds),
    };
}
function parseProvider(value) {
    const item = record(value);
    const upstream = record(item.upstream);
    return {
        name: string(item.name, "unknown"),
        model: string(item.model, "unknown"),
        streaming: nullableBoolean(item.streaming),
        configured: item.configured === true,
        upstreamState: string(upstream.state, "unknown"),
        lastCheckedAt: nullableString(upstream.last_checked_at),
        errorSummary: nullableString(upstream.error_summary),
    };
}
function parsePlatform(value) {
    const item = record(value);
    return {
        id: string(item.id, "unknown"),
        label: string(item.label, "未知平台"),
        configured: item.configured === true,
        enabled: item.enabled === true,
        state: runtimeState(item.state),
        lastEventAt: nullableString(item.last_event_at),
        lastErrorSummary: nullableString(item.last_error_summary),
        readyAt: nullableString(item.ready_at),
        resumedAt: nullableString(item.resumed_at),
        capabilityScopes: array(item.capability_scopes).map(parseCapabilityScope),
    };
}
function parseCapabilityScope(value) {
    const item = record(value);
    return {
        id: string(item.id, "unknown"),
        label: string(item.label, "未知作用域"),
        enabled: item.enabled === true,
        capabilities: parseDirectionalCapabilities(item.capabilities),
    };
}
function parseCapabilities(value) {
    const item = record(value);
    return {
        text: valueState(item.text),
        markdown: valueState(item.markdown),
        image: valueState(item.image),
        file: valueState(item.file),
        mixedMessage: valueState(item.mixed_message),
        streaming: valueState(item.streaming),
    };
}
function parseDirectionalCapabilities(value) {
    const item = record(value);
    return {
        inbound: parseCapabilities(item.inbound),
        outbound: parseCapabilities(item.outbound),
    };
}
function parseStorage(value) {
    const item = record(value);
    return {
        id: string(item.id, "unknown"),
        label: string(item.label, "未知存储"),
        pathSummary: string(item.path_summary, "not_available"),
        state: runtimeState(item.state),
        exists: nullableBoolean(item.exists),
        readable: nullableBoolean(item.readable),
        writable: nullableBoolean(item.writable),
        errorSummary: nullableString(item.error_summary),
        schemaSummary: nullableString(item.schema_summary),
    };
}
function parseConfiguration(value) {
    const item = record(value);
    return {
        listen: string(item.listen, "unknown"),
        corsAllowlistConfigured: item.cors_allowlist_configured === true,
        rssEnabled: item.rss_enabled === true,
        toolCallingEnabled: item.tool_calling_enabled === true,
    };
}
function record(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value)
        ? value
        : {};
}
function array(value) {
    return Array.isArray(value) ? value : [];
}
function string(value, fallback) {
    return typeof value === "string" && value.length > 0 ? value : fallback;
}
function nullableString(value) {
    return typeof value === "string" && value.length > 0 ? value : null;
}
function nullableBoolean(value) {
    return typeof value === "boolean" ? value : null;
}
function finiteNumber(value) {
    return typeof value === "number" && Number.isFinite(value) ? value : null;
}
function runtimeState(value) {
    return value === "online" || value === "offline" || value === "available" || value === "not_available" || value === "not_configured"
        ? value
        : "unknown";
}
function valueState(value) {
    return value === "supported" || value === "disabled" || value === "unsupported" || value === "not_available" || value === "not_configured"
        ? value
        : "unknown";
}
