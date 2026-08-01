import { array, element, inputId, inputValue, isEmptyInputValue, record, string } from "./fields.js";
import { agentSceneConfig, saveAgent, saveAgentScene, saveOpenCodeProvider } from "./agent-fields.js";
import { agentWebSearchInputValue, agentWebSearchKey, readAgentWebSearchConfig } from "./web-search.js";
import { autosaveBound, current, setAutosaveBound, setQueuedFocusRestoreId } from "./state.js";
import { savePublicFields } from "./public-fields.js";
import { saveSecrets, secretIsDirty } from "./secret-fields.js";
export function shouldAutosaveOnBlur(input) {
    if (input.scope === "secret") {
        if (input.clearRequested === true)
            return input.configured === true;
        return typeof input.value === "string" && input.value.length > 0;
    }
    if (input.scope === "public" && (input.baseline === null || input.baseline === undefined) && isEmptyInputValue(input.value)) {
        return false;
    }
    return JSON.stringify(input.value) !== JSON.stringify(input.baseline);
}
export function bindAutosave() {
    if (autosaveBound)
        return;
    setAutosaveBound(true);
    document.addEventListener("focusout", (event) => {
        const target = event.target;
        if (!(target instanceof HTMLInputElement) && !(target instanceof HTMLSelectElement))
            return;
        const related = event.relatedTarget;
        setQueuedFocusRestoreId(related instanceof HTMLElement && related.id ? related.id : null);
        // 点击显式保存按钮会先触发输入框 blur；此时由按钮提交，避免同一 revision 入队两次。
        if (related instanceof HTMLElement && shouldDeferAutosaveToButton(target, related))
            return;
        void autosaveBlur(target);
    });
}
export function shouldDeferAutosaveToButton(target, related) {
    if (related.id === "save-secret-config") {
        return target.dataset.autosaveScope === "secret" || target.dataset.clearKey !== undefined;
    }
    if (related.id === "save-public-config")
        return target.dataset.autosaveScope === "public";
    if (related.id === "save-agent-config")
        return target.dataset.autosaveScope === "agent";
    return target.dataset.autosaveProvider !== undefined && related.matches(".provider-action");
}
export async function autosaveBlur(target) {
    if (target.disabled || !current)
        return;
    const scene = target.dataset.autosaveScene;
    if (target.dataset.configKey) {
        const field = current.fields.find((value) => value.key === target.dataset.configKey);
        if (!field || !field.editable)
            return;
        if (field.sensitivity === "secret") {
            const clear = document.querySelector(`input[data-clear-key="${field.key}"]`);
            if (!secretIsDirty(field, target.value, clear?.checked === true))
                return;
            await saveSecrets();
            return;
        }
        const value = inputValue(target, field);
        if (!shouldAutosaveOnBlur({ scope: "public", value, baseline: field.savedValue ?? field.effectiveValue }))
            return;
        await savePublicFields();
        return;
    }
    if (target.dataset.clearKey) {
        const field = current.fields.find((value) => value.key === target.dataset.clearKey);
        if (!(target instanceof HTMLInputElement) || !field || !field.editable || !target.checked)
            return;
        if (!secretIsDirty(field, element(inputId(field.key), HTMLInputElement).value, true))
            return;
        await saveSecrets();
        return;
    }
    const providerId = target.dataset.autosaveProvider;
    if (providerId) {
        await saveOpenCodeProvider(providerId);
        return;
    }
    if (scene) {
        if (agentSceneChanged(scene))
            await saveAgentScene(scene);
        return;
    }
    if (target.dataset.autosaveScope === "agent" && agentFieldChanged(target.id))
        await saveAgent();
}
export function agentSceneChanged(sceneName) {
    if (!current?.agent)
        return false;
    const savedScenes = record(record(current.agent.savedValue).scenes);
    return shouldAutosaveOnBlur({
        scope: "agent",
        value: agentSceneConfig(sceneName, savedScenes),
        baseline: record(savedScenes[sceneName]),
    });
}
export function agentFieldChanged(id) {
    if (!current?.agent)
        return false;
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
