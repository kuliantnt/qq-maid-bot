/** 通用供应商表单仅消费服务端受信模板；Credential 明文只存在于本次输入和请求。 */
import type { ConfigurationSnapshot } from "../../types.js";
import { testProviderConnection, updateAgentConfiguration, updateConnectionCredential } from "../../api.js";
import { current, runSave } from "./state.js";
import { inputId, record, string } from "./fields.js";
import { errorMessage, showResult } from "./ui.js";

import { appendModelManager } from "./models.js";

const recentTests = new Map<string, string>();
export function clearProviderTests(): void { recentTests.clear(); }

export function providerChange(id: string, value: Record<string, unknown>, exists = false): Record<string, unknown> {
  // 新建 key 统一 canonical 为小写；已有 key 只允许请求与 agent.toml 原始 key 精确一致。
  const pattern = exists ? /^[A-Za-z_][A-Za-z0-9_-]{0,63}$/ : /^[a-z_][a-z0-9_-]{0,63}$/;
  if (!pattern.test(id)) throw new Error(exists ? "已有 Connection ID 只能与保存配置中的原始 key 完全一致" : "Connection ID 必须为小写字母、数字、下划线或连字符");
  return { action: "set_provider", id, provider: value };
}

function hasCanonicalProvider(saved: Record<string, unknown>, id: string): boolean {
  const requested = id.toLowerCase();
  return Object.keys(saved).some(key => key.toLowerCase() === requested);
}

export function renderProviders(snapshot: ConfigurationSnapshot): HTMLElement {
  const section = node("section", "");
  section.className = "config-field-group";
  section.append(node("h3", "供应商连接"), node("p", "选择受信预设或创建自定义连接。保存后重启生效；Connection ID 创建后不能修改。"));
  section.append(createProviderDialog(snapshot));
  const grid = node("div", "");
  grid.className = "provider-card-grid";
  renderBuiltinProviders(snapshot, grid);
  const saved = record(record(snapshot.agent?.savedValue).providers);
  Object.entries(saved).sort(([left], [right]) => left.localeCompare(right)).forEach(([id, value]) => grid.append(connectionCard(snapshot, id, record(value), true)));
  section.append(grid);
  return section;
}

function createProviderDialog(snapshot: ConfigurationSnapshot): HTMLElement {
  const container = node("div", "");
  const open = node("button", "+ 新建供应商");
  open.id = "create-provider"; open.type = "button"; open.disabled = snapshot.agent?.editable !== true;
  const dialog = document.createElement("dialog");
  dialog.id = "create-provider-dialog"; dialog.className = "provider-create-dialog";
  dialog.setAttribute("aria-labelledby", "create-provider-title");
  const title = node("h3", "新建供应商"); title.id = "create-provider-title";
  const presets = snapshot.providers?.presets ?? [];
  const chooser = document.createElement("select");
  chooser.setAttribute("aria-label", "供应商预设");
  chooser.append(option("自定义连接", ""));
  presets.forEach(preset => chooser.append(option(preset.name, preset.id)));
  const creation = node("div", "");
  const refresh = (): void => {
    const preset = presets.find(preset => preset.id === chooser.value);
    creation.replaceChildren(connectionCard(snapshot, preset?.id ?? "", preset ? {
      kind: preset.kind, base_url: preset.base_url, auth_header: preset.auth_header, auth_scheme: preset.auth_scheme,
    } : { kind: "openai_compatible" }, false, () => {
      dialog.close();
      document.getElementById("create-provider")?.focus();
    }));
  };
  chooser.onchange = refresh;
  refresh();
  const cancel = node("button", "取消"); cancel.type = "button"; cancel.className = "secondary";
  cancel.onclick = () => dialog.close();
  dialog.addEventListener("close", () => open.focus());
  open.onclick = () => { dialog.showModal(); chooser.focus(); };
  dialog.append(title, chooser, creation, cancel);
  container.append(open, dialog);
  return container;
}

function renderBuiltinProviders(snapshot: ConfigurationSnapshot, grid: HTMLElement): void {
  for (const [id, name] of [["openai", "OpenAI"], ["deepseek", "DeepSeek"], ["bigmodel", "智谱 BigModel"], ["gemini", "Gemini"]]) {
    const fields = snapshot.fields.filter(field => field.key.startsWith(`provider.${id}.`));
    if (!fields.length) continue;
    const card = node("article", ""); card.className = "provider-card";
    card.append(node("h4", `${name} · ${id}`), node("p", "内置连接：保留原配置来源，字段自动保存。"));
    // 复用已有字段、revision 和保存行为，只移动 DOM；不复制密钥表单或重写持久化来源。
    for (const field of fields) {
      const row = document.getElementById(inputId(field.key))?.closest(".config-row");
      if (row) card.append(row);
    }
    appendDiagnostic(card, id!, snapshot.revision, fields.find(field => field.key.endsWith(".enabled"))?.effectiveValue !== false,
      fields.find(field => field.sensitivity === "secret")?.revision ?? "");
    grid.append(card);
  }
}

function connectionCard(snapshot: ConfigurationSnapshot, id: string, saved: Record<string, unknown>, exists: boolean, onCreated?: () => void): HTMLElement {
  const card = node("article", "");
  card.className = "provider-card";
  const editable = snapshot.agent?.editable === true;
  const running = record(record(record(snapshot.agent?.runningValue).providers)[id]);
  const credential = snapshot.providers?.credentials[id];
  const preset = snapshot.providers?.presets.find(preset => preset.id === id);
  card.id = `connection-${exists ? id : "new"}`;
  card.append(node("h4", string(saved.display_name) || preset?.name || id || "自定义连接"));
  if (exists) {
    const pending = JSON.stringify(saved) !== JSON.stringify(running) || credential?.pending_restart;
    card.append(node("p", `保存：${saved.enabled === false ? "停用" : "启用"} · 运行：${Object.keys(running).length ? running.enabled === false ? "停用" : "启用" : "未加载"} · ${pending ? "等待重启" : "已生效"}`));
    card.append(node("p", `Credential：${credential?.configured ? "已配置" : "未配置"}`));
  }
  const identity = input(card, "Connection ID", id, exists || !editable);
  const name = input(card, "显示名称", string(saved.display_name) || preset?.name || id, !editable);
  const kind = document.createElement("select");
  kind.setAttribute("aria-label", "协议 Adapter");
  kind.id = `${card.id}-kind`;
  for (const adapter of snapshot.providers?.adapters ?? []) kind.append(option(adapter, adapter));
  kind.value = string(saved.kind);
  kind.disabled = !editable;
  card.append(kind);
  const enabled = input(card, "启用供应商", "", !editable, "checkbox");
  enabled.checked = saved.enabled !== false;
  const base = input(card, "Base URL", string(saved.base_url), !editable);
  const advanced = document.createElement("details");
  advanced.id = `${card.id}-advanced`;
  advanced.append(node("summary", "请求配置"));
  const header = input(advanced, "认证 Header", string(saved.auth_header) || "Authorization", !editable);
  const scheme = input(advanced, "认证 Scheme（空表示无前缀）", saved.auth_scheme === null ? "" : string(saved.auth_scheme) || "Bearer", !editable);
  const timeout = input(advanced, "请求超时（秒，可留空）", saved.request_timeout_seconds == null ? "" : String(saved.request_timeout_seconds), !editable, "number");
  if (exists) advanced.append(node("p", `Credential 引用：${string(saved.api_key_env)}（不可修改）`));
  card.append(advanced);
  action(card, exists ? "保存连接" : "创建连接", !editable, async () => {
    const provider: Record<string, unknown> = {
      display_name: name.value.trim() || null, enabled: enabled.checked, kind: kind.value, base_url: base.value.trim(), auth_header: header.value.trim(),
      auth_scheme: scheme.value.trim() || null, request_timeout_seconds: timeout.value ? Number(timeout.value) : null,
    };
    if (kind.value === "openai_responses") provider.chat_fallback = false;
    if (!exists && hasCanonicalProvider(record(record(current?.agent?.savedValue).providers), identity.value.trim()))
      throw new Error("该 Connection ID 已存在（大小写不敏感），请使用新的 ID");
    const change = providerChange(identity.value.trim(), provider, exists);
    const resetInputs = new Set(exists ? [] : Array.from(card.querySelectorAll<HTMLInputElement | HTMLSelectElement>("input, select"), input => `id:${input.id}`));
    const result = await runSave(async () => {
      try { return await updateAgentConfiguration(current!.agent!.revision, [change]); }
      catch (error) { inlineError(card, errorMessage(error)); throw error; }
    }, resetInputs);
    if (result && !exists) onCreated?.();
  });
  if (!exists) return card;
  action(card, "删除供应商", !editable, async () => {
    if (window.confirm(`删除 ${id}？仍被路线引用时会拒绝，专属 Credential 将归档。`)) {
      await runSave(() => updateAgentConfiguration(current!.agent!.revision, [{ action: "remove_provider", id }]));
    }
  });
  if (credential?.editable) {
    const key = input(card, "新增 / 替换 API Key", "", !editable, "password");
    key.autocomplete = "new-password";
    key.dataset.transientCredential = "true";
    action(card, "保存 API Key", !editable, async () => {
      const value = key.value;
      key.value = "";
      if (!value.trim()) throw new Error("请输入 API Key");
      await runSave(() => updateConnectionCredential(id, current!.agent!.revision, credential.revision, value));
    });
    action(card, "清除 API Key", !editable, async () => {
      if (window.confirm("确定清除此 Credential？历史共享引用可能影响其他连接。")) await runSave(() => updateConnectionCredential(id, current!.agent!.revision, credential.revision, null));
    });
  } else card.append(node("p", "历史环境变量凭证需在部署环境中修改。"));
  appendDiagnostic(card, id, snapshot.agent!.revision, editable && saved.enabled !== false, credential?.revision ?? "");
  return card;
}

function appendDiagnostic(card: HTMLElement, id: string, revision: string, enabled: boolean, credentialRevision: string): void {
  appendModelManager(card, id, revision, enabled);
  const model = input(card, "测试模型 ID（将产生一次最小真实调用）", "", !enabled);
  const cacheKey = `${id}:${revision}:${credentialRevision}`;
  const status = node("p", recentTests.get(cacheKey) ?? "最近测试：尚未测试");
  status.setAttribute("role", "status");
  action(card, "测试已保存连接", !enabled, async () => {
    status.textContent = "正在测试已保存配置…";
    try {
      const result = await testProviderConnection(id, revision, model.value.trim());
      const labels: Record<string, string> = { success: "成功", failed: "失败", unknown: "无法确认", not_tested: "未测试" };
      status.textContent = `网络：${labels[String(result.network)]} · 认证：${labels[String(result.authentication)]} · 协议：${labels[String(result.adapter)]} · 模型调用：${labels[String(result.model_call)]} · ${result.elapsed_ms} ms · HTTP ${result.http_status ?? "未知"} · ${result.category}`;
      for (const key of recentTests.keys()) if (key.startsWith(`${id}:`)) recentTests.delete(key);
      recentTests.set(cacheKey, status.textContent);
    } catch (error) { status.textContent = errorMessage(error); }
  });
  card.append(status);
}

function node<K extends keyof HTMLElementTagNameMap>(tag: K, text: string): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag); element.textContent = text; return element;
}

function option(label: string, value: string): HTMLOptionElement {
  const element = node("option", label); element.value = value; return element;
}

function input(parent: HTMLElement, label: string, value: string, disabled: boolean, type = "text"): HTMLInputElement {
  const row = node("label", label);
  row.className = type === "checkbox" ? "provider-field provider-checkbox" : "provider-field";
  const field = document.createElement("input");
  field.type = type; field.value = value; field.disabled = disabled;
  if (parent.id) field.id = `${parent.id}-input-${parent.querySelectorAll("input").length}`;
  row.append(field); parent.append(row); return field;
}

function action(parent: HTMLElement, label: string, disabled: boolean, invoke: () => Promise<void>): void {
  const button = node("button", label); button.type = "button"; button.disabled = disabled;
  button.className = "secondary provider-action";
  button.onclick = async () => {
    button.disabled = true;
    try { await invoke(); } catch (error) { inlineError(parent, errorMessage(error)); showResult(errorMessage(error), true); }
    finally { button.disabled = disabled; }
  };
  parent.append(button);
}

function inlineError(parent: HTMLElement, message: string): void {
  let error = parent.querySelector<HTMLElement>('[role="alert"]');
  if (!error) { error = node("p", ""); error.setAttribute("role", "alert"); parent.append(error); }
  error.textContent = message; error.className = "error";
}
