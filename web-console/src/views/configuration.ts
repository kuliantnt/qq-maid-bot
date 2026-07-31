import {
  ConsoleApiError,
  fetchConfiguration,
  requestRestart,
  testProviderConnection,
  updateAgentConfiguration,
  updateRuntimeConfiguration,
  updateSecretConfiguration,
  validateConfiguration,
} from "../api.js";
import { agentToolOptions, selectedAgentToolNames, type AgentToolOption } from "../agent-tools.js";
import { togglePasswordReveal } from "../dom.js";
import {
  openCodeProviderChange,
  readOpenCodeProviders,
  renderOpenCodeProviders,
  renderOpenCodeRouteHints,
} from "../opencode-providers.js";
import type { ConfigFieldSnapshot, ConfigurationSnapshot, UserFile, UserPreferences } from "../types.js";
import type { ThemeController } from "../theme.js";
import { renderThemeSelector } from "./theme-selector.js";
import type { BackgroundController } from "../background.js";

const FIELD_LABELS: Record<string, string> = {
  "command.prefix": "聊天命令前缀",
  "delivery.tts.provider": "语音 Provider",
  "delivery.tts.qwen_api_key": "千问 TTS API Key",
  "delivery.tts.qwen_base_url": "千问 TTS Base URL",
  "delivery.tts.qwen_model": "千问 TTS 模型",
  "delivery.tts.qwen_voice": "千问 TTS 音色",
  "delivery.tts.request_timeout_seconds": "请求超时（秒）",
  "delivery.tts.max_text_chars": "最大朗读字符数",
  "provider.openai.base_url": "OpenAI Base URL",
  "provider.openai.api_mode": "OpenAI API 模式",
  "provider.openai.api_key": "OpenAI API Key",
  "provider.deepseek.base_url": "DeepSeek Base URL",
  "provider.deepseek.api_key": "DeepSeek API Key",
  "provider.bigmodel.base_url": "BigModel Base URL",
  "provider.bigmodel.api_key": "BigModel API Key",
  "provider.gemini.base_url": "Gemini Base URL",
  "provider.gemini.api_key": "Gemini API Key",
  "provider.mimo.api_key": "MiMo API Key",
  "provider.opencode.api_key": "OpenCode API Key",
  "tools.web_search.tavily.api_key": "Tavily API Key",
  "weather.qweather.api_key": "和风天气 API Key",
  "weather.qweather.api_host": "QWeather API Host",
  "weather.qweather.geo_host": "QWeather Geo Host",
  "platform.qq_official.enabled": "QQ 官方入口",
  "platform.qq_official.app_id": "QQ AppID",
  "platform.qq_official.app_secret": "QQ AppSecret",
  "platform.onebot11.enabled": "OneBot 11 入口",
  "platform.onebot11.bind_host": "OneBot 绑定地址",
  "platform.onebot11.bind_port": "OneBot 绑定端口",
  "platform.onebot11.websocket_path": "OneBot WebSocket 路径",
  "platform.onebot11.access_token": "OneBot Access Token",
  "platform.wechat_service.enabled": "微信服务号入口",
  "platform.wechat_service.token": "微信 Token",
  "platform.wechat_service.app_id": "微信 AppID",
  "platform.wechat_service.app_secret": "微信 AppSecret",
  "platform.wechat_service.encryption_mode": "微信消息加密模式",
  "platform.wechat_service.encoding_aes_key": "微信 EncodingAESKey",
  "platform.wechat_service.bind_host": "微信回调监听地址",
  "platform.wechat_service.bind_port": "微信回调监听端口",
  "platform.wechat_service.callback_path": "微信回调路径",
  "features.rss.enabled": "RSS",
  "features.rss.translation_enabled": "RSS 翻译",
  "features.memory.consolidation_enabled": "Memory 整理",
  "features.memory.dream_enabled": "Session Dream",
  "features.todo.daily_reminder_enabled": "Todo 每日提醒",
  "features.todo.daily_reminder_time": "Todo 提醒时间",
  "console.enabled": "Web 控制台",
  "console.allowed_origins": "Web 控制台允许来源",
  "console.trusted_proxy_ips": "Web 控制台可信代理 IP",
  "console.secure_cookies": "Web 控制台安全 Cookie",
  "bootstrap.listen_host": "Core 监听地址",
  "bootstrap.listen_port": "Core 监听端口",
  "bootstrap.database_file": "数据库文件",
  "bootstrap.database_pool_size": "数据库连接池大小",
  "bootstrap.runtime_config_file": "运行配置文件",
  "bootstrap.master_key_file": "主密钥文件",
  "bootstrap.agent_config_file": "Agent 配置文件",
  "bootstrap.ops_config_file": "运维配置文件",
};

interface FieldGroupDefinition {
  label: string;
  prefix: string;
  description?: string;
}

const FIELD_GROUPS: readonly FieldGroupDefinition[] = [
  { label: "命令设置", prefix: "command." },
  {
    label: "语音回复",
    prefix: "delivery.tts.",
    description: "修改后需要重启。Web 控制台只配置全局 TTS 能力；Provider 关闭或千问 API Key 未配置时，/语音 开启会被拒绝。私聊或群聊是否启用仍通过 /语音 开启、/语音 关闭管理。关闭 Provider 不会清除已保存的千问配置，也可以提前填写。",
  },
  { label: "模型服务", prefix: "provider." },
  { label: "QQ 官方入口", prefix: "platform.qq_official." },
  { label: "OneBot 11 入口", prefix: "platform.onebot11." },
  { label: "微信服务号入口", prefix: "platform.wechat_service." },
  { label: "功能开关", prefix: "features." },
  { label: "联网搜索", prefix: "tools.web_search." },
  { label: "天气服务", prefix: "weather." },
  { label: "Web 控制台", prefix: "console." },
  { label: "基础运行", prefix: "bootstrap." },
];

const TTS_PROVIDER_OPTIONS: ReadonlyArray<readonly [string, string]> = [
  ["disabled", "关闭"],
  ["qwen", "千问"],
];

const TTS_NUMBER_RANGES: Readonly<Record<string, readonly [number, number]>> = {
  "delivery.tts.request_timeout_seconds": [1, 120],
  "delivery.tts.max_text_chars": [1, 600],
};

export function configFieldLabel(key: string): string {
  return FIELD_LABELS[key] ?? key;
}

export function configFieldGroupLabel(key: string): string {
  return FIELD_GROUPS.find((group) => key.startsWith(group.prefix))?.label ?? "其他配置";
}

/** 未识别的历史 Provider 必须原样保留，避免页面加载或保存时静默改成受支持值。 */
export function ttsProviderOptions(currentValue: unknown): Array<[string, string]> {
  const current = currentValue === null || currentValue === undefined ? "disabled" : String(currentValue);
  const options = TTS_PROVIDER_OPTIONS.map(([value, label]): [string, string] => [value, label]);
  if (!options.some(([value]) => value === current)) {
    options.push([current, `${current}（当前自定义值）`]);
  }
  return options;
}

export function ttsNumberRange(key: string): readonly [number, number] | null {
  return TTS_NUMBER_RANGES[key] ?? null;
}

/** TTS 范围字段必须先完整通过整数与边界校验，不能沿用普通整数的宽松 parseInt 语义。 */
export function parseTtsNumberValue(key: string, rawValue: string): number {
  const range = ttsNumberRange(key);
  if (!range) throw new Error(`${configFieldLabel(key)}没有可用的页面输入范围`);
  const value = rawValue.trim() === "" ? Number.NaN : Number(rawValue);
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < range[0] || value > range[1]) {
    throw new Error(`${configFieldLabel(key)}必须是 ${range[0]} 到 ${range[1]} 之间的整数`);
  }
  return value;
}

const AGENT_ROUTE_LABELS: Record<string, string> = {
  private_main: "私聊主路线",
  group_main: "群聊主路线",
  aux: "辅助任务路线",
  private_search: "私聊搜索路线",
  group_search: "群聊搜索路线",
};

type ConfigurationPrimary = "runtime" | "secrets" | "agent" | "interface";
type ConfigurationNavigation = {
  primary: ConfigurationPrimary;
  secondary: Partial<Record<ConfigurationPrimary, string>>;
};

const PRIMARY_NAVIGATION: ReadonlyArray<{ id: ConfigurationPrimary; label: string; description: string }> = [
  { id: "runtime", label: "普通配置", description: "runtime.toml" },
  { id: "secrets", label: "敏感凭据", description: "高风险" },
  { id: "agent", label: "Agent 策略", description: "可能需重启" },
  { id: "interface", label: "Interface / Theme", description: "local-only" },
];

const SECONDARY_LABELS: Record<string, string> = {
  knowledge: "知识检索",
  "web-search": "联网搜索",
  providers: "模型 Provider",
  routes: "模型路线",
  "private-scene": "私聊场景",
  "group-scene": "群聊场景",
  theme: "主题",
};

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

export type AutosaveScope = "public" | "secret" | "agent";
export interface AutosaveBlurInput {
  readonly scope: AutosaveScope;
  readonly value: unknown;
  readonly baseline: unknown;
  readonly configured?: boolean;
  readonly clearRequested?: boolean;
}

export interface UserDataController {
  readonly preferences: UserPreferences;
  readonly files: readonly UserFile[];
  readonly updatePreferences: (patch: {
    readonly customColors?: readonly string[];
    readonly backgroundFileIds?: readonly string[];
    readonly activeBackgroundFileId?: string | null;
    readonly kuliantnt?: boolean;
  }) => Promise<UserPreferences>;
  readonly uploadFile?: (file: File) => Promise<UserFile>;
  readonly deleteFile?: (file: UserFile) => Promise<void>;
}

export function shouldAutosaveOnBlur(input: AutosaveBlurInput): boolean {
  if (input.scope === "secret") {
    if (input.clearRequested === true) return input.configured === true;
    return typeof input.value === "string" && input.value.length > 0;
  }
  if (input.scope === "public" && (input.baseline === null || input.baseline === undefined) && isEmptyInputValue(input.value)) {
    return false;
  }
  return JSON.stringify(input.value) !== JSON.stringify(input.baseline);
}

const DEFAULT_WEB_SEARCH_CONFIG: AgentWebSearchConfig = {
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
    ? "已选择 Tavily，但 Tavily API Key 尚未配置。请先在“敏感凭据”中保存 Key，重启后搜索才可用。"
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

let current: ConfigurationSnapshot | null = null;
let currentThemeController: ThemeController | null = null;
let currentBackgroundController: BackgroundController | null = null;
let toastTimer: number | undefined;
let configurationNavigation: ConfigurationNavigation = { primary: "runtime", secondary: {} };
let autosaveBound = false;
let queuedFocusRestoreId: string | null = null;
let saveQueue: Promise<void> = Promise.resolve();

export async function initializeConfiguration(
  themeController: ThemeController,
  backgroundController: BackgroundController,
  userData: UserDataController | null = null,
): Promise<void> {
  currentThemeController = themeController;
  currentBackgroundController = backgroundController;
  current = await fetchConfiguration();
  bindAutosave();
  render(current, themeController, backgroundController, userData);
}

function render(
  snapshot: ConfigurationSnapshot,
  themeController: ThemeController,
  backgroundController: BackgroundController,
  userData: UserDataController | null = null,
): void {
  current = snapshot;
  renderSummary(snapshot);
  renderThemeSelector(element("console-theme-selector"), themeController, backgroundController, userData);
  renderPublicFields(snapshot);
  renderSecretFields(snapshot);
  bindTtsProviderState();
  renderAgent(snapshot);
  renderConfigurationNavigation();
  bindRestart(snapshot);
  bindValidation();
  bindConnectionTest();
}

function renderSummary(snapshot: ConfigurationSnapshot): void {
  const target = element("configuration-summary");
  target.replaceChildren();
  const invalid = snapshot.fields.filter((field) => !field.valid).length;
  const pending = snapshot.fields.filter((field) => field.pendingRestart).length
    + (snapshot.agent?.pendingRestart ? 1 : 0);
  target.append(
    badge(snapshot.fileExists ? "runtime.toml 已建立" : "runtime.toml 尚未建立", snapshot.fileExists ? "ok" : "warn"),
    badge(invalid === 0 ? "本地预检通过" : "需要完成配置", invalid === 0 ? "ok" : "warn"),
    badge(pending === 0 ? "无待重启变更" : `${pending} 项重启后生效`, pending === 0 ? "muted" : "warn"),
  );
}

function renderPublicFields(snapshot: ConfigurationSnapshot): void {
  const target = element("public-config-fields");
  target.replaceChildren();
  appendGroupedFields(
    target,
    snapshot.fields.filter((value) => value.sensitivity !== "secret"),
    (field) => {
    const row = document.createElement("div");
    row.className = "config-row";
    decorateTtsRow(row, field);
    const label = document.createElement("label");
    label.htmlFor = inputId(field.key);
    label.textContent = configFieldLabel(field.key);
    label.append(meta(field));
    const input = fieldInput(field);
    input.dataset.autosaveScope = "public";
    row.append(label, input);
    if (field.savedValue !== null && field.editable) {
      const remove = button("恢复未保存值", "secondary");
      remove.addEventListener("click", () => void removePublicField(field.key));
      row.append(remove);
    }
      return row;
    },
    "runtime",
  );
  const save = element("save-public-config", HTMLButtonElement);
  save.onclick = () => void savePublicFields();
}

function renderSecretFields(snapshot: ConfigurationSnapshot): void {
  const target = element("secret-config-fields");
  target.replaceChildren();
  appendGroupedFields(
    target,
    snapshot.fields.filter((value) => value.sensitivity === "secret"),
    (field) => {
    const row = document.createElement("div");
    row.className = "config-row secret-row";
    decorateTtsRow(row, field);
    const label = document.createElement("label");
    label.htmlFor = inputId(field.key);
    label.textContent = configFieldLabel(field.key);
    label.append(meta(field));
    const input = document.createElement("input");
    input.id = inputId(field.key);
    input.type = "password";
    input.autocomplete = "new-password";
    input.placeholder = field.configured ? "已配置；留空表示不修改" : "尚未配置";
    input.disabled = !field.editable;
    input.dataset.configKey = field.key;
    input.dataset.autosaveScope = "secret";
    const reveal = document.createElement("button");
    reveal.type = "button";
    reveal.className = "reveal-button";
    reveal.textContent = "显示";
    reveal.setAttribute("aria-pressed", "false");
    reveal.setAttribute("aria-label", "显示或隐藏敏感值");
    reveal.disabled = !field.editable;
    reveal.addEventListener("click", () => togglePasswordReveal(reveal, input));
    const wrap = document.createElement("div");
    wrap.className = "password-field";
    wrap.append(input, reveal);
    const clearLabel = document.createElement("label");
    clearLabel.className = "clear-secret";
    const clear = document.createElement("input");
    clear.type = "checkbox";
    clear.dataset.clearKey = field.key;
    clear.disabled = !field.editable || !field.configured;
    clearLabel.append(clear, document.createTextNode(" 显式清除"));
    row.append(label, wrap, clearLabel);
      return row;
    },
    "secrets",
  );
  const save = element("save-secret-config", HTMLButtonElement);
  save.onclick = () => void saveSecrets();
}

function renderAgent(snapshot: ConfigurationSnapshot): void {
  const target = element("agent-config-fields");
  target.replaceChildren();
  const agent = snapshot.agent;
  if (!agent || !agent.fileExists) {
    target.textContent = "Agent 策略文件尚不可用；请检查默认 config/agent.toml 是否可写。";
    element("save-agent-config", HTMLButtonElement).disabled = true;
    return;
  }
  const documentValue = record(agent.savedValue);
  const knowledge = record(documentValue.knowledge);
  const embedding = record(knowledge.embedding);
  const runningKnowledge = record(record(agent.runningValue).knowledge);
  const runningEmbedding = record(runningKnowledge.embedding);
  target.append(configurationGroup("agent", "knowledge", fieldGroup("知识检索", [
    selectField("知识检索模式", "agent-knowledge-mode", string(knowledge.mode) || "preflight", [
      ["preflight", "preflight（高相关时条件注入）"],
      ["tool", "tool（完全由 Agent 检索）"],
      ["auto", "auto（紧急回退）"],
    ], !agent.editable, "agent"),
    checkboxField("本地语义召回", "agent-knowledge-embedding-enabled", embedding.enabled === true, !agent.editable, "agent"),
    statusField(
      `当前生效：${string(runningKnowledge.mode) || "preflight"} · 本地语义召回：${runningEmbedding.enabled === true ? "开启" : "关闭"}`,
      `来源：${sourceLabel(agent.source)}${agent.pendingRestart ? " · 已保存变更等待重启" : ""}`,
    ),
    statusField(
      "本地模型资源",
      "首次开启会下载 BAAI/bge-small-zh-v1.5，并增加 CPU、内存占用；低配置服务器建议关闭。",
    ),
  ])));
  const savedWebSearch = readAgentWebSearchConfig(documentValue);
  const runningWebSearch = readAgentWebSearchConfig(agent.runningValue);
  const tavilyKeyConfigured = snapshot.fields.some(
    (field) => field.key === "tools.web_search.tavily.api_key" && field.configured,
  );
  const backendPendingRestart = savedWebSearch.backend !== runningWebSearch.backend;
  const credentialStatus = statusField("Tavily 凭据", "");
  credentialStatus.id = "agent-web-search-credential-status";
  target.append(configurationGroup("agent", "web-search", fieldGroup("联网搜索", [
    statusField(
      `当前生效后端：${webSearchBackendLabel(runningWebSearch.backend)}`,
      `已保存后端：${webSearchBackendLabel(savedWebSearch.backend)} · ${backendPendingRestart ? "等待重启" : "当前已生效"}`,
    ),
    selectField("搜索后端", "agent-web-search-backend", savedWebSearch.backend, [
      ["provider_native", "Provider 原生搜索"],
      ["tavily", "Tavily"],
      ["disabled", "关闭联网搜索"],
    ], !agent.editable, "agent"),
    numberField("Tavily 结果数", "agent-web-search-max-results", savedWebSearch.maxResults, 1, 10, !agent.editable, "agent"),
    selectField("Tavily 搜索深度", "agent-web-search-depth", savedWebSearch.searchDepth, [
      ["basic", "basic"],
      ["advanced", "advanced"],
    ], !agent.editable, "agent"),
    selectField("Tavily 主题", "agent-web-search-topic", savedWebSearch.topic, [
      ["general", "通用"],
      ["news", "新闻"],
      ["finance", "金融"],
    ], !agent.editable, "agent"),
    selectField("Tavily 时间范围", "agent-web-search-time-range", savedWebSearch.timeRange ?? "", [
      ["", "不限"],
      ["day", "最近一天"],
      ["week", "最近一周"],
      ["month", "最近一月"],
      ["year", "最近一年"],
    ], !agent.editable, "agent"),
    numberField("连接超时（秒）", "agent-web-search-connect-timeout", savedWebSearch.connectTimeoutSeconds, 1, 3600, !agent.editable, "agent"),
    numberField("首响应超时（秒）", "agent-web-search-first-response-timeout", savedWebSearch.firstResponseTimeoutSeconds, 1, 3600, !agent.editable, "agent"),
    numberField("总超时（秒）", "agent-web-search-total-timeout", savedWebSearch.totalTimeoutSeconds, 1, 3600, !agent.editable, "agent"),
    credentialStatus,
  ])));
  const backendSelect = element("agent-web-search-backend", HTMLSelectElement);
  const refreshCredentialStatus = (): void => {
    updateTavilyCredentialStatus(
      isWebSearchBackend(backendSelect.value) ? backendSelect.value : "provider_native",
      tavilyKeyConfigured,
    );
  };
  backendSelect.addEventListener("change", refreshCredentialStatus);
  refreshCredentialStatus();
  target.append(configurationGroup("agent", "providers", renderOpenCodeProviders(
    snapshot,
    async (form) => {
      let change: Record<string, unknown>;
      try {
        change = openCodeProviderChange(form);
      } catch (cause) {
        showResult(errorMessage(cause), true);
        return;
      }
      await runSave(async () => updateAgentConfiguration(current!.agent!.revision, [change]));
    },
    async (id) => runSave(async () => updateAgentConfiguration(current!.agent!.revision, [{
      action: "remove_provider",
      id,
     }])),
  )));
  const openCodeKeyConfigured = snapshot.fields.some(
    (field) => field.key === "provider.opencode.api_key" && field.configured,
  );
  const modelRoutes = record(documentValue.model_routes);
  const routes = document.createElement("div");
  routes.append(fieldGroup("模型路线", [
    ...["private_main", "group_main", "aux"].map((routeName) => {
      const route = record(modelRoutes[routeName]);
      return textField(AGENT_ROUTE_LABELS[routeName] ?? routeName, `agent-route-${routeName}`, array(route.candidates).join(", "), !agent.editable, "agent");
    }),
    ...["private_search", "group_search"].map((routeName) => textField(
      AGENT_ROUTE_LABELS[routeName] ?? routeName,
      `agent-search-${routeName}`,
      savedWebSearch.routes[routeName] ?? "",
      !agent.editable,
      "agent",
    )),
  ]));
  routes.append(renderOpenCodeRouteHints(
    !agent.editable,
    readOpenCodeProviders(documentValue).filter((provider) => provider.enabled).map((provider) => provider.id),
    openCodeKeyConfigured,
  ));
  target.append(configurationGroup("agent", "routes", routes));
  const scenesGroup = document.createElement("div");
  const scenes = record(documentValue.scenes);
  for (const sceneName of ["private", "group"]) {
    const scene = record(scenes[sceneName]);
    const row = document.createElement("div");
    row.className = "config-row compact-row";
    const label = document.createElement("label");
    label.htmlFor = `agent-tool-${sceneName}`;
    label.textContent = `${sceneName === "private" ? "私聊" : "群聊"} Tool Calling`;
    const input = document.createElement("input");
    input.id = `agent-tool-${sceneName}`;
    input.type = "checkbox";
    input.checked = scene.tool_calling_enabled === true;
    input.disabled = !agent.editable;
    input.dataset.autosaveScope = "agent-scene";
    input.dataset.autosaveScene = sceneName;
    row.append(label, input);
    const sceneGroup = document.createElement("div");
    sceneGroup.append(row);

    const tools = document.createElement("fieldset");
    tools.className = "tool-whitelist";
    const legend = document.createElement("legend");
    legend.textContent = `${sceneName === "private" ? "私聊" : "群聊"}工具白名单`;
    tools.append(legend);
    const savedNames = array(scene.enabled_tools).filter((value): value is string => typeof value === "string");
    const visibleTools = agentToolOptions(snapshot.registeredTools, savedNames, agent.editable);
    if (visibleTools.length === 0) {
      const hint = document.createElement("p");
      hint.className = "hint";
      hint.textContent = "当前没有可用的已注册工具。";
      tools.append(hint);
    } else {
      const grid = document.createElement("div");
      grid.className = "tool-whitelist-grid";
      for (const tool of visibleTools) {
        grid.append(toolCheckbox(tool, sceneName));
      }
      tools.append(grid);
    }
    const saveScene = document.createElement("button");
    saveScene.type = "button";
    saveScene.className = "secondary tool-whitelist-save";
    saveScene.textContent = `保存${sceneName === "private" ? "私聊" : "群聊"}配置`;
    saveScene.disabled = !agent.editable;
    saveScene.onclick = () => void saveAgentScene(sceneName);
    tools.append(saveScene);
    sceneGroup.append(tools);
    scenesGroup.append(configurationGroup("agent", `${sceneName}-scene`, sceneGroup));
  }
  target.append(scenesGroup);
  const save = element("save-agent-config", HTMLButtonElement);
  save.disabled = !agent.editable;
  save.onclick = () => void saveAgent();
}

function appendGroupedFields(
  target: HTMLElement,
  fields: ConfigFieldSnapshot[],
  row: (field: ConfigFieldSnapshot) => HTMLElement,
  primary: ConfigurationPrimary,
): void {
  const remaining = new Set(fields);
  for (const group of FIELD_GROUPS) {
    const grouped = fields.filter((field) => field.key.startsWith(group.prefix));
    if (grouped.length === 0) continue;
    target.append(configurationGroup(primary, group.prefix, fieldGroup(group.label, grouped.map(row))));
    grouped.forEach((field) => remaining.delete(field));
  }
  if (remaining.size > 0) target.append(configurationGroup(primary, "other", fieldGroup("其他配置", [...remaining].map(row))));
}

function configurationGroup(primary: ConfigurationPrimary, group: string, content: HTMLElement): HTMLElement {
  const wrapper = document.createElement("section");
  wrapper.className = "configuration-content-group";
  wrapper.dataset.configurationGroup = group;
  wrapper.dataset.configurationPrimary = primary;
  wrapper.append(content);
  return wrapper;
}

function fieldGroup(label: string, rows: HTMLElement[], description?: string): HTMLElement {
  const section = document.createElement("section");
  section.className = "config-field-group";
  const heading = document.createElement("h3");
  heading.textContent = label;
  const grid = document.createElement("div");
  grid.className = "config-field-group-grid";
  grid.append(...rows);
  section.append(heading);
  if (description) {
    const hint = document.createElement("p");
    hint.className = "config-field-group-hint";
    hint.textContent = description;
    section.append(hint);
  }
  section.append(grid);
  return section;
}

function renderConfigurationNavigation(): void {
  const primaryTabs = element("configuration-primary-tabs");
  const secondaryTabs = element("configuration-secondary-tabs");
  primaryTabs.replaceChildren();
  secondaryTabs.replaceChildren();
  const availableGroups = new Map<ConfigurationPrimary, string[]>();
  for (const primary of PRIMARY_NAVIGATION.map((item) => item.id)) {
    const panel = element(`configuration-panel-${primary}`);
    const groups = [...panel.querySelectorAll<HTMLElement>("[data-configuration-group]")]
      .map((group) => group.dataset.configurationGroup)
      .filter((group): group is string => typeof group === "string");
    availableGroups.set(primary, primary === "interface" ? ["theme"] : groups);
  }
  if (!availableGroups.get(configurationNavigation.primary)?.length) configurationNavigation.primary = "runtime";
  const secondary = availableGroups.get(configurationNavigation.primary) ?? [];
  const selectedSecondary = configurationNavigation.secondary[configurationNavigation.primary];
  if (!selectedSecondary || !secondary.includes(selectedSecondary)) {
    const firstSecondary = secondary[0];
    if (firstSecondary) configurationNavigation.secondary[configurationNavigation.primary] = firstSecondary;
  }

  for (const [index, item] of PRIMARY_NAVIGATION.entries()) {
    const tab = configurationTab(`configuration-primary-${item.id}`, item.label, item.description);
    tab.dataset.configurationPrimary = item.id;
    tab.setAttribute("aria-selected", String(item.id === configurationNavigation.primary));
    tab.tabIndex = item.id === configurationNavigation.primary ? 0 : -1;
    tab.addEventListener("click", () => {
      configurationNavigation.primary = item.id;
      renderConfigurationNavigation();
    });
    bindTabKeyboard(tab, primaryTabs, index, PRIMARY_NAVIGATION.length, () => {
      configurationNavigation.primary = item.id;
      renderConfigurationNavigation();
    });
    primaryTabs.append(tab);
  }
  for (const [index, group] of secondary.entries()) {
    const tab = configurationTab(
      `configuration-secondary-${configurationNavigation.primary}-${group.replaceAll(".", "-")}`,
      secondaryLabel(configurationNavigation.primary, group),
      "配置分组",
    );
    tab.dataset.configurationGroup = group;
    tab.setAttribute("aria-selected", String(group === configurationNavigation.secondary[configurationNavigation.primary]));
    tab.tabIndex = group === configurationNavigation.secondary[configurationNavigation.primary] ? 0 : -1;
    tab.addEventListener("click", () => {
      configurationNavigation.secondary[configurationNavigation.primary] = group;
      renderConfigurationNavigation();
    });
    bindTabKeyboard(tab, secondaryTabs, index, secondary.length, () => {
      configurationNavigation.secondary[configurationNavigation.primary] = group;
      renderConfigurationNavigation();
    });
    secondaryTabs.append(tab);
  }
  for (const item of PRIMARY_NAVIGATION) {
    const panel = element(`configuration-panel-${item.id}`);
    const isPrimary = item.id === configurationNavigation.primary;
    panel.hidden = !isPrimary;
    if (!isPrimary) continue;
    for (const group of panel.querySelectorAll<HTMLElement>("[data-configuration-group]")) {
      group.hidden = group.dataset.configurationGroup !== configurationNavigation.secondary[item.id];
    }
  }
}

function configurationTab(id: string, label: string, description: string): HTMLButtonElement {
  const tab = document.createElement("button");
  tab.id = id;
  tab.type = "button";
  tab.className = "configuration-tab";
  tab.setAttribute("role", "tab");
  const panelId = id.includes("-secondary-")
    ? `configuration-panel-${id.split("-secondary-")[1]?.split("-")[0] ?? "runtime"}`
    : id.replace("-primary-", "-panel-");
  tab.setAttribute("aria-controls", panelId);
  tab.title = description;
  tab.textContent = label;
  return tab;
}

function bindTabKeyboard(
  tab: HTMLButtonElement,
  tablist: HTMLElement,
  index: number,
  count: number,
  activate: () => void,
): void {
  tab.addEventListener("keydown", (event) => {
    const tabs = [...tablist.querySelectorAll<HTMLButtonElement>("[role=tab]")];
    let nextIndex: number | null = null;
    if (event.key === "ArrowRight") nextIndex = (index + 1) % count;
    if (event.key === "ArrowLeft") nextIndex = (index - 1 + count) % count;
    if (event.key === "Home") nextIndex = 0;
    if (event.key === "End") nextIndex = count - 1;
    if (nextIndex !== null) {
      event.preventDefault();
      tabs[nextIndex]?.focus();
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      activate();
    }
  });
}

function secondaryLabel(primary: ConfigurationPrimary, group: string): string {
  if (primary === "runtime" || primary === "secrets") {
    return FIELD_GROUPS.find((item) => item.prefix === group)?.label ?? "其他配置";
  }
  return SECONDARY_LABELS[group] ?? group;
}

export function publicConfigurationChanges(
  fields: ConfigFieldSnapshot[],
  values: ReadonlyMap<string, unknown>,
): Array<Record<string, unknown>> {
  const changes: Array<Record<string, unknown>> = [];
  for (const field of fields.filter((value) => value.sensitivity === "public" && value.editable)) {
    if (!values.has(field.key)) continue;
    const value = values.get(field.key);
    const baseline = field.savedValue ?? field.effectiveValue;
    // 未配置的可选字段会显示为空输入框；用户未触碰时不能把空字符串误当成新配置提交。
    if ((baseline === null || baseline === undefined) && isEmptyInputValue(value)) continue;
    if (JSON.stringify(value) !== JSON.stringify(baseline)) {
      changes.push({ action: "set", key: field.key, value });
    }
  }
  return changes;
}

async function savePublicFields(): Promise<void> {
  if (!current) return;
  const values = new Map<string, unknown>();
  for (const field of current.fields.filter((value) => value.sensitivity === "public" && value.editable)) {
    const input = configInput(field.key);
    if (!input.checkValidity()) {
      input.reportValidity();
      return showResult(`${configFieldLabel(field.key)}不符合页面输入范围，请修改后再保存。`, true);
    }
    try {
      const value = ttsNumberRange(field.key)
        ? parseTtsNumberValue(field.key, input.value)
        : inputValue(input, field);
      values.set(field.key, value);
    } catch (cause) {
      return showResult(errorMessage(cause), true);
    }
  }
  const changes = publicConfigurationChanges(current.fields, values);
  if (changes.length === 0) return showResult("没有需要保存的普通配置。", false);
  await runSave(async () => updateRuntimeConfiguration(current!.revision, changes));
}

async function removePublicField(key: string): Promise<void> {
  if (!current) return;
  await runSave(async () => updateRuntimeConfiguration(current!.revision, [{ action: "remove", key }]));
}

export function secretConfigurationChanges(
  fields: ConfigFieldSnapshot[],
  values: ReadonlyMap<string, string>,
  clearKeys: ReadonlySet<string>,
): Array<Record<string, unknown>> {
  const changes: Array<Record<string, unknown>> = [];
  for (const field of fields.filter((value) => value.sensitivity === "secret" && value.editable)) {
    if (clearKeys.has(field.key)) {
      changes.push({ action: "clear", key: field.key, expected_revision: field.revision ?? "missing" });
    } else {
      const value = values.get(field.key) ?? "";
      if (value.length > 0) {
        changes.push({ action: "replace", key: field.key, value, expected_revision: field.revision ?? "missing" });
      }
    }
  }
  return changes;
}

async function saveSecrets(): Promise<void> {
  if (!current) return;
  const values = new Map<string, string>();
  const clearKeys = new Set<string>();
  for (const field of current.fields.filter((value) => value.sensitivity === "secret" && value.editable)) {
    values.set(field.key, element(inputId(field.key), HTMLInputElement).value);
    const clear = document.querySelector<HTMLInputElement>(`input[data-clear-key="${field.key}"]`);
    if (clear?.checked) clearKeys.add(field.key);
  }
  const changes = secretConfigurationChanges(current.fields, values, clearKeys);
  if (changes.length === 0) return showResult("留空不会清除 secret；当前没有显式变更。", false);
  await runSave(async () => updateSecretConfiguration(changes));
}

async function saveAgent(): Promise<void> {
  if (!current?.agent) return;
  let webSearchChange: Record<string, unknown>;
  try {
    webSearchChange = webSearchConfigChange(webSearchFormConfig());
  } catch (cause) {
    showResult(errorMessage(cause), true);
    return;
  }
  const documentValue = record(current.agent.savedValue);
  const scenes = record(documentValue.scenes);
  const embedding = record(record(documentValue.knowledge).embedding);
  const changes: unknown[] = [{
    action: "set_knowledge",
    mode: element("agent-knowledge-mode", HTMLSelectElement).value,
    embedding: {
      enabled: element("agent-knowledge-embedding-enabled", HTMLInputElement).checked,
      cache_dir: string(embedding.cache_dir) || "cache/knowledge-embedding",
    },
  }, webSearchChange];
  for (const routeName of ["private_main", "group_main", "aux"]) {
    const candidates = element(`agent-route-${routeName}`, HTMLInputElement).value
      .split(",").map((value) => value.trim()).filter(Boolean);
    changes.push({ action: "set_model_route", name: routeName, candidates });
  }
  const savedRoutes = readAgentWebSearchConfig(documentValue).routes;
  changes.push(...webSearchRouteChanges(savedRoutes, {
    private_search: element("agent-search-private_search", HTMLInputElement).value,
    group_search: element("agent-search-group_search", HTMLInputElement).value,
  }));
  for (const sceneName of ["private", "group"]) {
    changes.push({ action: "set_scene", scene: sceneName, config: agentSceneConfig(sceneName, scenes) });
  }
  await runSave(async () => updateAgentConfiguration(current!.agent!.revision, changes));
}

async function saveAgentScene(sceneName: string): Promise<void> {
  if (!current?.agent) return;
  const scenes = record(record(current.agent.savedValue).scenes);
  await runSave(async () => updateAgentConfiguration(current!.agent!.revision, [{
    action: "set_scene",
    scene: sceneName,
    config: agentSceneConfig(sceneName, scenes),
  }]));
}

async function saveOpenCodeProvider(id: string): Promise<void> {
  if (!current?.agent) return;
  const baseUrl = element(`${id}-base-url`, HTMLInputElement);
  const timeout = element(`${id}-timeout`, HTMLInputElement);
  const saved = readOpenCodeProviders(current.agent.savedValue).find((provider) => provider.id === id);
  if (!saved) return;
  const form = {
    ...saved,
    baseUrl: baseUrl.value,
    requestTimeoutSeconds: timeout.value.trim() ? Number(timeout.value) : null,
    enabled: true,
  };
  if (!shouldAutosaveOnBlur({ scope: "agent", value: form, baseline: saved })) return;
  let change: Record<string, unknown>;
  try {
    change = openCodeProviderChange(form);
  } catch (cause) {
    showResult(errorMessage(cause), true);
    return;
  }
  await runSave(async () => updateAgentConfiguration(current?.agent?.revision ?? "missing", [change]));
}

function agentSceneConfig(sceneName: string, scenes: Record<string, unknown>): Record<string, unknown> {
  const toolInputs = document.querySelectorAll<HTMLInputElement>(`input[data-agent-tool="${sceneName}"]`);
  return {
    ...record(scenes[sceneName]),
    tool_calling_enabled: element(`agent-tool-${sceneName}`, HTMLInputElement).checked,
    enabled_tools: selectedAgentToolNames(toolInputs),
  };
}

function webSearchFormConfig(): AgentWebSearchConfig {
  const backend = element("agent-web-search-backend", HTMLSelectElement).value;
  const searchDepth = element("agent-web-search-depth", HTMLSelectElement).value;
  const topic = element("agent-web-search-topic", HTMLSelectElement).value;
  const timeRange = element("agent-web-search-time-range", HTMLSelectElement).value;
  if (!isWebSearchBackend(backend)) throw new Error("联网搜索后端无效");
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

function bindAutosave(): void {
  if (autosaveBound) return;
  autosaveBound = true;
  document.addEventListener("focusout", (event) => {
    const target = event.target;
    if (!(target instanceof HTMLInputElement) && !(target instanceof HTMLSelectElement)) return;
    const related = event.relatedTarget;
    queuedFocusRestoreId = related instanceof HTMLElement && related.id ? related.id : null;
    void autosaveBlur(target);
  });
}

async function autosaveBlur(target: HTMLInputElement | HTMLSelectElement): Promise<void> {
  if (target.disabled || !current) return;
  const scene = target.dataset.autosaveScene;
  if (target.dataset.configKey) {
    const field = current.fields.find((value) => value.key === target.dataset.configKey);
    if (!field || !field.editable) return;
    if (field.sensitivity === "secret") {
      const clear = document.querySelector<HTMLInputElement>(`input[data-clear-key="${field.key}"]`);
      if (!shouldAutosaveOnBlur({
        scope: "secret",
        value: target.value,
        baseline: field.savedValue,
        configured: field.configured,
        ...(clear?.checked === undefined ? {} : { clearRequested: clear.checked }),
      })) return;
      await saveSecrets();
      return;
    }
    const value = inputValue(target, field);
    if (!shouldAutosaveOnBlur({ scope: "public", value, baseline: field.savedValue ?? field.effectiveValue })) return;
    await savePublicFields();
    return;
  }
  if (target.dataset.clearKey) {
    const field = current.fields.find((value) => value.key === target.dataset.clearKey);
    if (!(target instanceof HTMLInputElement) || !field || !field.editable || !target.checked) return;
    if (!shouldAutosaveOnBlur({ scope: "secret", value: "", baseline: field.savedValue, configured: field.configured, clearRequested: true })) return;
    await saveSecrets();
    return;
  }
  const providerId = target.dataset.autosaveProvider;
  if (providerId) {
    await saveOpenCodeProvider(providerId);
    return;
  }
  if (scene) {
    if (agentSceneChanged(scene)) await saveAgentScene(scene);
    return;
  }
  if (target.dataset.autosaveScope === "agent" && agentFieldChanged(target.id)) await saveAgent();
}

function agentSceneChanged(sceneName: string): boolean {
  if (!current?.agent) return false;
  const savedScenes = record(record(current.agent.savedValue).scenes);
  return shouldAutosaveOnBlur({
    scope: "agent",
    value: agentSceneConfig(sceneName, savedScenes),
    baseline: record(savedScenes[sceneName]),
  });
}

function agentFieldChanged(id: string): boolean {
  if (!current?.agent) return false;
  const saved = record(current.agent.savedValue);
  const webSearch = readAgentWebSearchConfig(current.agent.savedValue);
  const currentValue = id === "agent-knowledge-mode" ? element(id, HTMLSelectElement).value
    : id === "agent-knowledge-embedding-enabled" ? element(id, HTMLInputElement).checked
    : id.startsWith("agent-web-search-") ? agentWebSearchInputValue(id)
    : id.startsWith("agent-route-") ? element(id, HTMLInputElement).value.split(",").map((value) => value.trim()).filter(Boolean)
    : id.startsWith("agent-search-") ? element(id, HTMLInputElement).value.trim()
    : null;
  const baseline = id === "agent-knowledge-mode" ? string(record(saved.knowledge).mode) || "preflight"
    : id === "agent-knowledge-embedding-enabled" ? record(record(saved.knowledge).embedding).enabled === true
    : id.startsWith("agent-web-search-") ? webSearch[agentWebSearchKey(id)]
    : id.startsWith("agent-route-") ? array(record(record(saved.model_routes)[id.replace("agent-route-", "")]).candidates).map(string)
    : id.startsWith("agent-search-") ? webSearch.routes[id.replace("agent-search-", "")]
    : null;
  return currentValue !== null && shouldAutosaveOnBlur({ scope: "agent", value: currentValue, baseline });
}

function agentWebSearchKey(id: string): keyof AgentWebSearchConfig {
  return ({
    "agent-web-search-backend": "backend",
    "agent-web-search-max-results": "maxResults",
    "agent-web-search-depth": "searchDepth",
    "agent-web-search-topic": "topic",
    "agent-web-search-time-range": "timeRange",
    "agent-web-search-connect-timeout": "connectTimeoutSeconds",
    "agent-web-search-first-response-timeout": "firstResponseTimeoutSeconds",
    "agent-web-search-total-timeout": "totalTimeoutSeconds",
  } as const)[id] ?? "backend";
}

function agentWebSearchInputValue(id: string): unknown {
  const key = agentWebSearchKey(id);
  if (key === "backend" || key === "searchDepth" || key === "topic" || key === "timeRange") {
    return element(id, HTMLSelectElement).value;
  }
  return Number(element(id, HTMLInputElement).value);
}

function toolCheckbox(tool: AgentToolOption, sceneName: string): HTMLElement {
  const label = document.createElement("label");
  label.className = "tool-checkbox";
  label.title = tool.description;
  const input = document.createElement("input");
  input.type = "checkbox";
  input.value = tool.name;
  input.checked = tool.checked;
  input.disabled = tool.disabled;
  input.dataset.agentTool = sceneName;
  input.dataset.autosaveScene = sceneName;
  const name = document.createElement("span");
  name.textContent = tool.name === "image_generation" ? "图片生成" : tool.name;
  label.append(input, name);
  if (!tool.registered) {
    const state = document.createElement("span");
    state.className = "tool-registration-state";
    state.textContent = "当前进程未注册";
    label.append(state);
  }
  return label;
}

function bindRestart(snapshot: ConfigurationSnapshot): void {
  const restart = element("restart-service", HTMLButtonElement);
  restart.disabled = !snapshot.restartAvailable;
  restart.title = snapshot.restartAvailable ? "通过当前运行目录的 botctl 重启" : "当前运行目录没有可用的 botctl 重启脚本";
  restart.onclick = async () => {
    if (!window.confirm("确定要重启服务吗？控制台会短暂离线。")) return;
    restart.disabled = true;
    try {
      showResult(await requestRestart(), false);
    } catch (cause) {
      showResult(errorMessage(cause), true);
      restart.disabled = !snapshot.restartAvailable;
    }
  };
}

function bindValidation(): void {
  element("validate-config", HTMLButtonElement).onclick = async () => {
    try {
      const result = await validateConfiguration();
      showResult(result.message, !result.valid);
    } catch (cause) {
      showResult(errorMessage(cause), true);
    }
  };
}

function bindConnectionTest(): void {
  const button = element("test-provider-connection", HTMLButtonElement);
  button.onclick = async () => {
    const target = element("connection-provider", HTMLSelectElement).value;
    button.disabled = true;
    showConnectionTestResult("正在连接 Provider，请稍候……", false);
    try {
      const result = await testProviderConnection(target);
      showConnectionTestResult(`${result.message}（${result.classification}）`, !result.success);
    } catch (cause) {
      showConnectionTestResult(errorMessage(cause), true);
    } finally {
      button.disabled = false;
    }
  };
}

async function runSave(action: () => Promise<ConfigurationSnapshot>): Promise<void> {
  const save = async (): Promise<void> => {
    setButtonsDisabled(true);
    try {
      const snapshot = await action();
      if (!currentThemeController || !currentBackgroundController) throw new Error("界面控制器尚未初始化");
      const restoreId = queuedFocusRestoreId;
      queuedFocusRestoreId = null;
      render(snapshot, currentThemeController, currentBackgroundController);
      if (restoreId) document.getElementById(restoreId)?.focus();
      showResult("配置已真实持久化；标记为“重启后生效”的项需按部署方式重启服务。", false);
    } catch (cause) {
      if (cause instanceof ConsoleApiError && cause.code === "config_conflict") {
        showResult("配置文件已被其他操作修改。请刷新后重新合并，旧 revision 未覆盖新文件。", true);
      } else {
        showResult(errorMessage(cause), true);
      }
    } finally {
      setButtonsDisabled(false);
    }
  };
  saveQueue = saveQueue.then(save, save);
  await saveQueue;
}

function fieldInput(field: ConfigFieldSnapshot): HTMLInputElement | HTMLSelectElement {
  const value = field.savedValue ?? field.effectiveValue;
  if (field.key === "delivery.tts.provider") {
    const select = document.createElement("select");
    select.id = inputId(field.key);
    select.dataset.configKey = field.key;
    select.disabled = !field.editable;
    const currentValue = value === null || value === undefined ? "disabled" : String(value);
    for (const [optionValue, label] of ttsProviderOptions(currentValue)) {
      const option = document.createElement("option");
      option.value = optionValue;
      option.textContent = label;
      select.append(option);
    }
    select.value = currentValue;
    return select;
  }
  if (field.key === "command.prefix") {
    const select = document.createElement("select");
    select.id = inputId(field.key);
    select.dataset.configKey = field.key;
    select.disabled = !field.editable;
    const currentValue = value === null || value === undefined ? "/" : String(value);
    const options: Array<[string, string]> = [
      ["/", "/（默认）"],
      ["#", "#"],
      ["*", "*"],
    ];
    if (!options.some(([option]) => option === currentValue)) {
      options.push([currentValue, `${currentValue}（当前自定义值）`]);
    }
    for (const [optionValue, label] of options) {
      const option = document.createElement("option");
      option.value = optionValue;
      option.textContent = label;
      select.append(option);
    }
    select.value = currentValue;
    return select;
  }
  const input = document.createElement("input");
  input.id = inputId(field.key);
  input.dataset.configKey = field.key;
  input.disabled = !field.editable;
  if (field.valueType === "boolean") {
    input.type = "checkbox";
    input.checked = value === true;
  } else {
    input.type = field.valueType === "integer" ? "number" : "text";
    input.value = Array.isArray(value) ? value.join(", ") : value === null || value === undefined ? "" : String(value);
    const range = ttsNumberRange(field.key);
    if (range) {
      input.min = String(range[0]);
      input.max = String(range[1]);
      input.step = "1";
      input.required = true;
    }
  }
  return input;
}

function decorateTtsRow(row: HTMLElement, field: ConfigFieldSnapshot): void {
  row.dataset.configFieldKey = field.key;
  if (field.key.startsWith("delivery.tts.qwen_")) row.dataset.ttsQwenField = "true";
}

/** 关闭 Provider 只做视觉提示，字段始终保持可编辑且不会生成清除操作。 */
function bindTtsProviderState(): void {
  const provider = document.getElementById(inputId("delivery.tts.provider"));
  if (!(provider instanceof HTMLSelectElement)) return;
  const refresh = (): void => {
    const disabled = provider.value === "disabled";
    for (const row of document.querySelectorAll<HTMLElement>("[data-tts-qwen-field='true']")) {
      row.classList.toggle("config-row-muted", disabled);
    }
  };
  provider.addEventListener("change", refresh);
  refresh();
}

function inputValue(input: HTMLInputElement | HTMLSelectElement, field: ConfigFieldSnapshot): unknown {
  if (field.valueType === "boolean") return input instanceof HTMLInputElement && input.checked;
  if (field.valueType === "integer") return Number.parseInt(input.value, 10);
  if (field.valueType === "string_list") return input.value.split(",").map((value) => value.trim()).filter(Boolean);
  return input.value.trim();
}

function configInput(key: string): HTMLInputElement | HTMLSelectElement {
  const value = document.getElementById(inputId(key));
  if (!(value instanceof HTMLInputElement) && !(value instanceof HTMLSelectElement)) {
    throw new Error(`缺少配置输入 #${inputId(key)}`);
  }
  return value;
}

function isEmptyInputValue(value: unknown): boolean {
  return value === "" || (Array.isArray(value) && value.length === 0);
}

function meta(field: ConfigFieldSnapshot): HTMLElement {
  const value = document.createElement("span");
  value.className = "field-meta";
  const flags = [sourceLabel(field.source), field.applyMode === "restart" ? "重启后生效" : "立即生效"];
  if (field.overridden) flags.push("已覆盖 .env");
  if (field.pendingRestart) flags.push("等待重启");
  if (!field.editable) flags.push("只读");
  value.textContent = flags.join(" · ");
  return value;
}

function sourceLabel(source: string): string {
  return ({ environment: "环境变量", managed_toml: "runtime.toml", agent_toml: "agent.toml", encrypted_secret: "加密存储", default: "默认值", not_configured: "未配置" } as Record<string, string>)[source] ?? source;
}

function textField(labelText: string, id: string, value: string, disabled: boolean, autosaveScope?: AutosaveScope): HTMLElement {
  const row = document.createElement("div");
  row.className = "config-row";
  const label = document.createElement("label");
  label.htmlFor = id;
  label.textContent = labelText;
  const input = document.createElement("input");
  input.id = id;
  input.type = "text";
  input.value = value;
  input.disabled = disabled;
  if (autosaveScope) input.dataset.autosaveScope = autosaveScope;
  row.append(label, input);
  return row;
}

function numberField(
  labelText: string,
  id: string,
  value: number,
  min: number,
  max: number,
  disabled: boolean,
  autosaveScope?: AutosaveScope,
): HTMLElement {
  const row = document.createElement("div");
  row.className = "config-row";
  const label = document.createElement("label");
  label.htmlFor = id;
  label.textContent = labelText;
  const input = document.createElement("input");
  input.id = id;
  input.type = "number";
  input.min = String(min);
  input.max = String(max);
  input.step = "1";
  input.value = String(value);
  input.disabled = disabled;
  if (autosaveScope) input.dataset.autosaveScope = autosaveScope;
  row.append(label, input);
  return row;
}

function selectField(
  labelText: string,
  id: string,
  value: string,
  options: Array<[string, string]>,
  disabled: boolean,
  autosaveScope?: AutosaveScope,
): HTMLElement {
  const row = document.createElement("div");
  row.className = "config-row";
  const label = document.createElement("label");
  label.htmlFor = id;
  label.textContent = labelText;
  const select = document.createElement("select");
  select.id = id;
  select.disabled = disabled;
  if (autosaveScope) select.dataset.autosaveScope = autosaveScope;
  for (const [optionValue, optionLabel] of options) {
    const option = document.createElement("option");
    option.value = optionValue;
    option.textContent = optionLabel;
    select.append(option);
  }
  select.value = value;
  row.append(label, select);
  return row;
}

function checkboxField(labelText: string, id: string, checked: boolean, disabled: boolean, autosaveScope?: AutosaveScope): HTMLElement {
  const row = document.createElement("div");
  row.className = "config-row compact-row";
  const label = document.createElement("label");
  label.htmlFor = id;
  label.textContent = labelText;
  const input = document.createElement("input");
  input.id = id;
  input.type = "checkbox";
  input.checked = checked;
  input.disabled = disabled;
  if (autosaveScope) input.dataset.autosaveScope = autosaveScope;
  row.append(label, input);
  return row;
}

function statusField(summary: string, detail: string): HTMLElement {
  const row = document.createElement("div");
  row.className = "config-row";
  const label = document.createElement("strong");
  label.textContent = summary;
  const meta = document.createElement("span");
  meta.className = "field-meta";
  meta.textContent = detail;
  row.append(label, meta);
  return row;
}

function updateTavilyCredentialStatus(backend: AgentWebSearchBackend, configured: boolean): void {
  const row = element("agent-web-search-credential-status");
  const summary = row.querySelector("strong");
  const detail = row.querySelector(".field-meta");
  if (!summary || !detail) throw new Error("缺少 Tavily 凭据状态元素");
  const notice = tavilyCredentialNotice(backend, configured);
  summary.textContent = configured ? "Tavily API Key：已配置" : "Tavily API Key：未配置";
  detail.textContent = notice || (configured
    ? "密钥保存在安全配置中心，不会写入 agent.toml 或回传浏览器。"
    : "可在“敏感凭据”中配置；未选择 Tavily 时不影响其他搜索后端。");
  row.classList.toggle("config-row-warning", notice.length > 0);
}

function badge(text: string, kind: string): HTMLElement {
  const value = document.createElement("span");
  value.className = `config-badge config-badge-${kind}`;
  value.textContent = text;
  return value;
}

function button(text: string, kind: string): HTMLButtonElement {
  const value = document.createElement("button");
  value.type = "button";
  value.className = kind;
  value.textContent = text;
  return value;
}

function inputId(key: string): string { return `config-${key.replaceAll(".", "-")}`; }
function record(value: unknown): Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {}; }
function array(value: unknown): unknown[] { return Array.isArray(value) ? value : []; }
function string(value: unknown): string { return typeof value === "string" ? value : ""; }
function positiveNumber(value: unknown, fallback: number): number { return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback; }
function integerInput(id: string): number { return Number(element(id, HTMLInputElement).value); }
function isWebSearchBackend(value: string): value is AgentWebSearchBackend { return value === "provider_native" || value === "tavily" || value === "disabled"; }
function isWebSearchTimeRange(value: string): value is NonNullable<AgentWebSearchConfig["timeRange"]> { return value === "day" || value === "week" || value === "month" || value === "year"; }
function webSearchBackendLabel(value: AgentWebSearchBackend): string {
  return ({ provider_native: "Provider 原生搜索", tavily: "Tavily", disabled: "已关闭" } as const)[value];
}

function showResult(message: string, error: boolean): void {
  const target = element("configuration-result");
  target.textContent = message;
  target.className = error ? "error" : "success";
  showToast(message, error);
}

function showConnectionTestResult(message: string, error: boolean): void {
  const target = element("connection-test-result");
  target.textContent = message;
  target.className = error ? "error" : "success";
  showToast(message, error);
}

/** 右上角浮层提醒；进行中的消息不设置自动隐藏，避免转圈提示被提前关掉。 */
function showToast(message: string, error: boolean): void {
  const toast = element("console-toast");
  toast.textContent = message;
  toast.className = `console-toast ${error ? "console-toast-error" : "console-toast-success"}`;
  toast.hidden = false;
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  if (!message.startsWith("正在")) {
    toastTimer = window.setTimeout(() => {
      toast.hidden = true;
      toastTimer = undefined;
    }, 8_000);
  }
}

function errorMessage(cause: unknown): string { return cause instanceof Error ? cause.message : "配置操作失败"; }

function setButtonsDisabled(disabled: boolean): void {
  for (const id of ["save-public-config", "save-secret-config", "save-agent-config", "validate-config", "test-provider-connection"]) {
    element(id, HTMLButtonElement).disabled = disabled;
  }
  for (const button of document.querySelectorAll<HTMLButtonElement>(".tool-whitelist-save")) {
    button.disabled = disabled || current?.agent?.editable !== true;
  }
  for (const button of document.querySelectorAll<HTMLButtonElement>(".provider-action")) {
    button.disabled = disabled || current?.agent?.editable !== true;
  }
}

function element<T extends HTMLElement>(id: string, constructor?: { new(): T }): T {
  const value = document.getElementById(id);
  if (!value || (constructor && !(value instanceof constructor))) throw new Error(`缺少页面元素 #${id}`);
  return value as T;
}
