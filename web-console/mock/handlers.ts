/**
 * Web Console dev mock 的 API 路由实现。
 *
 * 覆盖 src/api-routes.ts 与 src/api.ts 中的全部后端端点；响应形状与真实后端契约一致，
 * 供 src/api.ts / src/memory-api.ts 的解析函数直接消费。
 * Todo / Memory / 知识库 / 偏好 / 配置均为有状态内存实现，页面上可以真实增删改查；
 * 会话默认直接放行（MOCK_AUTH=gate 时改为 401，用于预览登录/初始化表单流程）。
 */
import type { IncomingMessage, ServerResponse } from "node:http";
import {
  agentDocument,
  agentRevision,
  bumpAgentRevision,
  bumpConfigRevision,
  bootstrapPayload,
  configFields,
  configRevision,
  consoleStatusPayload,
  knowledgeFiles,
  memories,
  nowIso,
  pendingMemoryOperations,
  preferences,
  providerAdapters,
  providerCredentials,
  providerPresets,
  registeredTools,
  sessionPayload,
  todoTargets,
  todos,
  userFiles,
  addUserFile,
  placeholderSvg,
  type MockKnowledgeFile,
  type MockMemory,
  type MockMemoryTarget,
  type MockTodo,
} from "./state.js";

/** 每个请求的固定延迟：让 TanStack Query 的加载态可见，模拟真实网络节奏。 */
const LATENCY_MS = 80;

export interface MockRequestContext {
  body: Record<string, unknown>;
  params: Record<string, string>;
  /** 原始请求体：multipart 上传需要自行解析文件名与大小。 */
  raw: Buffer;
}

type HandlerResult =
  | { kind: "json"; status: number; payload: unknown }
  | { kind: "binary"; status: number; contentType: string; body: Buffer; filename: string };

type Handler = (ctx: MockRequestContext) => HandlerResult | Promise<HandlerResult>;

const json = (payload: unknown, status = 200): HandlerResult => ({ kind: "json", status, payload });
const apiError = (status: number, code: string, message: string): HandlerResult =>
  json({ error: { code, message } }, status);

/* --------------------------------- 通用分页 -------------------------------- */

function paginate<T>(items: T[], body: Record<string, unknown>): { items: T[]; page: number; pageSize: number; total: number; totalPages: number } {
  const page = Math.max(1, typeof body.page === "number" ? Math.floor(body.page) : 1);
  const pageSize = Math.max(1, typeof body.page_size === "number" ? Math.floor(body.page_size) : 50);
  const total = items.length;
  const totalPages = Math.max(1, Math.ceil(total / pageSize));
  const start = (page - 1) * pageSize;
  return { items: items.slice(start, start + pageSize), page, pageSize, total, totalPages };
}

function text(body: Record<string, unknown>, key: string): string {
  return typeof body[key] === "string" ? (body[key] as string) : "";
}

/* ---------------------------------- 认证 ---------------------------------- */

/** MOCK_AUTH=gate 时会话返回 401，用于预览登录/初始化/密码重置表单。 */
function gateMode(): boolean {
  return process.env.MOCK_AUTH === "gate";
}

function handleSession(): HandlerResult {
  if (gateMode()) {
    return apiError(401, "unauthorized", "mock gate 模式：会话未建立");
  }
  return json(sessionPayload());
}

function handleLogin(ctx: MockRequestContext): HandlerResult {
  const username = text(ctx.body, "username") || "admin";
  return json(sessionPayload(username));
}

const handleBootstrap = (): HandlerResult => json(bootstrapPayload);
const handlePreauth = (): HandlerResult => json({ csrf_token: "mock-csrf-token" });

/* -------------------------------- 用户偏好 -------------------------------- */

function handlePreferencesGet(): HandlerResult {
  return json({ data: { ...preferences } });
}

function handlePreferencesUpdate(ctx: MockRequestContext): HandlerResult {
  const body = ctx.body;
  if (Array.isArray(body.custom_colors)) preferences.custom_colors = body.custom_colors.map(String);
  if (Array.isArray(body.background_file_ids)) preferences.background_file_ids = body.background_file_ids.map(String);
  if ("active_background_file_id" in body) {
    preferences.active_background_file_id = typeof body.active_background_file_id === "string" ? body.active_background_file_id : null;
  }
  if (body.background_mode === "default" || body.background_mode === "special") preferences.background_mode = body.background_mode;
  if (typeof body.kuliantnt === "boolean") preferences.kuliantnt = body.kuliantnt;
  return json({ data: { ...preferences } });
}

/* -------------------------------- 用户文件 -------------------------------- */

function handleFileList(ctx: MockRequestContext): HandlerResult {
  const view = paginate(userFiles, ctx.body);
  return json({
    data: {
      items: view.items,
      page: view.page,
      page_size: view.pageSize,
      total: view.total,
      total_pages: view.totalPages,
    },
  });
}

/** 从 multipart body 提取上传文件名；mock 不落盘，只登记元数据。 */
function filenameFromMultipart(raw: Buffer): { filename: string; size: number } {
  const text = raw.toString("utf8");
  const utf8 = /filename\*=UTF-8''([^;\r\n"]+)/i.exec(text);
  const plain = /filename="([^"]*)"/i.exec(text);
  const filename = utf8?.[1]
    ? safeDecode(utf8[1])
    : plain?.[1] ?? "mock-upload.svg";
  return { filename: filename || "mock-upload.svg", size: raw.length };
}

function safeDecode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function handleFileUpload(ctx: MockRequestContext): HandlerResult {
  const { filename } = filenameFromMultipart(ctx.raw);
  return json({ data: addUserFile(filename, ctx.raw.length) });
}

function handleFileDelete(ctx: MockRequestContext): HandlerResult {
  const fileId = text(ctx.body, "file_id");
  const index = userFiles.findIndex((file) => file.file_id === fileId);
  if (index >= 0) userFiles.splice(index, 1);
  if (preferences.active_background_file_id === fileId) preferences.active_background_file_id = null;
  preferences.background_file_ids = preferences.background_file_ids.filter((id) => id !== fileId);
  return json({ data: { deleted: true } });
}

/* --------------------------------- 知识库 --------------------------------- */

function handleKnowledgeCapabilities(): HandlerResult {
  return json({
    data: {
      supported_extensions: [".md", ".txt", ".pdf", ".html", ".docx"],
      max_file_bytes: 20 * 1024 * 1024,
      max_filename_chars: 120,
    },
  });
}

function handleKnowledgeList(ctx: MockRequestContext): HandlerResult {
  const search = text(ctx.body, "search").trim().toLowerCase();
  const status = text(ctx.body, "status");
  const sort = text(ctx.body, "sort") === "uploaded_at" ? "uploaded_at" : "updated_at";
  const order = text(ctx.body, "order") === "asc" ? 1 : -1;
  const filtered = knowledgeFiles
    .filter((file) => (status && status !== "all" ? file.status === status : true))
    .filter((file) => (search ? file.filename.toLowerCase().includes(search) : true))
    .sort((left, right) => (left[sort] ?? "").localeCompare(right[sort] ?? "") * order);
  const view = paginate(filtered, ctx.body);
  return json({
    data: {
      items: view.items,
      page: view.page,
      page_size: view.pageSize,
      total: view.total,
      total_pages: view.totalPages,
    },
  });
}

function handleKnowledgeUpload(ctx: MockRequestContext): HandlerResult {
  const { filename } = filenameFromMultipart(ctx.raw);
  const extension = filename.slice(filename.lastIndexOf(".")).toLowerCase();
  const contentTypes: Record<string, string> = {
    ".md": "text/markdown",
    ".txt": "text/plain",
    ".pdf": "application/pdf",
    ".html": "text/html",
  };
  const item: MockKnowledgeFile = {
    file_id: `kfile-upload-${Date.now()}`,
    filename,
    content_type: contentTypes[extension] ?? "application/octet-stream",
    size: ctx.raw.length,
    source: "managed",
    source_label: "控制台上传",
    status: "processing",
    uploaded_at: nowIso(),
    processing_started_at: nowIso(),
    processed_at: null,
    updated_at: nowIso(),
    error_code: null,
    error_summary: null,
    chunk_count: null,
    embedding_count: null,
    downloadable: false,
    download_url: null,
  };
  knowledgeFiles.unshift(item);
  // 模拟真实异步处理：4 秒后转 ready 并填充分片统计。
  setTimeout(() => {
    item.status = "ready";
    item.processed_at = nowIso();
    item.chunk_count = 12;
    item.embedding_count = 12;
    item.downloadable = true;
    item.updated_at = nowIso();
  }, 4000).unref();
  return json({ data: item });
}

function handleKnowledgeDelete(ctx: MockRequestContext): HandlerResult {
  const fileId = text(ctx.body, "file_id");
  const index = knowledgeFiles.findIndex((file) => file.file_id === fileId);
  if (index >= 0) knowledgeFiles.splice(index, 1);
  return json({ data: { deleted: true } });
}

function handleKnowledgeRetry(ctx: MockRequestContext): HandlerResult {
  const fileId = text(ctx.body, "file_id");
  const file = knowledgeFiles.find((entry) => entry.file_id === fileId);
  if (!file) return apiError(404, "not_found", `知识库文件 ${fileId} 不存在`);
  file.status = "processing";
  file.error_code = null;
  file.error_summary = null;
  setTimeout(() => {
    file.status = "ready";
    file.chunk_count = 9;
    file.embedding_count = 9;
    file.downloadable = true;
    file.updated_at = nowIso();
  }, 3000).unref();
  return json({ data: file });
}

/** 知识库下载：返回一份 UTF-8 文本 blob，保证浏览器保存与预览可用。 */
function handleKnowledgeDownload(params: Record<string, string>): HandlerResult {
  const file = knowledgeFiles.find((entry) => entry.file_id === params.fileId);
  if (!file) return apiError(404, "not_found", `知识库文件 ${params.fileId ?? ""} 不存在`);
  const body = Buffer.from(`# ${file.filename}\n\n这是 Web Console dev mock 生成的占位内容。\n`, "utf8");
  return { kind: "binary", status: 200, contentType: "text/markdown; charset=utf-8", body, filename: file.filename };
}

/* ---------------------------------- Todo ---------------------------------- */

function findTodo(id: string): MockTodo | undefined {
  return todos.find((todo) => todo.id === id);
}

function handleTodoList(ctx: MockRequestContext): HandlerResult {
  const status = text(ctx.body, "status");
  const keyword = text(ctx.body, "keyword").trim().toLowerCase();
  const filtered = todos
    .filter((todo) => (status && status !== "all" ? todo.status === status : true))
    .filter((todo) => (keyword ? `${todo.title} ${todo.detail ?? ""}`.toLowerCase().includes(keyword) : true));
  const view = paginate(filtered, ctx.body);
  return json({
    data: {
      items: view.items,
      page: view.page,
      page_size: view.pageSize,
      total: view.total,
      total_pages: view.totalPages,
    },
  });
}

function handleTodoTargets(ctx: MockRequestContext): HandlerResult {
  const view = paginate(todoTargets, ctx.body);
  return json({
    data: {
      items: view.items,
      page: view.page,
      page_size: view.pageSize,
      total: view.total,
      total_pages: view.totalPages,
    },
  });
}

function handleTodoCreate(ctx: MockRequestContext): HandlerResult {
  const targetRef = text(ctx.body, "target_ref");
  const target = todoTargets.find((entry) => entry.target_ref === targetRef) ?? todoTargets[0]!;
  const now = nowIso();
  const interval = typeof ctx.body.recurrence_interval === "number" ? ctx.body.recurrence_interval : null;
  const todo: MockTodo = {
    id: `todo-${Date.now()}`,
    title: text(ctx.body, "title") || "未命名 Todo",
    detail: text(ctx.body, "detail") || null,
    due_date: text(ctx.body, "due_date") || null,
    due_at: text(ctx.body, "due_at") || null,
    reminder_at: text(ctx.body, "reminder_at") || null,
    time_precision: text(ctx.body, "time_precision") || "none",
    recurrence_kind: text(ctx.body, "recurrence_kind") || "none",
    recurrence_interval_days: interval ?? 0,
    recurrence_interval: interval,
    recurrence_unit: text(ctx.body, "recurrence_unit") || "day",
    status: "pending",
    created_at: now,
    updated_at: now,
    completed_at: null,
    target,
  };
  todos.unshift(todo);
  return json({ data: todo });
}

function handleTodoGet(ctx: MockRequestContext): HandlerResult {
  const todo = findTodo(text(ctx.body, "id"));
  return todo ? json({ data: todo }) : apiError(404, "not_found", "Todo 不存在或已删除");
}

function handleTodoUpdate(ctx: MockRequestContext): HandlerResult {
  const todo = findTodo(text(ctx.body, "id"));
  if (!todo) return apiError(404, "not_found", "Todo 不存在或已删除");
  const { id: _id, ...changes } = ctx.body;
  for (const [key, value] of Object.entries(changes)) {
    if (key in todo) (todo as unknown as Record<string, unknown>)[key] = value;
  }
  todo.updated_at = nowIso();
  if (changes.status === "completed") todo.completed_at = nowIso();
  if (changes.status === "pending") todo.completed_at = null;
  return json({ data: todo });
}

function handleTodoDelete(ctx: MockRequestContext): HandlerResult {
  const index = todos.findIndex((todo) => todo.id === text(ctx.body, "id"));
  if (index < 0) return apiError(404, "not_found", "Todo 不存在或已删除");
  todos.splice(index, 1);
  return json({ data: { deleted: true } });
}

/* --------------------------------- Memory --------------------------------- */

function memoryTargets(): MockMemoryTarget[] {
  // 去重返回全部目标；真实后端按账号可见范围返回，mock 固定三个。
  const seen = new Map<string, MockMemoryTarget>();
  for (const memory of memories) seen.set(memory.target.target_ref, memory.target);
  return [...seen.values()];
}

function findMemory(targetRef: string, memoryRef: string): MockMemory | undefined {
  return memories.find((memory) => memory.memory_ref === memoryRef && memory.target.target_ref === targetRef);
}

function handleMemoryList(ctx: MockRequestContext): HandlerResult {
  const body = ctx.body;
  const scope = text(body, "scope");
  const status = text(body, "status");
  const category = text(body, "category");
  const visibility = text(body, "visibility");
  const pinned = body.pinned === true ? "true" : body.pinned === false ? "false" : "all";
  const keyword = text(body, "keyword").trim().toLowerCase();
  const filtered = memories
    .filter((memory) => (scope && scope !== "all" ? memory.kind === scope : true))
    .filter((memory) => (status && status !== "all" ? memory.status === status : true))
    .filter((memory) => (category && category !== "all" ? memory.category === category : true))
    .filter((memory) => (visibility && visibility !== "all" ? memory.visibility === visibility : true))
    .filter((memory) => (pinned === "all" ? true : pinned === "true" ? memory.pinned : !memory.pinned))
    .filter((memory) => (keyword ? memory.content.toLowerCase().includes(keyword) : true));
  const view = paginate(filtered, body);
  return json({
    data: {
      items: view.items,
      page: view.page,
      page_size: view.pageSize,
      total: view.total,
      total_pages: view.totalPages,
    },
  });
}

function handleMemoryTargets(): HandlerResult {
  const items = memoryTargets();
  return json({ data: { items, page: 1, page_size: 100, total: items.length, total_pages: 1 } });
}

function handleMemoryGet(ctx: MockRequestContext): HandlerResult {
  const memory = findMemory(text(ctx.body, "target_ref"), text(ctx.body, "memory_ref"));
  return memory ? json({ data: memory }) : apiError(404, "not_found", "Memory 不存在或已归档");
}

function handleMemoryCreate(ctx: MockRequestContext): HandlerResult {
  const targetRef = text(ctx.body, "target_ref");
  const target = memoryTargets().find((entry) => entry.target_ref === targetRef);
  if (!target) return apiError(404, "not_found", `Memory 目标 ${targetRef} 不存在`);
  const content = text(ctx.body, "content").trim();
  if (!content) return apiError(422, "invalid_argument", "Memory 内容不能为空");
  const memory: MockMemory = {
    memory_ref: `mem-${Date.now()}`,
    target,
    version: 1,
    content,
    kind: target.scope,
    category: (text(ctx.body, "category") || "note") as MockMemory["category"],
    visibility: (text(ctx.body, "visibility") || "private") as MockMemory["visibility"],
    status: "active",
    pinned: ctx.body.pinned === true,
    created_at: nowIso(),
    updated_at: null,
    last_confirmed_at: nowIso(),
    source_type: "manual_import",
    capabilities: { can_update: true, can_archive: true, can_restore: false, can_delete: true },
  };
  memories.unshift(memory);
  return json({ data: { memory } });
}

/** 版本冲突模拟：expected_version 与现值不一致时返回 409，供前端冲突分支联调。 */
function checkVersion(memory: MockMemory, body: Record<string, unknown>): HandlerResult | null {
  const expected = body.expected_version;
  if (typeof expected === "number" && expected !== memory.version) {
    return apiError(409, "version_conflict", `Memory 版本已变化（当前 v${memory.version}），请刷新后重试`);
  }
  return null;
}

function handleMemoryUpdate(ctx: MockRequestContext): HandlerResult {
  const memory = findMemory(text(ctx.body, "target_ref"), text(ctx.body, "memory_ref"));
  if (!memory) return apiError(404, "not_found", "Memory 不存在或已归档");
  const conflict = checkVersion(memory, ctx.body);
  if (conflict) return conflict;
  const patch = typeof ctx.body.patch === "object" && ctx.body.patch !== null ? ctx.body.patch as Record<string, unknown> : {};
  if (typeof patch.content === "string" && patch.content.trim()) memory.content = patch.content.trim();
  if (typeof patch.category === "string") memory.category = patch.category as MockMemory["category"];
  if (typeof patch.visibility === "string") memory.visibility = patch.visibility as MockMemory["visibility"];
  if (typeof patch.pinned === "boolean") memory.pinned = patch.pinned;
  memory.version += 1;
  memory.updated_at = nowIso();
  memory.last_confirmed_at = nowIso();
  return json({ data: { memory } });
}

function handleMemoryArchive(ctx: MockRequestContext): HandlerResult {
  return transitionMemory(ctx, "archived");
}

function handleMemoryRestore(ctx: MockRequestContext): HandlerResult {
  return transitionMemory(ctx, "active");
}

function transitionMemory(ctx: MockRequestContext, status: "active" | "archived"): HandlerResult {
  const memory = findMemory(text(ctx.body, "target_ref"), text(ctx.body, "memory_ref"));
  if (!memory) return apiError(404, "not_found", "Memory 不存在");
  const conflict = checkVersion(memory, ctx.body);
  if (conflict) return conflict;
  memory.status = status;
  memory.version += 1;
  memory.updated_at = nowIso();
  memory.capabilities = {
    can_update: status === "active",
    can_archive: status === "active",
    can_restore: status === "archived",
    can_delete: true,
  };
  return json({ data: { memory } });
}

function handleMemoryPrepare(ctx: MockRequestContext): HandlerResult {
  const operation = text(ctx.body, "operation");
  const targetRef = text(ctx.body, "target_ref");
  if (operation !== "clear_target" && operation !== "disable_group_profile" && operation !== "delete_memory") {
    return apiError(422, "invalid_argument", `未知操作 ${operation}`);
  }
  let affected = 0;
  let target = memoryTargets().find((entry) => entry.target_ref === targetRef);
  if (operation === "delete_memory") {
    const memory = findMemory(targetRef, text(ctx.body, "memory_ref"));
    if (!memory) return apiError(404, "not_found", "Memory 不存在");
    target = memory.target;
    affected = 1;
  } else {
    if (!target) return apiError(404, "not_found", `Memory 目标 ${targetRef} 不存在`);
    affected = memories.filter((memory) => memory.target.target_ref === targetRef).length;
  }
  const token = `confirm-${Date.now()}-${operation}`;
  pendingMemoryOperations.set(token, {
    operation,
    target_ref: targetRef,
    memory_ref: operation === "delete_memory" ? text(ctx.body, "memory_ref") : null,
  });
  return json({
    data: {
      confirmation_token: token,
      operation,
      target,
      affected_count: affected,
      expires_at: Math.floor(Date.now() / 1000) + 300,
    },
  });
}

function handleMemoryCommit(ctx: MockRequestContext): HandlerResult {
  const operation = text(ctx.body, "operation");
  const targetRef = text(ctx.body, "target_ref");
  const token = text(ctx.body, "confirmation_token");
  const pending = pendingMemoryOperations.get(token);
  if (!pending || pending.operation !== operation || pending.target_ref !== targetRef) {
    return apiError(409, "confirmation_invalid", "确认令牌无效或已过期，请重新发起操作");
  }
  pendingMemoryOperations.delete(token);
  const target = memoryTargets().find((entry) => entry.target_ref === targetRef);
  if (!target) return apiError(404, "not_found", `Memory 目标 ${targetRef} 不存在`);
  if (operation === "delete_memory") {
    const memoryRef = text(ctx.body, "memory_ref") || pending.memory_ref;
    const index = memories.findIndex((memory) => memory.memory_ref === memoryRef && memory.target.target_ref === targetRef);
    if (index < 0) return apiError(404, "not_found", "Memory 不存在");
    const [removed] = memories.splice(index, 1);
    return json({
      data: {
        affected_count: 1,
        capabilities: target.capabilities,
        target,
        operation,
        deleted: true,
        memory_ref: removed!.memory_ref,
      },
    });
  }
  if (operation === "clear_target") {
    const affected = memories.filter((memory) => memory.target.target_ref === targetRef).length;
    for (let index = memories.length - 1; index >= 0; index -= 1) {
      if (memories[index]?.target.target_ref === targetRef) memories.splice(index, 1);
    }
    return json({ data: { affected_count: affected, capabilities: target.capabilities, target, operation } });
  }
  // disable_group_profile：mock 只回执成功，不改变画像内容。
  return json({ data: { affected_count: 0, capabilities: target.capabilities, target, operation } });
}

/* --------------------------------- 运行状态 -------------------------------- */

const handleStatus = (): HandlerResult => json(consoleStatusPayload());

const handleRestart = (): HandlerResult =>
  json({ message: "mock 模式：重启命令已受理（不会真正重启）" });

/* ----------------------------- Markdown 渲染 ----------------------------- */

/** 极简 Markdown → HTML：覆盖标题/粗体/行内代码/链接/列表/段落，仅供工具页预览。 */
function renderMarkdownHtml(markdown: string): string {
  const escape = (value: string): string =>
    value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const inline = (value: string): string =>
    escape(value)
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/\*([^*]+)\*/g, "<em>$1</em>")
      .replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2">$1</a>');
  const lines = markdown.split(/\r?\n/);
  const html: string[] = [];
  let inList = false;
  for (const line of lines) {
    const listItem = /^\s*[-*]\s+(.*)$/.exec(line);
    if (listItem) {
      if (!inList) {
        html.push("<ul>");
        inList = true;
      }
      html.push(`<li>${inline(listItem[1] ?? "")}</li>`);
      continue;
    }
    if (inList) {
      html.push("</ul>");
      inList = false;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      const level = (heading[1] ?? "#").length;
      html.push(`<h${level}>${inline(heading[2] ?? "")}</h${level}>`);
    } else if (line.trim() === "") {
      // 空行只负责分段，不输出内容。
    } else {
      html.push(`<p>${inline(line)}</p>`);
    }
  }
  if (inList) html.push("</ul>");
  return html.join("\n");
}

function handleMarkdownRender(ctx: MockRequestContext): HandlerResult {
  const markdown = text(ctx.body, "markdown");
  return json({ ok: true, html: renderMarkdownHtml(markdown) });
}

/* --------------------------------- 配置管理 -------------------------------- */

function agentSnapshotPayload(): unknown {
  return {
    revision: agentRevision(),
    file_exists: true,
    source: "agent_toml",
    editable: true,
    read_only: false,
    pending_restart: false,
    saved_value: structuredClone(agentDocument),
    running_value: structuredClone(agentDocument),
  };
}

function configurationPayload(): unknown {
  return {
    configuration: {
      revision: configRevision(),
      file_exists: true,
      providers: {
        presets: providerPresets,
        adapters: providerAdapters,
        credentials: providerCredentials,
      },
      fields: configFields,
      agent: agentSnapshotPayload(),
    },
    registered_tools: registeredTools,
    restart: { available: true },
  };
}

const handleConfigurationGet = (): HandlerResult => json(configurationPayload());

/** 运行配置 set/clear/remove：内存直接改写字段快照，restart 字段保持 pending 态。 */
function applyRuntimeChanges(changes: unknown[]): void {
  for (const change of changes) {
    if (typeof change !== "object" || change === null) continue;
    const entry = change as Record<string, unknown>;
    const key = typeof entry.key === "string" ? entry.key : "";
    const target = configFields.find((field) => field.key === key);
    if (!target) continue;
    const revision = configRevision();
    if (entry.action === "set") {
      target.saved_value = entry.value;
      target.effective_value = entry.value;
      if (target.apply_mode === "immediate") target.running_value = entry.value;
      else target.pending_restart = true;
      target.source = "managed_toml";
      target.overridden = true;
      target.configured = true;
    } else if (entry.action === "remove" || entry.action === "clear") {
      target.saved_value = null;
      target.effective_value = null;
      target.configured = false;
      target.overridden = false;
      target.source = "not_configured";
      target.pending_restart = true;
    }
    target.revision = revision;
  }
}

function handleRuntimePatch(ctx: MockRequestContext): HandlerResult {
  applyRuntimeChanges(Array.isArray(ctx.body.changes) ? ctx.body.changes : []);
  bumpConfigRevision();
  return json(configurationPayload());
}

/** secret 变更：只翻转 configured 状态，原文永不回传。 */
function handleSecretsPatch(ctx: MockRequestContext): HandlerResult {
  const changes = Array.isArray(ctx.body.changes) ? ctx.body.changes : [];
  for (const change of changes) {
    if (typeof change !== "object" || change === null) continue;
    const entry = change as Record<string, unknown>;
    const target = configFields.find((field) => field.key === entry.key);
    if (!target) continue;
    if (entry.action === "replace") {
      target.configured = true;
      target.source = "encrypted_secret";
      target.overridden = true;
      target.saved_value = null;
      target.effective_value = "***";
      target.pending_restart = false;
    } else if (entry.action === "clear") {
      target.configured = false;
      target.source = "not_configured";
      target.overridden = false;
      target.saved_value = null;
      target.effective_value = null;
    }
  }
  bumpConfigRevision();
  return json(configurationPayload());
}

/** Agent 配置 set_provider / remove_provider / 其他动作的尽力应用。 */
function applyAgentChanges(changes: unknown[]): void {
  for (const change of changes) {
    if (typeof change !== "object" || change === null) continue;
    const entry = change as Record<string, unknown>;
    const action = typeof entry.action === "string" ? entry.action : "";
    if (action === "set_provider" && typeof entry.id === "string") {
      agentDocument.providers[entry.id] =
        typeof entry.provider === "object" && entry.provider !== null
          ? { ...(entry.provider as Record<string, unknown>) }
          : {};
    } else if (action === "remove_provider" && typeof entry.id === "string") {
      delete agentDocument.providers[entry.id];
      delete providerCredentials[entry.id];
    } else if (action === "set_knowledge" && typeof entry.mode === "string") {
      agentDocument.knowledge.mode = entry.mode;
      if (typeof entry.embedding === "object" && entry.embedding !== null) {
        const embedding = entry.embedding as Record<string, unknown>;
        agentDocument.knowledge.embedding = {
          enabled: embedding.enabled === true,
          cache_dir: typeof embedding.cache_dir === "string" ? embedding.cache_dir : agentDocument.knowledge.embedding.cache_dir,
        };
      }
    } else if (action === "set_model_route" && typeof entry.name === "string") {
      agentDocument.model_routes[entry.name] = {
        candidates: Array.isArray(entry.candidates) ? entry.candidates.map(String) : [],
      };
    } else if (action === "set_search_route" && typeof entry.name === "string") {
      agentDocument.tools.web_search.routes[entry.name] = {
        model: typeof entry.model === "string" ? entry.model : "",
      };
    } else if (action === "set_scene" && typeof entry.scene === "string") {
      const config = typeof entry.config === "object" && entry.config !== null ? entry.config as Record<string, unknown> : {};
      agentDocument.scenes[entry.scene] = {
        tool_calling_enabled: config.tool_calling_enabled === true,
        enabled_tools: Array.isArray(config.enabled_tools) ? config.enabled_tools.map(String) : [],
      };
    }
    // set_web_search 的参数已由前端校验，mock 直接覆盖对应字段。
    if (action === "set_web_search") {
      const webSearch = agentDocument.tools.web_search;
      if (typeof entry.backend === "string") webSearch.backend = entry.backend;
      if (typeof entry.max_results === "number") webSearch.max_results = entry.max_results;
      if (typeof entry.search_depth === "string") webSearch.search_depth = entry.search_depth;
      if (typeof entry.topic === "string") webSearch.topic = entry.topic;
      webSearch.time_range = typeof entry.time_range === "string" ? entry.time_range : null;
      for (const key of ["connect_timeout_seconds", "first_response_timeout_seconds", "total_timeout_seconds"] as const) {
        if (typeof entry[key] === "number") webSearch[key] = entry[key];
      }
    }
  }
  bumpAgentRevision();
}

function handleAgentPatch(ctx: MockRequestContext): HandlerResult {
  applyAgentChanges(Array.isArray(ctx.body.changes) ? ctx.body.changes : []);
  bumpConfigRevision();
  return json(configurationPayload());
}

function handleValidate(): HandlerResult {
  return json({ validation: { valid: true, message: "mock 模式：配置校验通过" } });
}

function handleCredentialPatch(ctx: MockRequestContext): HandlerResult {
  const id = text(ctx.body, "id");
  providerCredentials[id] = {
    configured: ctx.body.value !== null,
    editable: true,
    revision: `cred-rev-${Date.now()}`,
    pending_restart: ctx.body.value !== null,
  };
  bumpAgentRevision();
  bumpConfigRevision();
  return json(configurationPayload());
}

/** 连接测试：返回全链路 success 诊断；模型 ID 留空时按真实语义报失败。 */
function handleProviderTest(ctx: MockRequestContext): HandlerResult {
  const model = text(ctx.body, "model");
  if (!model) {
    return json({
      diagnostic: {
        network: "success",
        authentication: "not_tested",
        adapter: "not_tested",
        model_call: "not_tested",
        elapsed_ms: 12,
        http_status: null,
        category: "缺少测试模型 ID",
      },
    });
  }
  return json({
    diagnostic: {
      network: "success",
      authentication: "success",
      adapter: "success",
      model_call: "success",
      elapsed_ms: 640 + Math.floor(Math.random() * 400),
      http_status: 200,
      category: "openai",
    },
  });
}

function handleProviderModels(): HandlerResult {
  return json({
    discovery: {
      state: "success",
      category: "openai",
      models: [
        { id: "glm-4.7-flash" },
        { id: "glm-4.7-air" },
        { id: "glm-4.6" },
        { id: "glm-4.5" },
        { id: "embedding-3" },
      ],
    },
  });
}

function handleProviderModelMetadata(): HandlerResult {
  return json({
    metadata: {
      models: [
        { id: "glm-4.7-flash", display_name: "GLM-4.7 Flash", status: "active", enabled: true, provenance: { catalog: true } },
        { id: "glm-4.7-air", display_name: "GLM-4.7 Air", status: "active", enabled: true, provenance: { catalog: true } },
        { id: "glm-4.6", display_name: "GLM-4.6", status: "active", enabled: true, provenance: { catalog: true } },
        { id: "glm-4.5", display_name: "GLM-4.5", status: "deprecated", enabled: false, provenance: { catalog: true } },
        { id: "embedding-3", display_name: "Embedding-3", status: "active", enabled: true, provenance: { catalog: true } },
      ],
      overrides: [],
    },
  });
}

function handleModelOverridePatch(ctx: MockRequestContext): HandlerResult {
  bumpConfigRevision();
  return json(configurationPayload());
}

/* --------------------------------- 路由表 --------------------------------- */

interface Route {
  method: string;
  segments: string[];
  handler: Handler;
}

function route(method: string, pattern: string, handler: Handler): Route {
  return { method, segments: pattern.split("/").filter(Boolean), handler };
}

const routes: Route[] = [
  // 认证
  route("GET", "/api/v1/console/session", handleSession),
  route("GET", "/api/v1/console/auth/bootstrap", handleBootstrap),
  route("POST", "/api/v1/console/auth/preauth", handlePreauth),
  route("POST", "/api/v1/console/auth/login", handleLogin),
  route("POST", "/api/v1/console/auth/initialize", handleLogin),
  route("POST", "/api/v1/console/auth/logout", () => json({})),
  route("POST", "/api/v1/console/auth/password-reset/bootstrap", handleBootstrap),
  route("POST", "/api/v1/console/auth/password-reset", handleLogin),
  // 用户偏好与文件
  route("POST", "/api/v1/console/user-preferences/get", handlePreferencesGet),
  route("POST", "/api/v1/console/user-preferences/update", handlePreferencesUpdate),
  route("POST", "/api/v1/console/files/list", handleFileList),
  route("POST", "/api/v1/console/files/upload", handleFileUpload),
  route("POST", "/api/v1/console/files/delete", handleFileDelete),
  route("POST", "/api/v1/console/files/get/:fileId", (ctx) => binaryPlaceholder(ctx.params)),
  // 知识库
  route("POST", "/api/v1/console/knowledge/files/capabilities", handleKnowledgeCapabilities),
  route("POST", "/api/v1/console/knowledge/files/list", handleKnowledgeList),
  route("POST", "/api/v1/console/knowledge/files/upload", handleKnowledgeUpload),
  route("POST", "/api/v1/console/knowledge/files/delete", handleKnowledgeDelete),
  route("POST", "/api/v1/console/knowledge/files/retry", handleKnowledgeRetry),
  route("POST", "/api/v1/console/knowledge/files/get/:fileId", (ctx) => handleKnowledgeDownload(ctx.params)),
  // Todo
  route("POST", "/api/v1/console/todo/list", handleTodoList),
  route("POST", "/api/v1/console/todo/targets", handleTodoTargets),
  route("POST", "/api/v1/console/todo/create", handleTodoCreate),
  route("POST", "/api/v1/console/todo/get", handleTodoGet),
  route("POST", "/api/v1/console/todo/update", handleTodoUpdate),
  route("POST", "/api/v1/console/todo/delete", handleTodoDelete),
  // Memory
  route("POST", "/api/v1/console/memories/list", handleMemoryList),
  route("POST", "/api/v1/console/memories/targets", handleMemoryTargets),
  route("POST", "/api/v1/console/memories/get", handleMemoryGet),
  route("POST", "/api/v1/console/memories/create", handleMemoryCreate),
  route("POST", "/api/v1/console/memories/update", handleMemoryUpdate),
  route("POST", "/api/v1/console/memories/archive", handleMemoryArchive),
  route("POST", "/api/v1/console/memories/restore", handleMemoryRestore),
  route("POST", "/api/v1/console/memories/operations/prepare", handleMemoryPrepare),
  route("POST", "/api/v1/console/memories/operations/commit", handleMemoryCommit),
  // 运行状态与配置
  route("GET", "/api/v1/console/status", handleStatus),
  route("POST", "/api/v1/console/restart", handleRestart),
  route("POST", "/api/v1/markdown/render", handleMarkdownRender),
  route("GET", "/api/v1/console/configuration", handleConfigurationGet),
  route("PATCH", "/api/v1/console/configuration/runtime", handleRuntimePatch),
  route("PATCH", "/api/v1/console/configuration/secrets", handleSecretsPatch),
  route("PATCH", "/api/v1/console/configuration/agent", handleAgentPatch),
  route("POST", "/api/v1/console/configuration/validate", handleValidate),
  route("PATCH", "/api/v1/console/configuration/providers/credential", handleCredentialPatch),
  route("POST", "/api/v1/console/configuration/providers/test", handleProviderTest),
  route("POST", "/api/v1/console/configuration/providers/models", handleProviderModels),
  route("POST", "/api/v1/console/configuration/providers/model-metadata", handleProviderModelMetadata),
  route("PATCH", "/api/v1/console/configuration/providers/model-override", handleModelOverridePatch),
];

/** 匹配 method 与路径段；`:name` 段捕获为 ctx.params。 */
function matchRoute(method: string, pathname: string): { handler: Handler; params: Record<string, string> } | null {
  const segments = pathname.split("/").filter(Boolean);
  for (const candidate of routes) {
    if (candidate.method !== method || candidate.segments.length !== segments.length) continue;
    const params: Record<string, string> = {};
    let matched = true;
    for (let index = 0; index < candidate.segments.length; index += 1) {
      const pattern = candidate.segments[index]!;
      const actual = segments[index]!;
      if (pattern.startsWith(":")) params[pattern.slice(1)] = actual;
      else if (pattern !== actual) {
        matched = false;
        break;
      }
    }
    if (matched) return { handler: candidate.handler, params };
  }
  return null;
}

function binaryPlaceholder(params: Record<string, string>): HandlerResult {
  const label = userFiles.find((file) => file.file_id === params.fileId)?.filename ?? params.fileId ?? "mock 文件";
  const body = Buffer.from(placeholderSvg(label), "utf8");
  return { kind: "binary", status: 200, contentType: "image/svg+xml; charset=utf-8", body, filename: label };
}

/* -------------------------------- 入口分发 -------------------------------- */

function readRawBody(req: IncomingMessage): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", (chunk: Buffer) => chunks.push(chunk));
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

const delay = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

function sendJson(res: ServerResponse, status: number, payload: unknown): void {
  const body = JSON.stringify(payload);
  res.statusCode = status;
  res.setHeader("Content-Type", "application/json; charset=utf-8");
  res.end(body);
}

/** vite dev server 中间件入口：接管全部 /api/ 请求，其余交给 vite。 */
export async function handleMockRequest(req: IncomingMessage, res: ServerResponse): Promise<void> {
  const url = new URL(req.url ?? "/", "http://mock.local");
  const method = (req.method ?? "GET").toUpperCase();
  const matched = matchRoute(method, url.pathname);
  if (!matched) {
    sendJson(res, 404, { error: { code: "not_found", message: `mock 未实现：${method} ${url.pathname}` } });
    return;
  }
  const raw = await readRawBody(req);
  let body: Record<string, unknown> = {};
  if (raw.length > 0) {
    try {
      const parsed: unknown = JSON.parse(raw.toString("utf8"));
      if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) body = parsed as Record<string, unknown>;
    } catch {
      // multipart/非 JSON 请求体保留空对象，由 handler 自行解析 raw。
    }
  }
  await delay(LATENCY_MS);
  const result = await matched.handler({ body, params: matched.params, raw });
  if (result.kind === "json") {
    sendJson(res, result.status, result.payload);
    return;
  }
  res.statusCode = result.status;
  res.setHeader("Content-Type", result.contentType);
  res.setHeader("Content-Disposition", `attachment; filename*=UTF-8''${encodeURIComponent(result.filename)}`);
  res.end(result.body);
}
