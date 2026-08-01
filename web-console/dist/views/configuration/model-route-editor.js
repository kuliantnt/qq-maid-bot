/**
 * 模型候选路线 Chip 编辑器。
 *
 * 视觉上以独立 Chip 展示每个 provider:model 候选（完整文本、删除、拖动/上移下移排序），
 * 底层始终同步一个隐藏的逗号分隔输入框（data-autosaveScope="agent"）作为值与自动保存的载体，
 * 因此现有 saveAgent / set_model_route / 后端校验契约保持不变。
 */
/** 归一化候选列表：去首尾空白、过滤空值、按精确匹配去重，保持顺序。 */
export function normalizeCandidates(values) {
    const seen = new Set();
    const result = [];
    for (const raw of values) {
        const value = raw.trim();
        if (!value)
            continue;
        if (seen.has(value))
            continue;
        seen.add(value);
        result.push(value);
    }
    return result;
}
/** 候选格式校验：必须包含冒号，且 provider 与 model 均非空。 */
export function isMalformedCandidate(value) {
    const trimmed = value.trim();
    if (!trimmed)
        return true;
    const colon = trimmed.indexOf(":");
    if (colon <= 0 || colon === trimmed.length - 1)
        return true;
    return false;
}
/** 追加候选到末尾；空值、重复、非法格式返回明确错误。 */
export function addCandidate(list, candidate) {
    const value = candidate.trim();
    if (!value)
        return { list: [...list], error: "模型不能为空" };
    if (isMalformedCandidate(value))
        return { list: [...list], error: "格式应为 provider:model" };
    if (list.some((item) => item === value))
        return { list: [...list], error: "该模型已在路线中" };
    return { list: [...list, value], error: null };
}
/** 删除指定位置的候选。 */
export function removeCandidate(list, index) {
    if (index < 0 || index >= list.length)
        return [...list];
    return [...list.slice(0, index), ...list.slice(index + 1)];
}
/** 将候选从 from 移动到 to（越界时收敛到边界），保持其余顺序。 */
export function moveCandidate(list, from, to) {
    if (from < 0 || from >= list.length)
        return [...list];
    const target = Math.max(0, Math.min(to, list.length - 1));
    if (from === target)
        return [...list];
    const next = [...list];
    const moved = next.splice(from, 1)[0];
    if (moved === undefined)
        return [...list];
    next.splice(target, 0, moved);
    return next;
}
export function renderModelRouteEditor(options) {
    const wrapper = document.createElement("div");
    wrapper.className = "model-route-editor";
    const label = document.createElement("label");
    label.className = "model-route-label";
    label.textContent = options.label;
    const input = document.createElement("input");
    input.id = options.inputId;
    input.type = "text";
    input.tabIndex = -1;
    input.setAttribute("aria-hidden", "true");
    input.dataset.autosaveScope = "agent";
    input.className = "visually-hidden";
    input.value = options.candidates.join(", ");
    input.addEventListener("change", () => renderChips());
    const chips = document.createElement("div");
    chips.className = "model-route-chips";
    chips.setAttribute("role", "list");
    const status = document.createElement("p");
    status.className = "field-meta model-route-status";
    status.setAttribute("role", "status");
    status.setAttribute("aria-live", "polite");
    const commit = (next) => {
        input.value = next.join(", ");
        input.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    };
    const renderChips = () => {
        chips.replaceChildren();
        const list = normalizeCandidates(input.value.split(","));
        list.forEach((candidate, index) => {
            const chip = document.createElement("span");
            chip.className = "model-route-chip";
            chip.setAttribute("role", "listitem");
            chip.dataset.index = String(index);
            chip.dataset.candidate = candidate;
            if (!options.disabled) {
                chip.draggable = true;
                chip.setAttribute("aria-label", `候选 ${candidate}，拖动或使用上移/下移调整顺序`);
            }
            chip.addEventListener("dragstart", () => {
                chip.dataset.dragFrom = String(index);
                chip.classList.add("model-route-chip--dragging");
            });
            chip.addEventListener("dragend", () => {
                chip.classList.remove("model-route-chip--dragging");
            });
            chip.addEventListener("dragover", (event) => {
                event.preventDefault();
                chip.classList.add("model-route-chip--drag-over");
            });
            chip.addEventListener("dragleave", () => {
                chip.classList.remove("model-route-chip--drag-over");
            });
            chip.addEventListener("drop", (event) => {
                event.preventDefault();
                chip.classList.remove("model-route-chip--drag-over");
                const from = Number(chips.querySelector(".model-route-chip--dragging")?.dataset.dragFrom ?? -1);
                if (from >= 0 && from !== index) {
                    commit(moveCandidate(list, from, index));
                    renderChips();
                }
            });
            const handle = document.createElement("span");
            handle.className = "model-route-chip-handle";
            handle.textContent = "≡";
            handle.setAttribute("aria-hidden", "true");
            if (!options.disabled)
                chip.append(handle);
            const text = document.createElement("span");
            text.className = "model-route-chip-text";
            text.textContent = candidate;
            chip.append(text);
            if (!options.disabled) {
                const up = document.createElement("button");
                up.type = "button";
                up.className = "model-route-chip-move";
                up.textContent = "↑";
                up.setAttribute("aria-label", `上移 ${candidate}`);
                up.onclick = () => {
                    commit(moveCandidate(list, index, index - 1));
                    renderChips();
                };
                const down = document.createElement("button");
                down.type = "button";
                down.className = "model-route-chip-move";
                down.textContent = "↓";
                down.setAttribute("aria-label", `下移 ${candidate}`);
                down.onclick = () => {
                    commit(moveCandidate(list, index, index + 1));
                    renderChips();
                };
                const remove = document.createElement("button");
                remove.type = "button";
                remove.className = "model-route-chip-remove";
                remove.textContent = "×";
                remove.setAttribute("aria-label", `删除 ${candidate}`);
                remove.onclick = () => {
                    commit(removeCandidate(list, index));
                    renderChips();
                };
                chip.append(up, down, remove);
            }
            chips.append(chip);
        });
    };
    const addRow = document.createElement("div");
    addRow.className = "model-route-add-row";
    const addInput = document.createElement("input");
    addInput.type = "text";
    addInput.placeholder = "provider:model";
    addInput.disabled = options.disabled;
    addInput.setAttribute("aria-label", `为 ${options.label} 添加候选模型`);
    const addButton = document.createElement("button");
    addButton.type = "button";
    addButton.className = "secondary model-route-add-button";
    addButton.textContent = "添加";
    addButton.disabled = options.disabled;
    const tryAdd = () => {
        const result = addCandidate(normalizeCandidates(input.value.split(",")), addInput.value);
        if (result.error) {
            status.textContent = result.error;
            return;
        }
        status.textContent = "";
        addInput.value = "";
        commit(result.list);
        renderChips();
    };
    addButton.onclick = tryAdd;
    addInput.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
            event.preventDefault();
            tryAdd();
        }
    });
    addRow.append(addInput, addButton);
    wrapper.append(label, chips, addRow, status, input);
    renderChips();
    return wrapper;
}
