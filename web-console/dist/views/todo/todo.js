import { deleteTodo, getTodo, listTodoTargets, listTodos, updateTodo } from "../../api.js";
import { TARGET_PAGE_SIZE, appendTargetPage, hasMoreTargetPages, initialRefreshPage, initialTargetPager, pageAfterDelete } from "./todo-paging.js";
import { todoCard } from "./todo-card.js";
import { submitTodo } from "./todo-form.js";
export { todoRecurrenceKind } from "./todo-form.js";
let todos = [];
let page = 1;
let pager = initialTargetPager();
let targetLoading = false;
let createLoadMore = null;
let filterLoadMore = null;
export async function initializeTodo() {
    bindTodoControls();
    await loadMoreTargets();
    await refreshTodos("refresh");
}
function bindTodoControls() {
    const refresh = document.getElementById("todo-refresh");
    const filter = document.getElementById("todo-filter-submit");
    const reset = document.getElementById("todo-filter-reset");
    const advancedToggle = document.getElementById("todo-advanced-toggle");
    const advancedPanel = document.getElementById("todo-advanced-filter");
    const createOpen = document.getElementById("todo-create-open");
    const createClose = document.getElementById("todo-create-close");
    const dialog = document.getElementById("todo-create-dialog");
    const form = document.getElementById("todo-create-form");
    if (!(refresh instanceof HTMLButtonElement) || !(filter instanceof HTMLButtonElement) || !(reset instanceof HTMLButtonElement)
        || !(advancedToggle instanceof HTMLButtonElement) || !(advancedPanel instanceof HTMLElement)
        || !(createOpen instanceof HTMLButtonElement) || !(createClose instanceof HTMLButtonElement)
        || !(dialog instanceof HTMLDialogElement) || !(form instanceof HTMLFormElement)) {
        throw new Error("Todo 页面缺少必要控件");
    }
    refresh.onclick = () => void refreshTodos("refresh");
    filter.onclick = () => void refreshTodos("filter");
    reset.onclick = () => {
        for (const id of ["todo-status-filter", "todo-keyword-filter", "todo-time-filter", "todo-recurring-filter",
            "todo-target-filter", "todo-platform-filter", "todo-account-filter", "todo-user-filter", "todo-scope-filter",
            "todo-date-start", "todo-date-end"]) {
            const field = document.getElementById(id);
            if (field instanceof HTMLInputElement || field instanceof HTMLSelectElement)
                field.value = "";
        }
        syncAdvancedFilterState();
        void refreshTodos("filter");
    };
    advancedToggle.onclick = () => {
        advancedPanel.hidden = !advancedPanel.hidden;
        advancedToggle.setAttribute("aria-expanded", String(!advancedPanel.hidden));
        syncAdvancedFilterState();
    };
    createOpen.onclick = () => {
        document.getElementById("todo-create-error").textContent = "";
        dialog.showModal();
    };
    createClose.onclick = () => dialog.close();
    dialog.addEventListener("click", (event) => {
        if (event.target === dialog)
            dialog.close();
    });
    form.onsubmit = (event) => {
        event.preventDefault();
        void submitTodo(form, dialog);
    };
    syncAdvancedFilterState();
}
function syncAdvancedFilterState() {
    const toggle = document.getElementById("todo-advanced-toggle");
    if (!(toggle instanceof HTMLButtonElement))
        return;
    const active = ["todo-time-filter", "todo-recurring-filter", "todo-target-filter", "todo-platform-filter",
        "todo-account-filter", "todo-user-filter", "todo-scope-filter", "todo-date-start", "todo-date-end"]
        .some((id) => {
        const value = valueOf(id).trim();
        return value !== "" && value !== "all";
    });
    toggle.classList.toggle("todo-advanced-toggle--active", active);
    toggle.textContent = active ? "高级筛选 · 已启用" : "高级筛选";
}
export async function refreshTodos(trigger = "refresh") {
    page = initialRefreshPage(trigger, page);
    try {
        const status = valueOf("todo-status-filter");
        const keyword = valueOf("todo-keyword-filter").trim();
        const timeFilter = valueOf("todo-time-filter");
        const recurring = valueOf("todo-recurring-filter");
        const targetRef = valueOf("todo-target-filter");
        const platform = valueOf("todo-platform-filter").trim();
        const accountId = valueOf("todo-account-filter").trim();
        const userId = valueOf("todo-user-filter").trim();
        const scopeType = valueOf("todo-scope-filter");
        const dateStart = valueOf("todo-date-start");
        const dateEnd = valueOf("todo-date-end");
        const result = await listTodos({
            page,
            ...(status === "all" ? {} : { status }),
            ...(keyword ? { keyword } : {}),
            ...(timeFilter === "all" ? {} : { time_filter: timeFilter }),
            ...(recurring === "all" ? {} : { recurring: recurring === "true" }),
            ...(targetRef ? { target_ref: targetRef } : {}),
            ...(platform ? { platform } : {}),
            ...(accountId ? { account_id: accountId } : {}),
            ...(userId ? { user_id: userId } : {}),
            ...(scopeType ? { scope_type: scopeType } : {}),
            ...(dateStart && dateEnd ? { date_start: dateStart, date_end: dateEnd } : {}),
        });
        if (page > result.totalPages && page > 1) {
            page = pageAfterDelete(page, result.totalPages);
            return refreshTodos("refresh");
        }
        todos = result.items;
        renderTodos();
        renderPagination(result.page, result.totalPages);
        showResult(`${result.total} 项 Todo`, false);
    }
    catch (cause) {
        showResult(cause instanceof Error ? cause.message : "Todo 刷新失败", true);
    }
}
async function loadMoreTargets() {
    if (targetLoading || (pager.page > 0 && !hasMoreTargetPages(pager)))
        return;
    targetLoading = true;
    try {
        pager = appendTargetPage(pager, await listTodoTargets(pager.page + 1, TARGET_PAGE_SIZE));
        renderTargets();
    }
    catch (cause) {
        showResult(cause instanceof Error ? cause.message : "目标加载失败", true);
    }
    finally {
        targetLoading = false;
    }
}
function renderTargets() {
    const select = document.getElementById("todo-create-target");
    if (select instanceof HTMLSelectElement) {
        select.replaceChildren();
        if (pager.items.length === 0) {
            select.append(new Option("没有可用目标", ""));
            select.disabled = true;
        }
        else {
            select.disabled = false;
            select.append(new Option("选择目标…", ""));
            for (const target of pager.items)
                select.append(new Option(targetLabel(target), target.targetRef));
        }
        ensureLoadMoreButton(select);
    }
    const filter = document.getElementById("todo-target-filter");
    if (filter instanceof HTMLSelectElement) {
        filter.replaceChildren(new Option("全部目标", ""));
        for (const target of pager.items)
            filter.append(new Option(targetLabel(target), target.targetRef));
        ensureLoadMoreButton(filter);
    }
}
function ensureLoadMoreButton(select) {
    const button = select.id === "todo-create-target"
        ? createLoadMore ??= loadMoreTargetsButton()
        : filterLoadMore ??= loadMoreTargetsButton();
    const container = select.parentElement;
    if (container && button.parentElement !== container.parentElement)
        container.after(button);
    button.hidden = !hasMoreTargetPages(pager);
}
function loadMoreTargetsButton() {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "secondary";
    button.textContent = "加载更多目标…";
    button.onclick = () => void loadMoreTargets();
    return button;
}
function targetLabel(target) {
    return `${target.platform} · ${target.scopeType} · ${target.userId ?? target.groupId ?? target.targetRef}`;
}
export function renderTodos() {
    const list = document.getElementById("todo-list");
    if (!(list instanceof HTMLElement))
        return;
    list.replaceChildren();
    if (todos.length === 0) {
        list.append(Object.assign(document.createElement("p"), { className: "hint", textContent: "当前筛选没有 Todo。" }));
        return;
    }
    for (const todo of todos)
        list.append(todoCard(todo));
}
export async function changeTodoStatus(todo, status) {
    try {
        await updateTodo(todo.id, { status });
        await refreshTodos("refresh");
    }
    catch (cause) {
        showResult(cause instanceof Error ? cause.message : "Todo 更新失败", true);
    }
}
export async function loadTodoForEdit(id, get = getTodo, onError = (message) => showResult(message, true)) {
    try {
        return await get(id);
    }
    catch (cause) {
        onError(cause instanceof Error ? cause.message : "Todo 加载失败");
        return null;
    }
}
export async function openEditor(todo) {
}
function renderPagination(current, totalPages) {
    const list = document.getElementById("todo-pagination");
    if (!(list instanceof HTMLElement))
        return;
    list.replaceChildren();
    if (totalPages <= 1)
        return;
    list.append(actionButton("上一页", () => { if (page > 1) {
        page -= 1;
        void refreshTodos("refresh");
    } }));
    const label = document.createElement("span");
    label.textContent = `${current} / ${totalPages}`;
    list.append(label);
    list.append(actionButton("下一页", () => { if (page < totalPages) {
        page += 1;
        void refreshTodos("refresh");
    } }));
}
export async function removeTodo(todo) {
    if (!window.confirm(`确定删除 Todo「${todo.title}」吗？`))
        return;
    try {
        await deleteTodo(todo.id);
        await refreshTodos("refresh");
    }
    catch (cause) {
        showResult(cause instanceof Error ? cause.message : "Todo 删除失败", true);
    }
}
export function actionButton(label, action, variant = "secondary") {
    const button = document.createElement("button");
    button.type = "button";
    button.className = variant;
    button.textContent = label;
    button.onclick = action;
    return button;
}
export function valueOf(id) {
    const element = document.getElementById(id);
    return element instanceof HTMLInputElement || element instanceof HTMLSelectElement || element instanceof HTMLTextAreaElement ? element.value : "";
}
export function numberValue(id) {
    const value = valueOf(id).trim();
    return value ? Number(value) : null;
}
export function showResult(message, error) {
    const result = document.getElementById("todo-result");
    if (!(result instanceof HTMLElement))
        return;
    result.className = `status-message ${error ? "error" : "success"}`;
    result.textContent = message;
}
