/**
 * Web Console dev mock 的内存状态与 faker 种子数据。
 *
 * 只被 vite dev server（`apply: "serve"`）加载，绝不进入 src/ 生产构建与 dist/ 产物。
 * 数据形状以 src/api.ts、src/memory-api.ts 的解析函数为准（snake_case 线上格式），
 * 固定 faker 种子保证每次 `npm run dev` 看到同一批数据，便于截图对比与回归排查。
 * 所有 ID、openid、群号均为虚构，不对应任何真实账号。
 */
import { fakerZH_CN as faker } from "@faker-js/faker";

faker.seed(20260930);

/** mock 进程启动时刻：runtime.uptime_seconds 等按此动态计算。 */
const BOOT = Date.now();

/** 相对当前时刻的 ISO 时间；负偏移表示过去，正偏移表示未来。 */
export function iso(offsetSeconds = 0): string {
  return new Date(Date.now() + offsetSeconds * 1000).toISOString();
}

export function nowIso(): string {
  return new Date().toISOString();
}

/** 递增计数器：新建实体 ID，避免依赖随机数破坏可复现性。 */
function nextId(prefix: string, counter: { value: number }): string {
  counter.value += 1;
  return `${prefix}-${counter.value}`;
}

/* ---------------------------------- 会话 ---------------------------------- */

export const MOCK_USERNAME = "mock-admin";

export function sessionPayload(username = MOCK_USERNAME): unknown {
  return {
    session: {
      username,
      capabilities: ["console:manage"],
      csrf_token: "mock-csrf-token",
      expires_at: Math.floor(Date.now() / 1000) + 12 * 3600,
    },
  };
}

export const bootstrapPayload: unknown = {
  bootstrap: {
    initialized: true,
    setup_required: false,
    password_reset_pending: false,
    token_file: "config/secrets/bootstrap.token",
    expires_at: Math.floor(Date.now() / 1000) + 600,
  },
};

/* ------------------------------- 平台投递目标 ------------------------------ */

export interface MockTodoTarget {
  target_ref: string;
  platform: string;
  scope_type: string;
  user_id: string | null;
  group_id: string | null;
  account_id: string | null;
  reminder_supported: boolean;
  diagnostic: string | null;
}

/** Todo 可选投递目标：覆盖 OneBot 群/私聊、QQ C2C、微信服务号四种形态。 */
export const todoTargets: MockTodoTarget[] = [
  {
    target_ref: "onebot:group:778899",
    platform: "onebot",
    scope_type: "group",
    user_id: null,
    group_id: "778899",
    account_id: "maid-self",
    reminder_supported: true,
    diagnostic: null,
  },
  {
    target_ref: "onebot:user:100861",
    platform: "onebot",
    scope_type: "private",
    user_id: "100861",
    group_id: null,
    account_id: "maid-self",
    reminder_supported: true,
    diagnostic: null,
  },
  {
    target_ref: "qq_official:c2c:mock-openid-A1",
    platform: "qq_official",
    scope_type: "c2c",
    user_id: "mock-openid-A1",
    group_id: null,
    account_id: null,
    reminder_supported: false,
    diagnostic: "QQ C2C 不支持主动提醒，仅展示截止时间",
  },
  {
    target_ref: "wechat:mp:mock-open-o9",
    platform: "wechat_service",
    scope_type: "private",
    user_id: "mock-open-o9",
    group_id: null,
    account_id: null,
    reminder_supported: false,
    diagnostic: null,
  },
];

/* ---------------------------------- Todo ---------------------------------- */

export interface MockTodo {
  id: string;
  title: string;
  detail: string | null;
  due_date: string | null;
  due_at: string | null;
  reminder_at: string | null;
  time_precision: string;
  recurrence_kind: string;
  recurrence_interval_days: number;
  recurrence_interval: number | null;
  recurrence_unit: string;
  status: "pending" | "completed";
  created_at: string;
  updated_at: string;
  completed_at: string | null;
  target: MockTodoTarget;
}

const todoCounter = { value: 0 };

function seedTodo(overrides: Partial<Omit<MockTodo, "target">> & Pick<MockTodo, "title"> & { target_ref: string }): MockTodo {
  const { target_ref, ...rest } = overrides;
  const target = todoTargets.find((entry) => entry.target_ref === target_ref) ?? todoTargets[0]!;
  return {
    id: nextId("todo", todoCounter),
    detail: null,
    due_date: null,
    due_at: null,
    reminder_at: null,
    time_precision: "none",
    recurrence_kind: "none",
    recurrence_interval_days: 0,
    recurrence_interval: null,
    recurrence_unit: "day",
    status: "pending",
    created_at: iso(-faker.number.int({ min: 3600, max: 14 * 86400 })),
    updated_at: iso(-3600),
    completed_at: null,
    target,
    ...rest,
  };
}

const dayMs = 86400_000;
/** iso() 使用秒为单位的“一天”。 */
const dayS = 86_400;

function dateOnly(offsetDays: number): string {
  return new Date(Date.now() + offsetDays * dayMs).toISOString().slice(0, 10);
}

export const todos: MockTodo[] = [
  seedTodo({ title: "整理群公告草稿并同步频道", target_ref: "onebot:group:778899", due_date: dateOnly(1), due_at: iso(1 * dayS), reminder_at: iso(dayS - 3600), time_precision: "date_time" }),
  seedTodo({ title: "每周五汇总知识库新增词条", target_ref: "onebot:group:778899", recurrence_kind: "interval", recurrence_interval: 7, recurrence_unit: "day", recurrence_interval_days: 7, due_date: dateOnly(3) }),
  seedTodo({ title: "跟进 OneBot 适配器告警日志", target_ref: "onebot:user:100861", due_date: dateOnly(0), due_at: iso(6 * 3600), reminder_at: iso(5 * 3600), time_precision: "date_time" }),
  seedTodo({ title: "给新成员写一份入门指引", target_ref: "onebot:user:100861", detail: "覆盖常见命令、知识库检索和提醒用法。", due_date: dateOnly(5) }),
  seedTodo({ title: "检查 RSS 源抓取是否正常", target_ref: "wechat:mp:mock-open-o9", due_date: dateOnly(-1), time_precision: "date" }),
  seedTodo({ title: "备份 SQLite 主库并校验迁移版本", target_ref: "onebot:user:100861", status: "completed", completed_at: iso(-2 * dayS) }),
  seedTodo({ title: "清理知识库失效分片", target_ref: "onebot:group:778899", status: "completed", completed_at: iso(-5 * dayS) }),
  seedTodo({ title: "填写月度运行报告", target_ref: "qq_official:c2c:mock-openid-A1", due_date: dateOnly(8) }),
  seedTodo({ title: "试运行新的天气查询工具", target_ref: "onebot:group:778899", detail: "重点观察和风天气配额消耗。", due_date: dateOnly(2), reminder_at: iso(2 * dayS - 1800), time_precision: "date_time" }),
  seedTodo({ title: "归档上一季度聊天摘要", target_ref: "onebot:user:100861", status: "completed", completed_at: iso(-9 * dayS) }),
];

/* --------------------------------- Memory --------------------------------- */

export interface MockMemoryTarget {
  target_ref: string;
  scope: "personal" | "group_profile" | "group";
  platform: string;
  account_ref: string;
  group_ref: string | null;
  subject_ref: string | null;
  capabilities: { can_clear_target: boolean; can_disable_group_profile: boolean };
}

export interface MockMemory {
  memory_ref: string;
  target: MockMemoryTarget;
  version: number;
  content: string;
  kind: "personal" | "group_profile" | "group";
  category: "note" | "preference" | "identity" | "relation" | "instruction";
  visibility: "private" | "context_only" | "group_members" | "public";
  status: "active" | "archived";
  pinned: boolean;
  created_at: string;
  updated_at: string | null;
  last_confirmed_at: string | null;
  source_type: "user_confirmed" | "manual_import" | "system_derived" | "legacy";
  capabilities: { can_update: boolean; can_archive: boolean; can_restore: boolean; can_delete: boolean };
}

const personalTarget: MockMemoryTarget = {
  target_ref: "onebot:user:100861",
  scope: "personal",
  platform: "onebot",
  account_ref: "onebot:user:100861",
  group_ref: null,
  subject_ref: null,
  capabilities: { can_clear_target: true, can_disable_group_profile: false },
};

const groupTarget: MockMemoryTarget = {
  target_ref: "onebot:group:778899",
  scope: "group",
  platform: "onebot",
  account_ref: "onebot:group:778899",
  group_ref: "778899",
  subject_ref: null,
  capabilities: { can_clear_target: true, can_disable_group_profile: false },
};

const groupProfileTarget: MockMemoryTarget = {
  target_ref: "onebot:group:778899:profile",
  scope: "group_profile",
  platform: "onebot",
  account_ref: "onebot:group:778899",
  group_ref: "778899",
  subject_ref: null,
  capabilities: { can_clear_target: false, can_disable_group_profile: true },
};

const memoryCounter = { value: 0 };

function seedMemory(
  target: MockMemoryTarget,
  content: string,
  overrides: Partial<Omit<MockMemory, "memory_ref" | "target" | "content">> = {},
): MockMemory {
  memoryCounter.value += 1;
  const archived = overrides.status === "archived";
  return {
    memory_ref: `mem-${memoryCounter.value}`,
    target,
    version: 1,
    content,
    kind: target.scope,
    category: "note",
    visibility: target.scope === "group" ? "group_members" : "private",
    status: "active",
    pinned: false,
    created_at: iso(-faker.number.int({ min: 86400, max: 60 * 86400 })),
    updated_at: null,
    last_confirmed_at: iso(-faker.number.int({ min: 3600, max: 7 * 86400 })),
    source_type: "user_confirmed",
    capabilities: {
      can_update: !archived,
      can_archive: !archived,
      can_restore: archived,
      can_delete: true,
    },
    ...overrides,
  };
}

export const memories: MockMemory[] = [
  seedMemory(personalTarget, "管理员时区为 UTC+8，工作日 10:00-19:00 方便接收提醒。", { category: "preference", pinned: true, version: 3, updated_at: iso(-2 * dayS) }),
  seedMemory(personalTarget, "偏好在对话中使用中文回复，技术名词保留英文原文。", { category: "preference" }),
  seedMemory(personalTarget, "管理员的常用开发机别名是 workshop，部署脚本都在 runtime/ 下。", { category: "identity", source_type: "manual_import" }),
  seedMemory(groupTarget, "群内周五为自由水群日，机器人当天不发布每日摘要。", { category: "instruction", visibility: "group_members", version: 2, updated_at: iso(-dayS) }),
  seedMemory(groupTarget, "群成员约定：求助时先贴报错截图再描述现象。", { category: "note", visibility: "group_members" }),
  seedMemory(groupTarget, "2025 年 12 月组织过一次线上答疑，记录归档在知识库。", { category: "note", visibility: "group_members", status: "archived" }),
  seedMemory(groupProfileTarget, "群氛围偏向技术交流，活跃时段为晚间 20:00-23:00。", { category: "identity", source_type: "system_derived", last_confirmed_at: null }),
  seedMemory(groupProfileTarget, "群成员对部署与运维话题提问最多。", { category: "note", source_type: "system_derived", last_confirmed_at: null, version: 4, updated_at: iso(-3 * dayS) }),
  seedMemory(personalTarget, "旧的 RSS 阅读偏好已被新规则取代。", { category: "preference", status: "archived", source_type: "legacy" }),
];

/** Memory 两阶段破坏性操作的待确认令牌：prepare 写入、commit 校验并消费。 */
export const pendingMemoryOperations = new Map<string, { operation: string; target_ref: string; memory_ref: string | null }>();

/* -------------------------------- 知识库文件 ------------------------------- */

export interface MockKnowledgeFile {
  file_id: string | null;
  filename: string;
  content_type: string;
  size: number | null;
  source: "managed" | "directory";
  source_label: string;
  status: "pending" | "processing" | "ready" | "failed";
  uploaded_at: string | null;
  processing_started_at: string | null;
  processed_at: string | null;
  updated_at: string;
  error_code: string | null;
  error_summary: string | null;
  chunk_count: number | null;
  embedding_count: number | null;
  downloadable: boolean;
  download_url: string | null;
}

const knowledgeCounter = { value: 0 };

function seedKnowledge(overrides: Partial<MockKnowledgeFile> & Pick<MockKnowledgeFile, "filename" | "content_type">): MockKnowledgeFile {
  knowledgeCounter.value += 1;
  const ready = overrides.status ?? "ready";
  return {
    file_id: `kfile-${knowledgeCounter.value}`,
    size: faker.number.int({ min: 20_000, max: 900_000 }),
    source: "managed",
    source_label: "控制台上传",
    status: ready,
    uploaded_at: iso(-faker.number.int({ min: 3600, max: 45 * 86400 })),
    processing_started_at: iso(-3600),
    processed_at: ready === "ready" ? iso(-1800) : null,
    updated_at: iso(-1800),
    error_code: null,
    error_summary: null,
    chunk_count: ready === "ready" ? faker.number.int({ min: 8, max: 120 }) : null,
    embedding_count: ready === "ready" ? faker.number.int({ min: 8, max: 120 }) : null,
    downloadable: ready === "ready",
    download_url: null,
    ...overrides,
  };
}

export const knowledgeFiles: MockKnowledgeFile[] = [
  seedKnowledge({ filename: "部署运维手册.md", content_type: "text/markdown", status: "ready" }),
  seedKnowledge({ filename: "常见问题解答.md", content_type: "text/markdown", status: "ready" }),
  seedKnowledge({ filename: "工具调用白名单说明.txt", content_type: "text/plain", status: "ready" }),
  seedKnowledge({ filename: "接口契约速查.pdf", content_type: "application/pdf", status: "ready", source: "directory", source_label: "目录扫描" }),
  seedKnowledge({ filename: "群规与礼仪指引.md", content_type: "text/markdown", status: "processing", processed_at: null, chunk_count: null, embedding_count: null }),
  seedKnowledge({ filename: "历史遗留资料.docx", content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", status: "failed", error_code: "extract_failed", error_summary: "文档解析失败：不支持的 OOXML 结构", processed_at: null, chunk_count: null, embedding_count: null }),
  seedKnowledge({ filename: "prompt-编写规范.md", content_type: "text/markdown", status: "ready", source: "directory", source_label: "目录扫描" }),
];

/* -------------------------------- 用户文件 -------------------------------- */

export interface MockUserFile {
  file_id: string;
  filename: string;
  content_type: string;
  size: number;
  created_at: string;
  url: string;
}

const userFileCounter = { value: 0 };

/** 生成一张简单的 SVG 渐变图：mock 文件读取接口统一返回它，保证背景预览可渲染。 */
export function placeholderSvg(label: string): string {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="960" height="540">
  <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="#312e81"/><stop offset="1" stop-color="#0f172a"/>
  </linearGradient></defs>
  <rect width="960" height="540" fill="url(#g)"/>
  <text x="480" y="270" font-family="sans-serif" font-size="32" fill="#94a3b8" text-anchor="middle">${label} · mock</text>
</svg>`;
}

export const userFiles: MockUserFile[] = [
  { file_id: "ufile-1", filename: "mock-背景-靛夜.svg", content_type: "image/svg+xml", size: 4_182, created_at: iso(-30 * dayS), url: "/api/v1/console/files/get/ufile-1" },
  { file_id: "ufile-2", filename: "mock-背景-青苔.svg", content_type: "image/svg+xml", size: 4_301, created_at: iso(-9 * dayS), url: "/api/v1/console/files/get/ufile-2" },
];

export function addUserFile(filename: string, size: number): MockUserFile {
  userFileCounter.value += 1;
  const entry: MockUserFile = {
    file_id: `ufile-upload-${userFileCounter.value}`,
    filename,
    content_type: "image/svg+xml",
    size,
    created_at: nowIso(),
    url: `/api/v1/console/files/get/ufile-upload-${userFileCounter.value}`,
  };
  userFiles.unshift(entry);
  return entry;
}

/* ------------------------------- 用户界面偏好 ------------------------------ */

export const preferences = {
  custom_colors: ["#7c6cf6", "#2dd4bf"],
  background_file_ids: userFiles.map((file) => file.file_id),
  active_background_file_id: userFiles[0]?.file_id ?? null,
  background_mode: "default",
  kuliantnt: false,
};

/* -------------------------------- 运行状态 -------------------------------- */

function capability(overrides: Partial<Record<string, string>> = {}): Record<string, string> {
  return {
    text: "supported",
    markdown: "supported",
    image: "supported",
    file: "supported",
    mixed_message: "supported",
    streaming: "supported",
    ...overrides,
  };
}

export function consoleStatusPayload(): unknown {
  const uptimeSeconds = Math.max(1, Math.floor((Date.now() - BOOT) / 1000));
  return {
    runtime: {
      ok: true,
      ready: true,
      state: "ready",
      version: "v0.25.3-mock",
      started_at: new Date(BOOT).toISOString(),
      uptime_seconds: uptimeSeconds,
    },
    provider: {
      name: "智谱主连接",
      model: "glm-4.7-flash",
      streaming: true,
      configured: true,
      upstream: {
        state: "online",
        last_checked_at: iso(-120),
        error_summary: null,
      },
    },
    platforms: [
      {
        id: "qq_official",
        label: "QQ 官方机器人",
        configured: true,
        enabled: true,
        state: "online",
        last_event_at: iso(-45),
        last_error_summary: null,
        ready_at: new Date(BOOT).toISOString(),
        resumed_at: null,
        capability_scopes: [
          {
            id: "c2c",
            label: "私聊（C2C）",
            enabled: true,
            capabilities: {
              inbound: capability({ file: "unsupported" }),
              outbound: capability({ markdown: "disabled", file: "unsupported", streaming: "supported" }),
            },
          },
          {
            id: "group",
            label: "群聊（群 at）",
            enabled: true,
            capabilities: {
              inbound: capability({ file: "unsupported" }),
              outbound: capability({ markdown: "disabled", file: "unsupported", streaming: "unsupported" }),
            },
          },
        ],
      },
      {
        id: "onebot11",
        label: "OneBot 11",
        configured: true,
        enabled: true,
        state: "online",
        last_event_at: iso(-12),
        last_error_summary: null,
        ready_at: new Date(BOOT).toISOString(),
        resumed_at: null,
        capability_scopes: [
          {
            id: "group",
            label: "群聊",
            enabled: true,
            capabilities: {
              inbound: capability(),
              outbound: capability({ streaming: "unsupported" }),
            },
          },
          {
            id: "private",
            label: "私聊",
            enabled: true,
            capabilities: {
              inbound: capability(),
              outbound: capability(),
            },
          },
        ],
      },
      {
        id: "wechat_service",
        label: "微信服务号",
        configured: false,
        enabled: false,
        state: "not_configured",
        last_event_at: null,
        last_error_summary: null,
        ready_at: null,
        resumed_at: null,
        capability_scopes: [],
      },
    ],
    storage: [
      {
        id: "app_db",
        label: "SQLite 主库",
        path_summary: "runtime/data/app.db",
        state: "available",
        exists: true,
        readable: true,
        writable: true,
        error_summary: null,
        schema_summary: "schema v25 · migration 全部已应用",
      },
      {
        id: "knowledge_dir",
        label: "知识库目录",
        path_summary: "runtime/data/knowledge",
        state: "available",
        exists: true,
        readable: true,
        writable: true,
        error_summary: null,
        schema_summary: null,
      },
      {
        id: "user_files",
        label: "用户文件目录",
        path_summary: "runtime/data/user-files",
        state: "available",
        exists: true,
        readable: true,
        writable: true,
        error_summary: null,
        schema_summary: null,
      },
      {
        id: "secret_store",
        label: "加密 Secret 存储",
        path_summary: "config/secrets",
        state: "available",
        exists: true,
        readable: true,
        writable: true,
        error_summary: null,
        schema_summary: null,
      },
    ],
    configuration: {
      listen: "127.0.0.1:8080",
      cors_allowlist_configured: false,
      rss_enabled: true,
      tool_calling_enabled: true,
    },
  };
}

/* --------------------------------- 配置快照 -------------------------------- */

export interface MockConfigField {
  key: string;
  module: string;
  value_type: "string" | "boolean" | "integer" | "string_list";
  source: "environment" | "managed_toml" | "agent_toml" | "encrypted_secret" | "default" | "not_configured";
  overridden: boolean;
  editable: boolean;
  configured: boolean;
  valid: boolean;
  revision: string | null;
  sensitivity: "public" | "secret" | "restricted";
  apply_mode: "immediate" | "restart";
  saved_value: unknown;
  effective_value: unknown;
  running_value: unknown;
  pending_restart: boolean;
}

function field(
  key: string,
  module: string,
  valueType: MockConfigField["value_type"],
  savedValue: unknown,
  options: Partial<Pick<MockConfigField, "source" | "sensitivity" | "apply_mode" | "configured" | "editable" | "overridden">> = {},
): MockConfigField {
  const sensitivity = options.sensitivity ?? "public";
  const configured = options.configured ?? true;
  const source = options.source ?? (configured ? "managed_toml" : "not_configured");
  return {
    key,
    module,
    value_type: valueType,
    source,
    overridden: options.overridden ?? false,
    editable: options.editable ?? true,
    configured,
    valid: true,
    revision: null,
    sensitivity,
    apply_mode: options.apply_mode ?? "restart",
    // secret 字段只回传状态，不回传原文（与真实后端契约一致）。
    saved_value: sensitivity === "secret" ? null : savedValue,
    effective_value: configured ? savedValue : null,
    running_value: configured ? savedValue : null,
    pending_restart: false,
  };
}

export const configFields: MockConfigField[] = [
  field("bootstrap.listen_host", "bootstrap", "string", "127.0.0.1"),
  field("bootstrap.listen_port", "bootstrap", "integer", 8080),
  field("console.enabled", "console", "boolean", true),
  field("console.allowed_origins", "console", "string_list", []),
  field("console.trusted_proxy_ips", "console", "string_list", []),
  field("console.secure_cookies", "console", "boolean", false),
  field("command.prefix", "command", "string", "/"),
  field("provider.openai.enabled", "provider", "boolean", false, { configured: false }),
  field("provider.openai.base_url", "provider", "string", "https://api.openai.com/v1", { configured: false }),
  field("provider.openai.api_key", "provider", "string", null, { sensitivity: "secret", configured: false }),
  field("provider.deepseek.enabled", "provider", "boolean", false, { configured: false }),
  field("provider.deepseek.api_key", "provider", "string", null, { sensitivity: "secret", configured: false }),
  field("provider.bigmodel.enabled", "provider", "boolean", true),
  field("provider.bigmodel.base_url", "provider", "string", "https://open.bigmodel.cn/api/paas/v4"),
  field("provider.bigmodel.api_key", "provider", "string", null, { sensitivity: "secret" }),
  field("provider.gemini.enabled", "provider", "boolean", false, { configured: false }),
  field("provider.gemini.api_key", "provider", "string", null, { sensitivity: "secret", configured: false }),
  field("platform.qq_official.enabled", "platform", "boolean", true),
  field("platform.qq_official.app_id", "platform", "string", "mock-app-id-1234", { sensitivity: "restricted" }),
  field("platform.qq_official.app_secret", "platform", "string", null, { sensitivity: "secret" }),
  field("platform.onebot11.enabled", "platform", "boolean", true),
  field("platform.onebot11.bind_host", "platform", "string", "127.0.0.1"),
  field("platform.onebot11.bind_port", "platform", "integer", 3001),
  field("platform.onebot11.websocket_path", "platform", "string", "/onebot/v11/ws"),
  field("platform.onebot11.access_token", "platform", "string", null, { sensitivity: "secret" }),
  field("platform.wechat_service.enabled", "platform", "boolean", false, { configured: false }),
  field("platform.wechat_service.token", "platform", "string", null, { sensitivity: "secret", configured: false }),
  field("platform.wechat_service.app_id", "platform", "string", null, { sensitivity: "restricted", configured: false }),
  field("platform.wechat_service.app_secret", "platform", "string", null, { sensitivity: "secret", configured: false }),
  field("delivery.tts.provider", "delivery", "string", "none"),
  field("delivery.tts.qwen_api_key", "delivery", "string", null, { sensitivity: "secret", configured: false }),
  field("delivery.tts.qwen_model", "delivery", "string", "qwen-tts-latest", { configured: false }),
  field("delivery.tts.max_text_chars", "delivery", "integer", 200),
  field("features.rss.enabled", "features", "boolean", true),
  field("features.rss.translation_enabled", "features", "boolean", false),
  field("features.memory.consolidation_enabled", "features", "boolean", true),
  field("features.memory.dream_enabled", "features", "boolean", false),
  field("features.todo.daily_reminder_enabled", "features", "boolean", true),
  field("features.todo.daily_reminder_time", "features", "string", "08:30"),
  field("tools.web_search.tavily.api_key", "tools", "string", null, { sensitivity: "secret", configured: false }),
  field("weather.qweather.api_key", "weather", "string", null, { sensitivity: "secret", configured: false }),
  field("weather.qweather.api_host", "weather", "string", "https://geoapi.qweather.com", { configured: false }),
];

export const registeredTools = [
  { name: "todo_create", description: "创建 Todo 任务并按目标投递提醒" },
  { name: "todo_list", description: "按状态与关键词查询可见 Todo 列表" },
  { name: "todo_update", description: "修改 Todo 状态、截止时间或重复规则" },
  { name: "memory_save", description: "将用户明确要求的记忆写入对应范围" },
  { name: "memory_search", description: "按关键词检索当前可见的记忆条目" },
  { name: "rss_subscribe", description: "订阅 RSS 源并纳入每日摘要" },
  { name: "web_search", description: "显式联网搜索（/查 入口）" },
  { name: "knowledge_search", description: "在知识库分片中检索相关内容" },
  { name: "weather_query", description: "查询城市实时天气与预报" },
  { name: "train_ticket", description: "查询剩余车票信息" },
];

export interface MockAgentDocument {
  knowledge: { mode: string; embedding: { enabled: boolean; cache_dir: string } };
  tools: {
    web_search: {
      backend: string;
      max_results: number;
      search_depth: string;
      topic: string;
      time_range: string | null;
      connect_timeout_seconds: number;
      first_response_timeout_seconds: number;
      total_timeout_seconds: number;
      routes: Record<string, { model: string }>;
    };
  };
  model_routes: Record<string, { candidates: string[] }>;
  search_routes: Record<string, string>;
  scenes: Record<string, { tool_calling_enabled: boolean; enabled_tools: string[] }>;
  providers: Record<string, Record<string, unknown>>;
}

export const agentDocument: MockAgentDocument = {
  knowledge: { mode: "preflight", embedding: { enabled: true, cache_dir: "cache/knowledge-embedding" } },
  tools: {
    web_search: {
      backend: "tavily",
      max_results: 5,
      search_depth: "basic",
      topic: "general",
      time_range: null,
      connect_timeout_seconds: 10,
      first_response_timeout_seconds: 30,
      total_timeout_seconds: 90,
      routes: { private_search: { model: "glm-4.7-flash" }, group_search: { model: "glm-4.7-flash" } },
    },
  },
  model_routes: {
    private_main: { candidates: ["bigmodel_main:glm-4.7-flash"] },
    group_main: { candidates: ["bigmodel_main:glm-4.7-flash"] },
    aux: { candidates: ["bigmodel_main:glm-4.7-air"] },
  },
  search_routes: { private_search: "tavily", group_search: "tavily" },
  scenes: {
    private: {
      tool_calling_enabled: true,
      enabled_tools: ["todo_create", "todo_list", "memory_save", "memory_search", "web_search", "knowledge_search", "weather_query"],
    },
    group: { tool_calling_enabled: false, enabled_tools: [] },
  },
  providers: {
    bigmodel_main: {
      display_name: "智谱主连接",
      enabled: true,
      kind: "openai",
      base_url: "https://open.bigmodel.cn/api/paas/v4",
      auth_header: "Authorization",
      auth_scheme: "Bearer",
      request_timeout_seconds: 60,
    },
  },
};

export const providerPresets = [
  { id: "openai", name: "OpenAI", kind: "openai", base_url: "https://api.openai.com/v1", auth_header: "Authorization", auth_scheme: "Bearer" },
  { id: "deepseek", name: "DeepSeek", kind: "openai", base_url: "https://api.deepseek.com/v1", auth_header: "Authorization", auth_scheme: "Bearer" },
  { id: "bigmodel", name: "智谱 BigModel", kind: "openai", base_url: "https://open.bigmodel.cn/api/paas/v4", auth_header: "Authorization", auth_scheme: "Bearer" },
  { id: "gemini", name: "Gemini", kind: "gemini", base_url: "https://generativelanguage.googleapis.com/v1beta", auth_header: "x-goog-api-key", auth_scheme: "" },
];

export const providerAdapters = ["openai", "openai_responses", "gemini", "anthropic"];

export const providerCredentials: Record<string, { configured: boolean; editable: boolean; revision: string; pending_restart: boolean }> = {
  bigmodel_main: { configured: true, editable: true, revision: "cred-rev-1", pending_restart: false },
};

/** 配置与 Agent 各自的修订号：任何一次保存都递增，前端靠它做并发冲突检测。 */
export const configState = {
  revision: 1,
  agentRevision: 1,
};

export function configRevision(): string {
  return `mock-rev-${configState.revision}`;
}

export function agentRevision(): string {
  return `mock-agent-rev-${configState.agentRevision}`;
}

export function bumpConfigRevision(): string {
  configState.revision += 1;
  return configRevision();
}

export function bumpAgentRevision(): string {
  configState.agentRevision += 1;
  return agentRevision();
}
