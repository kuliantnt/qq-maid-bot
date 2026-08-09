import {
  AUTH_ROUTES,
  CONFIGURATION_ROUTES,
  MARKDOWN_RENDER_ROUTE,
  KNOWLEDGE_ROUTES,
  MEMORY_ROUTES,
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
  KnowledgeFileCapabilities,
  KnowledgeFileItem,
  KnowledgeFileListParams,
  KnowledgeFilePage,
  KnowledgeFileSource,
  KnowledgeFileStatus,
  MemoryConfirmation,
  MemoryCreateInput,
  MemoryItem,
  MemoryListParams,
  MemoryOperation,
  MemoryOperationCapabilities,
  MemoryPage,
  MemoryStatus,
  MemoryTargetPage,
  MemoryTargetView,
  MemoryUpdateInput,
  MemoryVersionedInput,
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
let unauthorizedHandler: (() => void) | null = null;

export function setCsrfToken(value: string): void {
  csrfToken = value;
}

/** 统一通知页面会话失效，避免各个页面分别吞掉 401 后继续显示已认证状态。 */
export function setUnauthorizedHandler(handler: (() => void) | null): void {
  unauthorizedHandler = handler;
}

function notifyUnauthorized(status: number): void {
  if (status === 401) unauthorizedHandler?.();
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
  if (!response.ok) {
    notifyUnauthorized(response.status);
    throw new ConsoleApiError(`文件上传失败（HTTP ${response.status}）`, "request_failed", response.status);
  }
  const payload = record(await response.json() as unknown);
  return parseUserFile(payload.data);
}

export async function readUserFile(file: UserFile): Promise<Blob> {
  const response = await fetch(file.url, {
    method: "POST",
    credentials: "same-origin",
    headers: { "X-CSRF-Token": csrfToken },
  });
  if (!response.ok) {
    notifyUnauthorized(response.status);
    throw new ConsoleApiError(`文件读取失败（HTTP ${response.status}）`, "request_failed", response.status);
  }
  return response.blob();
}

export async function deleteUserFile(fileId: string): Promise<void> {
  await mutatingJson(USER_DATA_ROUTES.filesDelete, "POST", { file_id: fileId });
}

export async function fetchKnowledgeCapabilities(): Promise<KnowledgeFileCapabilities> {
  const payload = record(await mutatingJson(KNOWLEDGE_ROUTES.capabilities, "POST", {}));
  const data = record(payload.data);
  return {
    supported_extensions: array(data.supported_extensions).map((value) => requiredString(value, "supported_extensions")),
    max_file_bytes: requiredFiniteNumber(data.max_file_bytes, "max_file_bytes"),
    max_filename_chars: requiredFiniteNumber(data.max_filename_chars, "max_filename_chars"),
  };
}

export async function listKnowledgeFiles(params: KnowledgeFileListParams): Promise<KnowledgeFilePage> {
  const payload = record(await mutatingJson(KNOWLEDGE_ROUTES.list, "POST", {
    page: params.page,
    page_size: params.page_size,
    search: params.search,
    ...(params.status === "all" ? {} : { status: params.status }),
    sort: params.sort,
    order: params.order,
  }));
  return parseKnowledgeFilePage(payload.data);
}

export async function uploadKnowledgeFile(file: File): Promise<KnowledgeFileItem> {
  const form = new FormData();
  form.append("file", file);
  const response = await fetch(KNOWLEDGE_ROUTES.upload, {
    method: "POST",
    credentials: "same-origin",
    headers: { Accept: "application/json", "X-CSRF-Token": csrfToken },
    body: form,
  });
  if (!response.ok) throw await responseError(response);
  return parseKnowledgeFileItem(record(await response.json() as unknown).data);
}

export async function downloadKnowledgeFile(item: Pick<KnowledgeFileItem, "file_id" | "filename">): Promise<{ blob: Blob; filename: string }> {
  if (item.file_id === null) throw new ConsoleApiError("知识库文件缺少标识", "invalid_response");
  const response = await fetch(KNOWLEDGE_ROUTES.get(item.file_id), {
    method: "POST",
    credentials: "same-origin",
    headers: { "X-CSRF-Token": csrfToken },
  });
  if (!response.ok) throw await responseError(response);
  return {
    blob: await response.blob(),
    filename: filenameFromContentDisposition(response.headers.get("Content-Disposition")) ?? item.filename,
  };
}

export function filenameFromContentDisposition(value: string | null): string | null {
  if (value === null) return null;
  const encoded = /(?:^|;)\s*filename\*=UTF-8''([^;]+)/i.exec(value);
  if (encoded?.[1] !== undefined) {
    try {
      return decodeURIComponent(encoded[1]);
    } catch {
      return null;
    }
  }
  const plain = /(?:^|;)\s*filename="([^"]+)"/i.exec(value);
  return plain?.[1] ?? null;
}

export async function deleteKnowledgeFile(fileId: string): Promise<void> {
  await mutatingJson(KNOWLEDGE_ROUTES.delete, "POST", { file_id: fileId });
}

export async function retryKnowledgeFile(fileId: string): Promise<KnowledgeFileItem> {
  const payload = record(await mutatingJson(KNOWLEDGE_ROUTES.retry, "POST", { file_id: fileId }));
  return parseKnowledgeFileItem(payload.data);
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

export async function listMemories(params: MemoryListParams): Promise<MemoryPage> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.list, "POST", {
    page: params.page,
    page_size: params.pageSize,
    ...(params.scope === "all" ? {} : { scope: params.scope }),
    ...(params.status === "all" ? {} : { status: params.status }),
    ...(params.category === "all" ? {} : { category: params.category }),
    ...(params.visibility === "all" ? {} : { visibility: params.visibility }),
    ...(params.pinned === "all" ? {} : { pinned: params.pinned === "true" }),
    ...(params.keyword ? { keyword: params.keyword } : {}),
    ...(params.platform ? { platform: params.platform } : {}),
    ...(params.accountRef ? { account_ref: params.accountRef } : {}),
    ...(params.groupRef ? { group_ref: params.groupRef } : {}),
    ...(params.subjectRef ? { subject_ref: params.subjectRef } : {}),
  }));
  return parseMemoryPage(payload.data);
}

export async function listMemoryTargets(page = 1, pageSize = 100): Promise<MemoryTargetPage> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.targets, "POST", {
    page,
    page_size: pageSize,
  }));
  return parseMemoryTargetPage(payload.data);
}

export async function getMemory(targetRef: string, memoryRef: string): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.get, "POST", {
    target_ref: targetRef,
    memory_ref: memoryRef,
  }));
  return ensureMemoryTarget(parseMemoryItem(payload.data), targetRef);
}

export async function createMemory(input: MemoryCreateInput): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.create, "POST", {
    target_ref: input.targetRef,
    content: input.content,
    category: input.category,
    visibility: input.visibility,
    pinned: input.pinned === true,
    ...(input.attributeKey === undefined ? {} : { attribute_key: input.attributeKey }),
  }));
  return parseMemoryMutation(payload.data, input.targetRef);
}

export async function updateMemory(input: MemoryUpdateInput): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.update, "POST", {
    target_ref: input.targetRef,
    memory_ref: input.memoryRef,
    expected_version: input.expectedVersion,
    patch: {
      ...(input.patch.content === undefined ? {} : { content: input.patch.content }),
      ...(input.patch.category === undefined ? {} : { category: input.patch.category }),
      ...(input.patch.visibility === undefined ? {} : { visibility: input.patch.visibility }),
      ...(input.patch.pinned === undefined ? {} : { pinned: input.patch.pinned }),
      ...(input.patch.attributeKey === undefined ? {} : { attribute_key: input.patch.attributeKey }),
    },
  }));
  return parseMemoryMutation(payload.data, input.targetRef);
}

export async function archiveMemory(input: MemoryVersionedInput): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.archive, "POST", {
    target_ref: input.targetRef,
    memory_ref: input.memoryRef,
    expected_version: input.expectedVersion,
  }));
  return parseMemoryMutation(payload.data, input.targetRef);
}

export async function restoreMemory(input: MemoryVersionedInput): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.restore, "POST", {
    target_ref: input.targetRef,
    memory_ref: input.memoryRef,
    expected_version: input.expectedVersion,
  }));
  return parseMemoryMutation(payload.data, input.targetRef);
}

export async function prepareMemoryOperation(input: {
  readonly operation: MemoryOperation;
  readonly targetRef: string;
}): Promise<MemoryConfirmation> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.prepare, "POST", {
    operation: input.operation,
    target_ref: input.targetRef,
  }));
  const confirmation = parseMemoryConfirmation(payload.data);
  if (confirmation.operation !== input.operation || confirmation.target.targetRef !== input.targetRef) {
    throw new ConsoleApiError("Memory 确认接口返回了不匹配的操作范围", "invalid_response");
  }
  return confirmation;
}

export async function commitMemoryOperation(input: {
  readonly operation: MemoryOperation;
  readonly targetRef: string;
  readonly confirmationToken: string;
}): Promise<{ affectedCount: number; capabilities: MemoryOperationCapabilities }> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.commit, "POST", {
    operation: input.operation,
    target_ref: input.targetRef,
    confirmation_token: input.confirmationToken,
  }));
  const data = record(payload.data);
  if (data.operation !== input.operation || parseMemoryTarget(data.target).targetRef !== input.targetRef) {
    throw new ConsoleApiError("Memory 提交接口返回了不匹配的操作范围", "invalid_response");
  }
  return {
    affectedCount: requiredNonNegativeInteger(data.affected_count, "affected_count"),
    capabilities: parseOperationCapabilities(data.capabilities),
  };
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

function parseMemoryPage(value: unknown): MemoryPage {
  const data = record(value);
  return {
    items: array(data.items).map(parseMemoryItem),
    page: finiteNumber(data.page) ?? 1,
    pageSize: finiteNumber(data.page_size) ?? 20,
    total: finiteNumber(data.total) ?? 0,
    totalPages: finiteNumber(data.total_pages) ?? 0,
  };
}

function parseMemoryTargetPage(value: unknown): MemoryTargetPage {
  const data = record(value);
  return {
    items: array(data.items).map(parseMemoryTarget),
    page: finiteNumber(data.page) ?? 1,
    pageSize: finiteNumber(data.page_size) ?? 100,
    total: finiteNumber(data.total) ?? 0,
    totalPages: finiteNumber(data.total_pages) ?? 0,
  };
}

function parseMemoryTarget(value: unknown): MemoryTargetView {
  const target = record(value);
  const scope = memoryKindValue(target.scope);
  if (scope === null) {
    throw new ConsoleApiError("Memory 目标接口返回了无效范围", "invalid_response");
  }
  return {
    targetRef: requiredString(target.target_ref, "target_ref"),
    scope,
    platform: requiredString(target.platform, "platform"),
    accountRef: requiredString(target.account_ref, "account_ref"),
    groupRef: nullableString(target.group_ref),
    subjectRef: nullableString(target.subject_ref),
  };
}

function parseMemoryItem(value: unknown): MemoryItem {
  const item = record(value);
  const kind = memoryKindValue(item.kind);
  const status = memoryStatusValue(item.status);
  const category = memoryCategoryValue(item.category);
  const visibility = memoryVisibilityValue(item.visibility);
  const sourceType = memorySourceTypeValue(item.source_type);
  if (kind === null || status === null || category === null || visibility === null || sourceType === null) {
    throw new ConsoleApiError("Memory 管理接口返回了无效范围或状态", "invalid_response");
  }
  return {
    memoryRef: requiredString(item.memory_ref, "memory_ref"),
    target: parseMemoryTarget(item.target),
    version: requiredPositiveInteger(item.version, "version"),
    content: requiredString(item.content, "content"),
    kind,
    category,
    visibility,
    status,
    pinned: requiredBoolean(item.pinned, "pinned"),
    createdAt: requiredString(item.created_at, "created_at"),
    updatedAt: nullableString(item.updated_at),
    lastConfirmedAt: nullableString(item.last_confirmed_at),
    sourceType,
    capabilities: parseMemoryCapabilities(item.capabilities),
  };
}

function parseMemoryMutation(value: unknown, targetRef: string): MemoryItem {
  return ensureMemoryTarget(parseMemoryItem(record(value).memory), targetRef);
}

function ensureMemoryTarget(item: MemoryItem, targetRef: string): MemoryItem {
  if (item.target.targetRef !== targetRef) {
    throw new ConsoleApiError("Memory 接口返回了不匹配的操作范围", "invalid_response");
  }
  return item;
}

function parseMemoryConfirmation(value: unknown): MemoryConfirmation {
  const data = record(value);
  const operation = data.operation;
  if (operation !== "clear_target" && operation !== "disable_group_profile") {
    throw new ConsoleApiError("Memory 确认操作无效", "invalid_response");
  }
  return {
    confirmationToken: requiredString(data.confirmation_token, "confirmation_token"),
    operation,
    target: parseMemoryTarget(data.target),
    affectedCount: requiredNonNegativeInteger(data.affected_count, "affected_count"),
    expiresAt: requiredFiniteNumber(data.expires_at, "expires_at"),
  };
}

function parseMemoryCapabilities(value: unknown): MemoryItem["capabilities"] {
  const data = record(value);
  return {
    canUpdate: requiredBoolean(data.can_update, "capabilities.can_update"),
    canArchive: requiredBoolean(data.can_archive, "capabilities.can_archive"),
    canRestore: requiredBoolean(data.can_restore, "capabilities.can_restore"),
  };
}

function parseOperationCapabilities(value: unknown): MemoryOperationCapabilities {
  const data = record(value);
  const canClearTarget = requiredBoolean(data.can_clear_target, "capabilities.can_clear_target");
  const canDisableGroupProfile = requiredBoolean(data.can_disable_group_profile, "capabilities.can_disable_group_profile");
  return {
    canClearTarget,
    canDisableGroupProfile,
  };
}

function memoryKindValue(value: unknown): MemoryItem["kind"] | null {
  return value === "personal" || value === "group_profile" || value === "group" ? value : null;
}

function memoryStatusValue(value: unknown): MemoryStatus | null {
  return value === "active" || value === "archived" ? value : null;
}

function memoryCategoryValue(value: unknown): MemoryItem["category"] | null {
  return value === "note" || value === "preference" || value === "identity" || value === "relation" || value === "instruction" ? value : null;
}

function memoryVisibilityValue(value: unknown): MemoryItem["visibility"] | null {
  return value === "private" || value === "context_only" || value === "group_members" || value === "public" ? value : null;
}

function memorySourceTypeValue(value: unknown): MemoryItem["sourceType"] | null {
  return value === "user_confirmed" || value === "manual_import" || value === "system_derived" || value === "legacy" ? value : null;
}

function parseKnowledgeFilePage(value: unknown): KnowledgeFilePage {
  const data = record(value);
  return {
    items: array(data.items).map(parseKnowledgeFileItem),
    page: finiteNumber(data.page) ?? 1,
    page_size: finiteNumber(data.page_size) ?? 20,
    total: finiteNumber(data.total) ?? 0,
    total_pages: finiteNumber(data.total_pages) ?? 1,
  };
}

export function parseKnowledgeFileItem(value: unknown): KnowledgeFileItem {
  const item = record(value);
  const source = item.source;
  const status = item.status;
  if (source !== "managed" && source !== "directory") throw new ConsoleApiError("知识库文件来源无效", "invalid_response");
  if (status !== "pending" && status !== "processing" && status !== "ready" && status !== "failed") {
    throw new ConsoleApiError("知识库文件状态无效", "invalid_response");
  }
  return {
    file_id: nullableString(item.file_id),
    filename: requiredString(item.filename, "filename"),
    content_type: requiredString(item.content_type, "content_type"),
    size: finiteNumber(item.size),
    source: source satisfies KnowledgeFileSource,
    source_label: requiredString(item.source_label, "source_label"),
    status: status satisfies KnowledgeFileStatus,
    uploaded_at: nullableString(item.uploaded_at),
    processing_started_at: nullableString(item.processing_started_at),
    processed_at: nullableString(item.processed_at),
    updated_at: requiredString(item.updated_at, "updated_at"),
    error_code: nullableString(item.error_code),
    error_summary: nullableString(item.error_summary),
    chunk_count: finiteNumber(item.chunk_count),
    embedding_count: finiteNumber(item.embedding_count),
    downloadable: requiredBoolean(item.downloadable, "downloadable"),
    download_url: nullableString(item.download_url),
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
    notifyUnauthorized(response.status);
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
    notifyUnauthorized(response.status);
    throw new ConsoleApiError(message, code, response.status);
  }
  return await response.json() as unknown;
}

async function responseError(response: Response): Promise<ConsoleApiError> {
  let code = "request_failed";
  let message = `管理接口请求失败（HTTP ${response.status}）`;
  try {
    const payload = record(await response.json() as unknown);
    const error = record(payload.error);
    code = string(error.code, code);
    message = string(error.message, message);
  } catch { /* 保留稳定错误。 */ }
  notifyUnauthorized(response.status);
  return new ConsoleApiError(message, code, response.status);
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

function requiredString(value: unknown, field: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new ConsoleApiError(`管理接口返回了无效 ${field}`, "invalid_response");
  }
  return value;
}

function requiredBoolean(value: unknown, field: string): boolean {
  if (typeof value !== "boolean") throw new ConsoleApiError(`管理接口返回了无效 ${field}`, "invalid_response");
  return value;
}

function requiredFiniteNumber(value: unknown, field: string): number {
  const number = finiteNumber(value);
  if (number === null) throw new ConsoleApiError(`管理接口返回了无效 ${field}`, "invalid_response");
  return number;
}

function requiredPositiveInteger(value: unknown, field: string): number {
  const number = requiredFiniteNumber(value, field);
  if (!Number.isInteger(number) || number <= 0) {
    throw new ConsoleApiError(`管理接口返回了无效 ${field}`, "invalid_response");
  }
  return number;
}

function requiredNonNegativeInteger(value: unknown, field: string): number {
  const number = requiredFiniteNumber(value, field);
  if (!Number.isInteger(number) || number < 0) {
    throw new ConsoleApiError(`管理接口返回了无效 ${field}`, "invalid_response");
  }
  return number;
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
