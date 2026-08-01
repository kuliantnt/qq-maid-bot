import { updateSecretConfiguration } from "../../api.js";
import { togglePasswordReveal } from "../../dom.js";
import { appendGroupedFields, element, inputId, meta } from "./fields.js";
import { configFieldLabel } from "./navigation.js";
import { EMPTY_EXCLUDED_KEYS, current, runSave, secretSavedStates } from "./state.js";
import { decorateTtsRow } from "./tts.js";
import { showResult } from "./ui.js";
export function renderSecretFields(snapshot) {
    const target = element("secret-config-fields");
    target.replaceChildren();
    appendGroupedFields(target, snapshot.fields.filter((value) => value.sensitivity === "secret"), (field) => {
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
    }, "secrets");
    const save = element("save-secret-config", HTMLButtonElement);
    save.onclick = () => void saveSecrets();
}
export function secretConfigurationChanges(fields, values, clearKeys, dirtyKeys) {
    const changes = [];
    for (const field of fields.filter((value) => value.sensitivity === "secret" && value.editable)) {
        // 只提交真正发生变化的 Secret：已成功保存的 Secret 输入仍留在页面，但不能重复提交旧 revision。
        if (!dirtyKeys.has(field.key))
            continue;
        if (clearKeys.has(field.key)) {
            changes.push({ action: "clear", key: field.key, expected_revision: field.revision ?? "missing" });
        }
        else {
            const value = values.get(field.key) ?? "";
            if (value.length > 0) {
                changes.push({ action: "replace", key: field.key, value, expected_revision: field.revision ?? "missing" });
            }
        }
    }
    return changes;
}
export async function saveSecrets() {
    if (!current)
        return;
    // Secret 的脏状态、changes 与 expected_revision 必须在保存队列真正执行时重新计算：
    // 前一个保存完成会更新 current snapshot 与 secretSavedStates，只有执行时才能拿到最新
    // revision；若在入队前就计算，两个 Secret 连续失焦时第二个请求会重复携带第一个 Secret
    // 的旧 revision，触发 config_conflict 并阻止第二个 Secret 保存。
    let excluded = EMPTY_EXCLUDED_KEYS;
    await runSave(async () => {
        const fields = current?.fields ?? [];
        const values = new Map();
        const clearKeys = new Set();
        const dirtyKeys = new Set();
        for (const field of fields.filter((value) => value.sensitivity === "secret" && value.editable)) {
            const value = element(inputId(field.key), HTMLInputElement).value;
            const clear = document.querySelector(`input[data-clear-key="${field.key}"]`);
            const clearChecked = clear?.checked === true;
            values.set(field.key, value);
            if (secretIsDirty(field, value, clearChecked)) {
                dirtyKeys.add(field.key);
                if (clearChecked)
                    clearKeys.add(field.key);
            }
        }
        const changes = secretConfigurationChanges(fields, values, clearKeys, dirtyKeys);
        if (changes.length === 0) {
            showResult("留空不会清除 secret；当前没有显式变更。", false);
            return null;
        }
        // 显式清除成功后，输入框和“显式清除”勾选都应复位到服务端状态，不能通过重建后的恢复保留旧值。
        const nextExcluded = new Set();
        for (const key of clearKeys) {
            nextExcluded.add(`clear:${key}`);
            nextExcluded.add(`id:${inputId(key)}`);
        }
        excluded = nextExcluded;
        const snapshot = await updateSecretConfiguration(changes);
        rememberSecretSavedStates(snapshot, dirtyKeys, values, clearKeys);
        return snapshot;
    }, () => excluded);
}
export function secretIsDirty(field, value, clearChecked) {
    const saved = secretSavedStates.get(field.key);
    if (saved) {
        if (clearChecked !== saved.clear)
            return true;
        return !clearChecked && value.length > 0 && value !== saved.value;
    }
    // 首次交互沿用既有语义：显式清除只在已配置时生效；非空输入视为新值；留空表示不修改。
    if (clearChecked)
        return field.configured;
    return value.length > 0;
}
/** 保存成功后按提交结果记录每个 Secret 的最新已保存状态，供后续脏判断使用。 */
export function rememberSecretSavedStates(snapshot, dirtyKeys, values, clearKeys) {
    for (const key of dirtyKeys) {
        const field = snapshot.fields.find((candidate) => candidate.key === key);
        if (!field)
            continue;
        secretSavedStates.set(key, {
            value: clearKeys.has(key) ? "" : values.get(key) ?? "",
            clear: clearKeys.has(key),
            revision: field.revision ?? null,
        });
    }
}
