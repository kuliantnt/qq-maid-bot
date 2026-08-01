import {
  AUTH_ROUTES,
  CONFIGURATION_ROUTES,
  MARKDOWN_RENDER_ROUTE,
  RESTART_ROUTE,
  STATUS_ROUTE,
  TODO_ROUTES,
  USER_DATA_ROUTES,
} from "./api-routes.js";
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
  TodoItem,
  TodoPage,
  TodoTargetOption,
  TodoTargetPage,
  TodoStatus,
  UserFile,
  UserPreferences,
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
  const payload = record(await fetchJson(AUTH_ROUTES.session, {
    headers: { Accept: "application/json" },
  }));
  const session = parseSession(payload.session);
  setCsrfToken(session.csrfToken);
  return session;
}

export async function fetchBootstrap(): Promise<BootstrapStatus> {
  const payload = record(await fetchJson(AUTH_ROUTES.bootstrap, {
    headers: { Accept: "application/json" },
  }));
  return parseBootstrapStatus(payload.bootstrap);
}

export async function issuePreAuth(): Promise<string> {
  const payload = record(await mutatingJson(AUTH_ROUTES.preauth, "POST"));
  const token = string(payload.csrf_token, "");
  if (!token) throw new ConsoleApiError("认证服务未返回 CSRF token", "invalid_response");
  setCsrfToken(token);
  return token;
}

export async function initializeAdmin(username: string, password: string, bootstrapToken: string): Promise<AdminSession> {
  const payload = record(await mutatingJson(AUTH_ROUTES.initialize, "POST", {
    username,
    password,
    bootstrap_token: bootstrapToken,
  }));
  const session = parseSession(payload.session);
  setCsrfToken(session.csrfToken);
  return session;
}

export async function requestPasswordReset(): Promise<BootstrapStatus> {
  const payload = record(await mutatingJson(AUTH_ROUTES.passwordResetBootstrap, "POST"));
  return parseBootstrapStatus(payload.bootstrap);
}

export async function resetAdminPassword(password: string, bootstrapToken: string): Promise<AdminSession> {
  const payload = record(await mutatingJson(AUTH_ROUTES.passwordReset, "POST", {
    password,
    bootstrap_token: bootstrapToken,
  }));
  const session = parseSession(payload.session);
  setCsrfToken(session.csrfToken);
  return session;
}

export async function loginAdmin(username: string, password: string): Promise<AdminSession> {
  const payload = record(await mutatingJson(AUTH_ROUTES.login, "POST", { username, password }));
  const session = parseSession(payload.session);
  setCsrfToken(session.csrfToken);
  return session;
}

export async function logoutAdmin(): Promise<void> {
  await mutatingJson(AUTH_ROUTES.logout, "POST", undefined, true);
  setCsrfToken("");
}

export async function fetchUserPreferences(): Promise<UserPreferences> {
  const payload = record(await mutatingJson(USER_DATA_ROUTES.preferencesGet, "POST", {}));
  return parseUserPreferences(payload.data);
}

export async function updateUserPreferences(patch: {
  readonly customColors?: readonly string[];
  readonly backgroundFileIds?: readonly string[];
  readonly activeBackgroundFileId?: string | null;
  readonly backgroundMode?: "default" | "special";
  readonly kuliantnt?: boolean;
}): Promise<UserPreferences> {
  const payload = record(await mutatingJson(USER_DATA_ROUTES.preferencesUpdate, "POST", {
    ...(patch.customColors === undefined ? {} : { custom_colors: patch.customColors }),
    ...(patch.backgroundFileIds === undefined ? {} : { background_file_ids: patch.backgroundFileIds }),
    ...(patch.activeBackgroundFileId === undefined ? {} : { active_background_file_id: patch.activeBackgroundFileId }),
    ...(patch.backgroundMode === undefined ? {} : { background_mode: patch.backgroundMode }),
    ...(patch.kuliantnt === undefined ? {} : { kuliantnt: patch.kuliantnt }),
  }));
  return parseUserPreferences(payload.data);
}

export interface UserFilePageData {
  readonly items: readonly UserFile[];
  readonly page: number;
  readonly pageSize: number;
  readonly total: number;
  readonly totalPages: number;
}

/** 按文件列表分页元数据完整收集全部用户文件，避免假设用户文件最多一页（100 条）。 */
export async function collectAllUserFiles(
  fetchPage: (page: number) => Promise<UserFilePageData>,
): Promise<readonly UserFile[]> {
  const collected: UserFile[] = [];
  let page = 1;
  while (true) {
    const current = await fetchPage(page);
    collected.push(...current.items);
    const totalPages = Math.max(current.totalPages, Math.ceil(current.total / Math.max(current.pageSize, 1)));
    if (page >= totalPages) return collected;
    page += 1;
  }
}

export async function listUserFiles(): Promise<readonly UserFile[]> {
  return collectAllUserFiles(async (page) => {
    const payload = record(await mutatingJson(USER_DATA_ROUTES.filesList, "POST", {
      page,
      page_size: 100,
    }));
    const data = record(payload.data);
    return {
      items: array(data.items).map(parseUserFile),
      page: finiteNumber(data.page) ?? 1,
      pageSize: finiteNumber(data.page_size) ?? 100,
      total: finiteNumber(data.total) ?? 0,
      totalPages: finiteNumber(data.total_pages) ?? 1,
    };
  });
}

export async function uploadUserFile(file: File): Promise<UserFile> {
  const response = await fetch(USER_DATA_ROUTES.filesUpload, {
    method: "POST",
    credentials: "same-origin",
    headers: { Accept: "application/json", "X-CSRF-Token": csrfToken },
    body: (() => { const form = new FormData(); form.append("file", file); return form; })(),
  });
  if (!response.ok) throw new ConsoleApiError(`文件上传失败（HTTP ${response.status}）`, "request_failed", response.status);
  const payload = record(await response.json() as unknown);
  return parseUserFile(payload.data);
}

export async function readUserFile(file: UserFile): Promise<Blob> {
  const response = await fetch(file.url, {
    method: "POST",
    credentials: "same-origin",
    headers: { "X-CSRF-Token": csrfToken },
  });
  if (!response.ok) throw new ConsoleApiError(`文件读取失败（HTTP ${response.status}）`, "request_failed", response.status);
  return response.blob();
}

export async function deleteUserFile(fileId: string): Promise<void> {
  await mutatingJson(USER_DATA_ROUTES.filesDelete, "POST", { file_id: fileId });
}

export async function fetchConfiguration(): Promise<ConfigurationSnapshot> {
  const payload = record(await fetchJson(CONFIGURATION_ROUTES.get, {
    headers: { Accept: "application/json" },
  }));
  return parseConfigurationPayload(payload);
}

export async function updateRuntimeConfiguration(expectedRevision: string, changes: unknown[]): Promise<ConfigurationSnapshot> {
  const payload = record(await mutatingJson(CONFIGURATION_ROUTES.runtime, "PATCH", {
    expected_revision: expectedRevision,
    changes,
  }));
  return parseConfigurationPayload(payload);
}

export async function updateSecretConfiguration(changes: unknown[]): Promise<ConfigurationSnapshot> {
  const payload = record(await mutatingJson(CONFIGURATION_ROUTES.secrets, "PATCH", { changes }));
  return parseConfigurationPayload(payload);
}

export async function updateAgentConfiguration(expectedRevision: string, changes: unknown[]): Promise<ConfigurationSnapshot> {
  const payload = record(await mutatingJson(CONFIGURATION_ROUTES.agent, "PATCH", {
    expected_revision: expectedRevision,
    changes,
  }));
  return parseConfigurationPayload(payload);
}

export async function requestRestart(): Promise<string> {
  const payload = record(await mutatingJson(RESTART_ROUTE, "POST", {}));
  return string(payload.message, "重启命令已提交");
}

export async function validateConfiguration(): Promise<{ valid: boolean; message: string }> {
  const payload = record(await mutatingJson(CONFIGURATION_ROUTES.validate, "POST", {}));
  const validation = record(payload.validation);
  return { valid: validation.valid === true, message: string(validation.message, "配置校验已完成") };
}

export async function fetchConsoleStatus(): Promise<ConsoleStatus> {
  const value = await fetchJson(STATUS_ROUTE, { headers: { Accept: "application/json" } });
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
  const value = await fetchJson(MARKDOWN_RENDER_ROUTE, {
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

export async function listTodos(filters: Record<string, unknown> = {}): Promise<TodoPage> {
  const payload = record(await mutatingJson(TODO_ROUTES.list, "POST", {
    page: 1,
    page_size: 50,
    ...filters,
  }));
  return parseTodoPage(payload.data);
}

export async function listTodoTargets(page = 1, pageSize = 100): Promise<TodoTargetPage> {
  const payload = record(await mutatingJson(TODO_ROUTES.targets, "POST", {
    page,
    page_size: pageSize,
  }));
  return parseTodoTargetPage(payload.data);
}

export async function createTodo(input: Record<string, unknown>): Promise<TodoItem> {
  const payload = record(await mutatingJson(TODO_ROUTES.create, "POST", input));
  return parseTodoItem(payload.data);
}

export async function getTodo(id: string): Promise<TodoItem> {
  const payload = record(await mutatingJson(TODO_ROUTES.get, "POST", { id }));
  return parseTodoItem(payload.data);
}

export async function updateTodo(id: string, changes: Record<string, unknown>): Promise<TodoItem> {
  const payload = record(await mutatingJson(TODO_ROUTES.update, "POST", { id, ...changes }));
  return parseTodoItem(payload.data);
}

export async function deleteTodo(id: string): Promise<void> {
  await mutatingJson(TODO_ROUTES.delete, "POST", { id });
}

function parseTodoPage(value: unknown): TodoPage {
  const data = record(value);
  return {
    items: array(data.items).map(parseTodoItem),
    page: finiteNumber(data.page) ?? 1,
    pageSize: finiteNumber(data.page_size) ?? 50,
    total: finiteNumber(data.total) ?? 0,
    totalPages: finiteNumber(data.total_pages) ?? 1,
  };
}

function parseUserPreferences(value: unknown): UserPreferences {
  const item = record(value);
  return {
    customColors: array(item.custom_colors).filter((entry): entry is string => typeof entry === "string"),
    backgroundFileIds: array(item.background_file_ids).filter((entry): entry is string => typeof entry === "string"),
    activeBackgroundFileId: nullableString(item.active_background_file_id),
    backgroundMode: item.background_mode === "special" ? "special" : "default",
    kuliantnt: item.kuliantnt === true,
  };
}

function parseUserFile(value: unknown): UserFile {
  const item = record(value);
  return {
    fileId: string(item.file_id, ""),
    filename: string(item.filename, "未命名文件"),
    contentType: string(item.content_type, "application/octet-stream"),
    size: finiteNumber(item.size) ?? 0,
    createdAt: string(item.created_at, ""),
    url: string(item.url, ""),
  };
}

function parseTodoItem(value: unknown): TodoItem {
  const item = record(value);
  const target = record(item.target);
  return {
    id: string(item.id, ""),
    title: string(item.title, "未命名 Todo"),
    detail: nullableString(item.detail),
    dueDate: nullableString(item.due_date),
    dueAt: nullableString(item.due_at),
    reminderAt: nullableString(item.reminder_at),
    timePrecision: string(item.time_precision, "none"),
    recurrenceKind: string(item.recurrence_kind, "none"),
    recurrenceIntervalDays: finiteNumber(item.recurrence_interval_days) ?? 0,
    recurrenceInterval: finiteNumber(item.recurrence_interval) ?? 0,
    recurrenceUnit: string(item.recurrence_unit, "day"),
    status: item.status === "completed" ? "completed" : "pending",
    createdAt: string(item.created_at, ""),
    updatedAt: string(item.updated_at, ""),
    completedAt: nullableString(item.completed_at),
    target: {
      targetRef: nullableString(target.target_ref),
      platform: string(target.platform, "unknown"),
      scopeType: string(target.scope_type, "unknown"),
      userId: nullableString(target.user_id),
      groupId: nullableString(target.group_id),
      accountId: nullableString(target.account_id),
      reminderSupported: target.reminder_supported === true,
      diagnostic: nullableString(target.diagnostic),
    },
  };
}

function parseTodoTargetOption(value: unknown): TodoTargetOption {
  const item = record(value);
  return {
    targetRef: string(item.target_ref, ""),
    platform: string(item.platform, "unknown"),
    accountId: nullableString(item.account_id),
    scopeType: string(item.scope_type, "unknown"),
    userId: nullableString(item.user_id),
    groupId: nullableString(item.group_id),
    reminderSupported: item.reminder_supported === true,
  };
}

function parseTodoTargetPage(value: unknown): TodoTargetPage {
  const data = record(value);
  return {
    items: array(data.items).map(parseTodoTargetOption),
    page: finiteNumber(data.page) ?? 1,
    pageSize: finiteNumber(data.page_size) ?? 100,
    total: finiteNumber(data.total) ?? 0,
    totalPages: finiteNumber(data.total_pages) ?? 1,
  };
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
