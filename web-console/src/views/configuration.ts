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
import { togglePasswordReveal } from "../dom.js";
import type { ConfigFieldSnapshot, ConfigurationSnapshot, RegisteredTool } from "../types.js";

const FIELD_LABELS: Record<string, string> = {
  "command.prefix": "聊天命令前缀",
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

const FIELD_GROUPS = [
  { label: "命令设置", prefix: "command." },
  { label: "模型服务", prefix: "provider." },
  { label: "QQ 官方入口", prefix: "platform.qq_official." },
  { label: "OneBot 11 入口", prefix: "platform.onebot11." },
  { label: "微信服务号入口", prefix: "platform.wechat_service." },
  { label: "功能开关", prefix: "features." },
  { label: "天气服务", prefix: "weather." },
  { label: "Web 控制台", prefix: "console." },
  { label: "基础运行", prefix: "bootstrap." },
] as const;

const AGENT_ROUTE_LABELS: Record<string, string> = {
  private_main: "私聊主路线",
  group_main: "群聊主路线",
  aux: "辅助任务路线",
  private_search: "私聊搜索路线",
  group_search: "群聊搜索路线",
};

let current: ConfigurationSnapshot | null = null;
let toastTimer: number | undefined;

export async function initializeConfiguration(): Promise<void> {
  current = await fetchConfiguration();
  render(current);
}

function render(snapshot: ConfigurationSnapshot): void {
  current = snapshot;
  renderSummary(snapshot);
  renderPublicFields(snapshot);
  renderSecretFields(snapshot);
  renderAgent(snapshot);
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
    const label = document.createElement("label");
    label.htmlFor = inputId(field.key);
    label.textContent = FIELD_LABELS[field.key] ?? field.key;
    label.append(meta(field));
    const input = fieldInput(field);
    row.append(label, input);
    if (field.savedValue !== null && field.editable) {
      const remove = button("恢复未保存值", "secondary");
      remove.addEventListener("click", () => void removePublicField(field.key));
      row.append(remove);
    }
      return row;
    },
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
    const label = document.createElement("label");
    label.htmlFor = inputId(field.key);
    label.textContent = FIELD_LABELS[field.key] ?? field.key;
    label.append(meta(field));
    const input = document.createElement("input");
    input.id = inputId(field.key);
    input.type = "password";
    input.autocomplete = "new-password";
    input.placeholder = field.configured ? "已配置；留空表示不修改" : "尚未配置";
    input.disabled = !field.editable;
    input.dataset.configKey = field.key;
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
  const modelRoutes = record(documentValue.model_routes);
  for (const routeName of ["private_main", "group_main", "aux"]) {
    const route = record(modelRoutes[routeName]);
    target.append(textField(AGENT_ROUTE_LABELS[routeName] ?? routeName, `agent-route-${routeName}`, array(route.candidates).join(", "), !agent.editable));
  }
  const searchRoutes = record(documentValue.search_routes);
  for (const routeName of ["private_search", "group_search"]) {
    const route = record(searchRoutes[routeName]);
    target.append(textField(AGENT_ROUTE_LABELS[routeName] ?? routeName, `agent-search-${routeName}`, string(route.model), !agent.editable));
  }
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
    row.append(label, input);
    target.append(row);

    const tools = document.createElement("fieldset");
    tools.className = "tool-whitelist";
    const legend = document.createElement("legend");
    legend.textContent = `${sceneName === "private" ? "私聊" : "群聊"}工具白名单`;
    tools.append(legend);
    const selected = new Set(array(scene.enabled_tools).filter((value): value is string => typeof value === "string"));
    const registeredNames = new Set(snapshot.registeredTools.map((tool) => tool.name));
    const visibleTools = [
      ...snapshot.registeredTools,
      ...[...selected]
        .filter((name) => !registeredNames.has(name))
        .map((name) => ({ name, description: "已写入 agent.toml，但当前进程未注册此工具" })),
    ];
    if (visibleTools.length === 0) {
      const hint = document.createElement("p");
      hint.className = "hint";
      hint.textContent = "当前没有可用的已注册工具。";
      tools.append(hint);
    } else {
      const grid = document.createElement("div");
      grid.className = "tool-whitelist-grid";
      for (const tool of visibleTools) {
        grid.append(toolCheckbox(tool, sceneName, selected.has(tool.name), !agent.editable || !registeredNames.has(tool.name)));
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
    target.append(tools);
  }
  const save = element("save-agent-config", HTMLButtonElement);
  save.disabled = !agent.editable;
  save.onclick = () => void saveAgent();
}

function appendGroupedFields(
  target: HTMLElement,
  fields: ConfigFieldSnapshot[],
  row: (field: ConfigFieldSnapshot) => HTMLElement,
): void {
  const remaining = new Set(fields);
  for (const group of FIELD_GROUPS) {
    const grouped = fields.filter((field) => field.key.startsWith(group.prefix));
    if (grouped.length === 0) continue;
    target.append(fieldGroup(group.label, grouped.map(row)));
    grouped.forEach((field) => remaining.delete(field));
  }
  if (remaining.size > 0) target.append(fieldGroup("其他配置", [...remaining].map(row)));
}

function fieldGroup(label: string, rows: HTMLElement[]): HTMLElement {
  const section = document.createElement("section");
  section.className = "config-field-group";
  const heading = document.createElement("h3");
  heading.textContent = label;
  const grid = document.createElement("div");
  grid.className = "config-field-group-grid";
  grid.append(...rows);
  section.append(heading, grid);
  return section;
}

async function savePublicFields(): Promise<void> {
  if (!current) return;
  const changes: unknown[] = [];
  for (const field of current.fields.filter((value) => value.sensitivity === "public" && value.editable)) {
    const input = configInput(field.key);
    const value = inputValue(input, field);
    const baseline = field.savedValue ?? field.effectiveValue;
    // 未配置的可选字段会显示为空输入框；用户未触碰时不能把空字符串误当成新配置提交。
    if ((baseline === null || baseline === undefined) && isEmptyInputValue(value)) continue;
    if (JSON.stringify(value) !== JSON.stringify(baseline)) {
      changes.push({ action: "set", key: field.key, value });
    }
  }
  if (changes.length === 0) return showResult("没有需要保存的普通配置。", false);
  await runSave(async () => updateRuntimeConfiguration(current!.revision, changes));
}

async function removePublicField(key: string): Promise<void> {
  if (!current) return;
  await runSave(async () => updateRuntimeConfiguration(current!.revision, [{ action: "remove", key }]));
}

async function saveSecrets(): Promise<void> {
  if (!current) return;
  const changes: unknown[] = [];
  for (const field of current.fields.filter((value) => value.sensitivity === "secret" && value.editable)) {
    const input = element(inputId(field.key), HTMLInputElement);
    const clear = document.querySelector<HTMLInputElement>(`input[data-clear-key="${field.key}"]`);
    if (clear?.checked) {
      changes.push({ action: "clear", key: field.key, expected_revision: field.revision ?? "missing" });
    } else if (input.value.length > 0) {
      changes.push({ action: "replace", key: field.key, value: input.value, expected_revision: field.revision ?? "missing" });
    }
  }
  if (changes.length === 0) return showResult("留空不会清除 secret；当前没有显式变更。", false);
  await runSave(async () => updateSecretConfiguration(changes));
}

async function saveAgent(): Promise<void> {
  if (!current?.agent) return;
  const documentValue = record(current.agent.savedValue);
  const scenes = record(documentValue.scenes);
  const changes: unknown[] = [];
  for (const routeName of ["private_main", "group_main", "aux"]) {
    const candidates = element(`agent-route-${routeName}`, HTMLInputElement).value
      .split(",").map((value) => value.trim()).filter(Boolean);
    changes.push({ action: "set_model_route", name: routeName, candidates });
  }
  for (const routeName of ["private_search", "group_search"]) {
    changes.push({ action: "set_search_route", name: routeName, model: element(`agent-search-${routeName}`, HTMLInputElement).value.trim() });
  }
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

function agentSceneConfig(sceneName: string, scenes: Record<string, unknown>): Record<string, unknown> {
  return {
    ...record(scenes[sceneName]),
    tool_calling_enabled: element(`agent-tool-${sceneName}`, HTMLInputElement).checked,
    enabled_tools: [...document.querySelectorAll<HTMLInputElement>(`input[data-agent-tool="${sceneName}"]:checked`)].map((input) => input.value),
  };
}

function toolCheckbox(tool: RegisteredTool, sceneName: string, checked: boolean, disabled: boolean): HTMLElement {
  const label = document.createElement("label");
  label.className = "tool-checkbox";
  label.title = tool.description;
  const input = document.createElement("input");
  input.type = "checkbox";
  input.value = tool.name;
  input.checked = checked;
  input.disabled = disabled;
  input.dataset.agentTool = sceneName;
  const name = document.createElement("span");
  name.textContent = tool.name;
  label.append(input, name);
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
  setButtonsDisabled(true);
  try {
    const snapshot = await action();
    render(snapshot);
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
}

function fieldInput(field: ConfigFieldSnapshot): HTMLInputElement | HTMLSelectElement {
  const value = field.savedValue ?? field.effectiveValue;
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
  }
  return input;
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

function textField(labelText: string, id: string, value: string, disabled: boolean): HTMLElement {
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
  row.append(label, input);
  return row;
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
}

function element<T extends HTMLElement>(id: string, constructor?: { new(): T }): T {
  const value = document.getElementById(id);
  if (!value || (constructor && !(value instanceof constructor))) throw new Error(`缺少页面元素 #${id}`);
  return value as T;
}
