import { ConsoleApiError } from "../../api.js";
import { errorMessage, setButtonsDisabled, showResult } from "./ui.js";
import { render } from "./configuration.js";
export let current = null;
export let currentThemeController = null;
export let currentBackgroundController = null;
export let currentUserDataController = null;
export let selectedBusinessGroup = "models-providers";
export let autosaveBound = false;
export let queuedFocusRestoreId = null;
export let saveQueue = Promise.resolve(null);
export const secretSavedStates = new Map();
export const EMPTY_EXCLUDED_KEYS = new Set();
export function resetConfigurationStateForTests() {
    current = null;
    selectedBusinessGroup = "models-providers";
    autosaveBound = false;
    setQueuedFocusRestoreId(null);
    secretSavedStates.clear();
    saveQueue = Promise.resolve(null);
}
export async function runSave(action, excludeKeys = EMPTY_EXCLUDED_KEYS) {
    const save = async () => {
        setButtonsDisabled(true);
        try {
            const snapshot = await action();
            if (!snapshot)
                return null;
            if (!currentThemeController || !currentBackgroundController)
                throw new Error("界面控制器尚未初始化");
            const restoreId = queuedFocusRestoreId;
            setQueuedFocusRestoreId(null);
            // 保存完成后会重建配置 DOM；重建前先完整收集所有 input/select 的当前值
            // （含 checkbox、select 与 Secret 输入），重建后恢复，避免覆盖其他字段未保存的输入。
            const captured = captureConfigurationInputState();
            render(snapshot, currentThemeController, currentBackgroundController, currentUserDataController);
            const restoredExcluded = typeof excludeKeys === "function" ? excludeKeys() : excludeKeys;
            restoreConfigurationInputState(captured, restoredExcluded);
            if (restoreId)
                document.getElementById(restoreId)?.focus();
            showResult("配置已真实持久化；标记为“重启后生效”的项需按部署方式重启服务。", false);
            return snapshot;
        }
        catch (cause) {
            if (cause instanceof ConsoleApiError && cause.code === "config_conflict") {
                showResult("配置文件已被其他操作修改。请刷新后重新合并，旧 revision 未覆盖新文件。", true);
            }
            else {
                showResult(errorMessage(cause), true);
            }
            return null;
        }
        finally {
            setButtonsDisabled(false);
        }
    };
    saveQueue = saveQueue.then(save, save);
    return saveQueue;
}
export function captureConfigurationInputState() {
    const captured = new Map();
    const root = document.getElementById(CONFIGURATION_ROOT_ID);
    if (!root)
        return captured;
    for (const input of root.querySelectorAll("input, select")) {
        if (input instanceof HTMLInputElement && input.type === "file")
            continue;
        const key = inputCaptureKey(input);
        if (!key)
            continue;
        captured.set(key, input instanceof HTMLSelectElement || input.type !== "checkbox" ? input.value : input.checked);
    }
    return captured;
}
export function inputCaptureKey(input) {
    if (input.id)
        return `id:${input.id}`;
    if (input.dataset.clearKey)
        return `clear:${input.dataset.clearKey}`;
    if (input instanceof HTMLInputElement && input.type === "checkbox") {
        if (input.dataset.agentTool)
            return `tool:${input.dataset.agentTool}:${input.value}`;
        const name = input.getAttribute("name");
        if (name === "console-theme" || name === "console-background")
            return `${name}:${input.value}`;
    }
    return null;
}
/**
 * 重建后恢复输入状态。`excludeKeys` 中的键保持重建后的服务端快照值（例如显式清除的
 * Secret、用户主动“恢复未保存值”的字段），避免把用户已决定丢弃的旧值再写回页面。
 */
export function restoreConfigurationInputState(captured, excludeKeys) {
    const root = document.getElementById(CONFIGURATION_ROOT_ID);
    if (!root)
        return;
    for (const input of root.querySelectorAll("input, select")) {
        if (input instanceof HTMLInputElement && input.type === "file")
            continue;
        const key = inputCaptureKey(input);
        if (!key || excludeKeys.has(key) || !captured.has(key))
            continue;
        const value = captured.get(key);
        if (value === undefined)
            continue;
        if (input instanceof HTMLSelectElement || input.type !== "checkbox") {
            // select 只恢复仍然存在的选项；选项列表随数据变化时以重建后的快照为准。
            if (input instanceof HTMLSelectElement && ![...input.options].some((option) => option.value === value))
                continue;
            input.value = String(value);
        }
        else {
            input.checked = value === true;
        }
    }
}
export const CONFIGURATION_ROOT_ID = "configuration";
export function setCurrent(value) { current = value; }
export function setSelectedBusinessGroup(value) { selectedBusinessGroup = value; }
export function setAutosaveBound(value) { autosaveBound = value; }
export function setQueuedFocusRestoreId(value) { queuedFocusRestoreId = value; }
export function setCurrentThemeController(value) { currentThemeController = value; }
export function setCurrentBackgroundController(value) { currentBackgroundController = value; }
export function setCurrentUserDataController(value) { currentUserDataController = value; }
