import type { ConfigFieldSnapshot } from "../../types.js";
import type { AgentWebSearchBackend, AgentWebSearchConfig } from "./web-search.js";
import type { AutosaveScope } from "./autosave.js";
import type { ConfigurationBusinessGroup } from "./navigation.js";
import { BUSINESS_GROUPS, FIELD_SECTIONS } from "./navigation.js";
import { ttsNumberRange, ttsProviderOptions } from "./tts.js";

export function appendGroupedFields(
  target: HTMLElement,
  fields: ConfigFieldSnapshot[],
  row: (field: ConfigFieldSnapshot) => HTMLElement,
  source: "runtime" | "secrets",
): void {
  const remaining = new Set(fields);
  for (const business of BUSINESS_GROUPS) {
    if (business.id === "advanced") continue;
    const sections: HTMLElement[] = [];
    for (const definition of FIELD_SECTIONS.filter((section) => section.business === business.id)) {
      const grouped = fields.filter((field) => field.key.startsWith(definition.prefix));
      if (grouped.length === 0) continue;
      const label = source === "secrets" ? `${definition.label} · 敏感凭据` : definition.label;
      sections.push(fieldGroup(label, grouped.map(row), source === "runtime" ? definition.description : undefined));
      grouped.forEach((field) => remaining.delete(field));
    }
    if (sections.length > 0) target.append(configurationGroup(business.id, source, sections));
  }
  if (remaining.size > 0) {
    const label = source === "secrets" ? "未归类敏感凭据" : "未归类受管字段";
    target.append(configurationGroup("advanced", source, fieldGroup(label, [...remaining].map(row),
      "当前前端尚未识别这些已登记字段；它们仍使用原配置键和原保存协议。")));
  }
}

export function configurationGroup(
  group: ConfigurationBusinessGroup,
  source: "runtime" | "secrets" | "agent" | "interface",
  content: HTMLElement | HTMLElement[],
): HTMLElement {
  const wrapper = document.createElement("section");
  wrapper.className = "configuration-content-group";
  wrapper.dataset.configurationGroup = group;
  wrapper.dataset.configurationSource = source;
  wrapper.append(...(Array.isArray(content) ? content : [content]));
  return wrapper;
}

export function fieldGroup(label: string, rows: HTMLElement[], description?: string): HTMLElement {
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

export function fieldInput(field: ConfigFieldSnapshot): HTMLInputElement | HTMLSelectElement {
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

export function inputValue(input: HTMLInputElement | HTMLSelectElement, field: ConfigFieldSnapshot): unknown {
  if (field.valueType === "boolean") return input instanceof HTMLInputElement && input.checked;
  if (field.valueType === "integer") return Number.parseInt(input.value, 10);
  if (field.valueType === "string_list") return input.value.split(",").map((value) => value.trim()).filter(Boolean);
  return input.value.trim();
}

export function configInput(key: string): HTMLInputElement | HTMLSelectElement {
  const value = document.getElementById(inputId(key));
  if (!(value instanceof HTMLInputElement) && !(value instanceof HTMLSelectElement)) {
    throw new Error(`缺少配置输入 #${inputId(key)}`);
  }
  return value;
}

export function isEmptyInputValue(value: unknown): boolean {
  return value === "" || (Array.isArray(value) && value.length === 0);
}

export function meta(field: ConfigFieldSnapshot): HTMLElement {
  const value = document.createElement("span");
  value.className = "field-meta";
  const flags = [sourceLabel(field.source), field.applyMode === "restart" ? "重启后生效" : "立即生效"];
  if (field.overridden) flags.push("已覆盖 .env");
  if (field.pendingRestart) flags.push("等待重启");
  if (!field.editable) flags.push("只读");
  value.textContent = flags.join(" · ");
  return value;
}

export function sourceLabel(source: string): string {
  return ({ environment: "环境变量", managed_toml: "runtime.toml", agent_toml: "agent.toml", encrypted_secret: "加密存储", default: "默认值", not_configured: "未配置" } as Record<string, string>)[source] ?? source;
}

export function textField(labelText: string, id: string, value: string, disabled: boolean, autosaveScope?: AutosaveScope): HTMLElement {
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

export function numberField(
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

export function selectField(
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

export function checkboxField(labelText: string, id: string, checked: boolean, disabled: boolean, autosaveScope?: AutosaveScope): HTMLElement {
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

export function statusField(summary: string, detail: string): HTMLElement {
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

export function badge(text: string, kind: string): HTMLElement {
  const value = document.createElement("span");
  value.className = `config-badge config-badge-${kind}`;
  value.textContent = text;
  return value;
}

export function button(text: string, kind: string): HTMLButtonElement {
  const value = document.createElement("button");
  value.type = "button";
  value.className = kind;
  value.textContent = text;
  return value;
}

export function inputId(key: string): string { return `config-${key.replaceAll(".", "-")}`; }
export function record(value: unknown): Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {}; }
export function array(value: unknown): unknown[] { return Array.isArray(value) ? value : []; }
export function string(value: unknown): string { return typeof value === "string" ? value : ""; }
export function positiveNumber(value: unknown, fallback: number): number { return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback; }
export function integerInput(id: string): number { return Number(element(id, HTMLInputElement).value); }
export function element<T extends HTMLElement>(id: string, constructor?: { new(): T }): T {
  const value = document.getElementById(id);
  if (!value || (constructor && !(value instanceof constructor))) throw new Error(`缺少页面元素 #${id}`);
  return value as T;
}

