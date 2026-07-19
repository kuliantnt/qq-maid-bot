import type {
  CapabilityStatus,
  CapabilityScopeStatus,
  ConfigurationStatus,
  ConsoleStatus,
  DirectionalCapabilityStatus,
  PlatformStatus,
  ProviderStatus,
  RuntimeState,
  RuntimeStatus,
  StorageStatus,
  ValueState,
  AdminSession,
  BootstrapStatus,
  ConfigurationSnapshot,
  ConfigFieldSnapshot,
  RegisteredTool,
} from "./types.js";

export class ConsoleApiError extends Error {
  constructor(message: string, readonly code = "request_failed", readonly status = 0) {
    super(message);
    this.name = "ConsoleApiError";
  }
}

let csrfToken = "";

export function setCsrfToken(value: string): void {
  csrfToken = value;
}

export async function fetchSession(): Promise<AdminSession> {
  const payload = record(await fetchJson("/api/v1/console/session", {
    headers: { Accept: "application/json" },
  }));
  const session = parseSession(payload.session);
  setCsrfToken(session.csrfToken);
  return session;
}

export async function fetchBootstrap(): Promise<BootstrapStatus> {
  const payload = record(await fetchJson("/api/v1/console/auth/bootstrap", {
    headers: { Accept: "application/json" },
  }));
  return parseBootstrapStatus(payload.bootstrap);
}

export async function issuePreAuth(): Promise<string> {
  const payload = record(await mutatingJson("/api/v1/console/auth/preauth", "POST"));
  const token = string(payload.csrf_token, "");
  if (!token) throw new ConsoleApiError("认证服务未返回 CSRF token", "invalid_response");
  setCsrfToken(token);
  return token;
}

export async function initializeAdmin(username: string, password: string, bootstrapToken: string): Promise<AdminSession> {
  const payload = record(await mutatingJson("/api/v1/console/auth/initialize", "POST", {
    username,
    password,
    bootstrap_token: bootstrapToken,
  }));
  const session = parseSession(payload.session);
  setCsrfToken(session.csrfToken);
  return session;
}

export async function requestPasswordReset(): Promise<BootstrapStatus> {
  const payload = record(await mutatingJson("/api/v1/console/auth/password-reset/bootstrap", "POST"));
  return parseBootstrapStatus(payload.bootstrap);
}

export async function resetAdminPassword(password: string, bootstrapToken: string): Promise<AdminSession> {
  const payload = record(await mutatingJson("/api/v1/console/auth/password-reset", "POST", {
    password,
    bootstrap_token: bootstrapToken,
  }));
  const session = parseSession(payload.session);
  setCsrfToken(session.csrfToken);
  return session;
}

export async function loginAdmin(username: string, password: string): Promise<AdminSession> {
  const payload = record(await mutatingJson("/api/v1/console/auth/login", "POST", { username, password }));
  const session = parseSession(payload.session);
  setCsrfToken(session.csrfToken);
  return session;
}

export async function logoutAdmin(): Promise<void> {
  await mutatingJson("/api/v1/console/auth/logout", "POST", undefined, true);
  setCsrfToken("");
}

export async function fetchConfiguration(): Promise<ConfigurationSnapshot> {
  const payload = record(await fetchJson("/api/v1/console/configuration", {
    headers: { Accept: "application/json" },
  }));
  return parseConfigurationPayload(payload);
}

export async function updateRuntimeConfiguration(expectedRevision: string, changes: unknown[]): Promise<ConfigurationSnapshot> {
  const payload = record(await mutatingJson("/api/v1/console/configuration/runtime", "PATCH", {
    expected_revision: expectedRevision,
    changes,
  }));
  return parseConfigurationPayload(payload);
}

export async function updateSecretConfiguration(changes: unknown[]): Promise<ConfigurationSnapshot> {
  const payload = record(await mutatingJson("/api/v1/console/configuration/secrets", "PATCH", { changes }));
  return parseConfigurationPayload(payload);
}

export async function updateAgentConfiguration(expectedRevision: string, changes: unknown[]): Promise<ConfigurationSnapshot> {
  const payload = record(await mutatingJson("/api/v1/console/configuration/agent", "PATCH", {
    expected_revision: expectedRevision,
    changes,
  }));
  return parseConfigurationPayload(payload);
}

export async function requestRestart(): Promise<string> {
  const payload = record(await mutatingJson("/api/v1/console/restart", "POST", {}));
  return string(payload.message, "重启命令已提交");
}

export async function validateConfiguration(): Promise<{ valid: boolean; message: string }> {
  const payload = record(await mutatingJson("/api/v1/console/configuration/validate", "POST", {}));
  const validation = record(payload.validation);
  return { valid: validation.valid === true, message: string(validation.message, "配置校验已完成") };
}

export async function testProviderConnection(target: string): Promise<{ success: boolean; classification: string; message: string }> {
  const payload = record(await mutatingJson("/api/v1/console/configuration/test-connection", "POST", { target }));
  const connection = record(payload.connection);
  return {
    success: connection.success === true,
    classification: string(connection.classification, "unknown"),
    message: string(connection.message, "连接测试已完成"),
  };
}

export async function fetchConsoleStatus(): Promise<ConsoleStatus> {
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

export async function renderMarkdown(markdown: string): Promise<string> {
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

async function fetchJson(input: RequestInfo | URL, init?: RequestInit): Promise<unknown> {
  let response: Response;
  try {
    response = await fetch(input, { credentials: "same-origin", ...init });
  } catch {
    throw new ConsoleApiError("无法连接本地管理接口，请检查服务是否仍在运行");
  }
  if (!response.ok) {
    let code = "request_failed";
    let message = `管理接口请求失败（HTTP ${response.status}）`;
    try {
      const payload = record(await response.json() as unknown);
      const error = record(payload.error);
      code = string(error.code, code);
      message = string(error.message, message);
    } catch { /* 保留稳定的 HTTP 错误摘要。 */ }
    throw new ConsoleApiError(message, code, response.status);
  }
  try {
    return await response.json() as unknown;
  } catch {
    throw new ConsoleApiError("管理接口返回了无效 JSON");
  }
}

async function mutatingJson(input: string, method: string, body?: unknown, allowEmpty = false): Promise<unknown> {
  const response = await fetch(input, {
    method,
    credentials: "same-origin",
    headers: {
      "Content-Type": "application/json",
      Accept: "application/json",
      "X-CSRF-Token": csrfToken,
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  if (allowEmpty && response.status === 204) return {};
  if (!response.ok) {
    let code = "request_failed";
    let message = `管理接口请求失败（HTTP ${response.status}）`;
    try {
      const payload = record(await response.json() as unknown);
      const error = record(payload.error);
      code = string(error.code, code);
      message = string(error.message, message);
    } catch { /* 保留稳定错误。 */ }
    throw new ConsoleApiError(message, code, response.status);
  }
  return await response.json() as unknown;
}

function parseRuntime(value: unknown): RuntimeStatus {
  const item = record(value);
  return {
    ok: item.ok === true,
    ready: item.ready === true,
    state: item.state === "ready" || item.state === "setup_required" ? item.state : "unknown",
    version: string(item.version, "unknown"),
    startedAt: nullableString(item.started_at),
    uptimeSeconds: finiteNumber(item.uptime_seconds),
  };
}

function parseBootstrapStatus(value: unknown): BootstrapStatus {
  const item = record(value);
  return {
    initialized: item.initialized === true,
    setupRequired: item.setup_required === true,
    passwordResetPending: item.password_reset_pending === true,
    tokenFile: string(item.token_file, "config/secrets/bootstrap.token"),
    expiresAt: finiteNumber(item.expires_at),
  };
}

function parseSession(value: unknown): AdminSession {
  const item = record(value);
  const token = string(item.csrf_token, "");
  if (!token) throw new ConsoleApiError("认证服务返回了无效会话", "invalid_response");
  return {
    username: string(item.username, "admin"),
    capabilities: array(item.capabilities).filter((value): value is string => typeof value === "string"),
    csrfToken: token,
    expiresAt: finiteNumber(item.expires_at) ?? 0,
  };
}

function parseConfigurationPayload(value: unknown): ConfigurationSnapshot {
  const payload = record(value);
  return parseConfigurationSnapshot(payload.configuration, payload.registered_tools, payload.restart);
}

function parseConfigurationSnapshot(value: unknown, toolsValue: unknown = [], restartValue: unknown = {}): ConfigurationSnapshot {
  const item = record(value);
  const agent = record(item.agent);
  return {
    revision: string(item.revision, "missing"),
    fileExists: item.file_exists === true,
    fields: array(item.fields).map(parseConfigField),
    registeredTools: array(toolsValue).map(parseRegisteredTool),
    restartAvailable: record(restartValue).available === true,
    agent: Object.keys(agent).length === 0 ? null : {
      revision: string(agent.revision, "missing"),
      fileExists: agent.file_exists === true,
      source: typeof agent.source === "string" ? agent.source as ConfigFieldSnapshot["source"] : "not_configured",
      editable: agent.editable === true,
      readOnly: agent.read_only === true,
      pendingRestart: agent.pending_restart === true,
      savedValue: agent.saved_value,
      runningValue: agent.running_value,
    },
  };
}

function parseRegisteredTool(value: unknown): RegisteredTool {
  const item = record(value);
  return {
    name: string(item.name, "unknown"),
    description: string(item.description, ""),
  };
}

function parseConfigField(value: unknown): ConfigFieldSnapshot {
  const item = record(value);
  const valueType = item.value_type === "boolean" || item.value_type === "integer" || item.value_type === "string_list" ? item.value_type : "string";
  const sensitivity = item.sensitivity === "secret" || item.sensitivity === "restricted" ? item.sensitivity : "public";
  const source = typeof item.source === "string" ? item.source as ConfigFieldSnapshot["source"] : "not_configured";
  return {
    key: string(item.key, "unknown"),
    module: string(item.module, "unknown"),
    valueType,
    source,
    overridden: item.overridden === true,
    editable: item.editable === true,
    configured: item.configured === true,
    valid: item.valid === true,
    revision: nullableString(item.revision),
    sensitivity,
    applyMode: item.apply_mode === "immediate" ? "immediate" : "restart",
    savedValue: item.saved_value,
    effectiveValue: item.effective_value,
    runningValue: item.running_value,
    pendingRestart: item.pending_restart === true,
  };
}

function parseProvider(value: unknown): ProviderStatus {
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

function parsePlatform(value: unknown): PlatformStatus {
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

function parseCapabilityScope(value: unknown): CapabilityScopeStatus {
  const item = record(value);
  return {
    id: string(item.id, "unknown"),
    label: string(item.label, "未知作用域"),
    enabled: item.enabled === true,
    capabilities: parseDirectionalCapabilities(item.capabilities),
  };
}

function parseCapabilities(value: unknown): CapabilityStatus {
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

function parseDirectionalCapabilities(value: unknown): DirectionalCapabilityStatus {
  const item = record(value);
  return {
    inbound: parseCapabilities(item.inbound),
    outbound: parseCapabilities(item.outbound),
  };
}

function parseStorage(value: unknown): StorageStatus {
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

function parseConfiguration(value: unknown): ConfigurationStatus {
  const item = record(value);
  return {
    listen: string(item.listen, "unknown"),
    corsAllowlistConfigured: item.cors_allowlist_configured === true,
    rssEnabled: item.rss_enabled === true,
    toolCallingEnabled: item.tool_calling_enabled === true,
  };
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function array(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function string(value: unknown, fallback: string): string {
  return typeof value === "string" && value.length > 0 ? value : fallback;
}

function nullableString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function nullableBoolean(value: unknown): boolean | null {
  return typeof value === "boolean" ? value : null;
}

function finiteNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function runtimeState(value: unknown): RuntimeState {
  return value === "online" || value === "offline" || value === "available" || value === "not_available" || value === "not_configured"
    ? value
    : "unknown";
}

function valueState(value: unknown): ValueState {
  return value === "supported" || value === "disabled" || value === "unsupported" || value === "not_available" || value === "not_configured"
    ? value
    : "unknown";
}
