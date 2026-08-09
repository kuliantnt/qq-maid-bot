import type { ConfigFieldSnapshot, ConfigurationSnapshot } from "../../types.js";
import { updateRuntimeConfiguration } from "../../api.js";
import { appendGroupedFields, button, configInput, element, fieldInput, inputId, inputValue, isEmptyInputValue, meta, string } from "./fields.js";
import { configFieldLabel } from "./navigation.js";
import { current, runSave } from "./state.js";
import { decorateTtsRow, parseTtsNumberValue, ttsNumberRange } from "./tts.js";
import { errorMessage, showResult } from "./ui.js";

export function renderPublicFields(snapshot: ConfigurationSnapshot): void {
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

export async function savePublicFields(): Promise<void> {
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

export async function removePublicField(key: string): Promise<void> {
  if (!current) return;
  // 用户点击“恢复未保存值”即明确放弃该字段的页面输入：重建后该字段应显示服务端默认值，
  // 因此把该输入排除在恢复集合之外。
  await runSave(
    async () => updateRuntimeConfiguration(current!.revision, [{ action: "remove", key }]),
    new Set([`id:${inputId(key)}`]),
  );
}

