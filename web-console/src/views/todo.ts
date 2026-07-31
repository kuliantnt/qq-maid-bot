import { createTodo, deleteTodo, getTodo, listTodoTargets, listTodos, updateTodo } from "../api.js";
import { TARGET_PAGE_SIZE, appendTargetPage, hasMoreTargetPages, initialRefreshPage, initialTargetPager, pageAfterDelete } from "./todo-paging.js";
import type { TargetPager, TodoRefreshTrigger } from "./todo-paging.js";
import type { TodoItem, TodoStatus, TodoTargetOption } from "../types.js";

let todos: TodoItem[] = [];
let page = 1;
let pager: TargetPager = initialTargetPager();
let targetLoading = false;
let createLoadMore: HTMLButtonElement | null = null;
let filterLoadMore: HTMLButtonElement | null = null;

export async function initializeTodo(): Promise<void> {
  bindTodoControls();
  await loadMoreTargets();
  await refreshTodos("refresh");
}

function bindTodoControls(): void {
  const refresh = document.getElementById("todo-refresh");
  const filter = document.getElementById("todo-filter-submit");
  const form = document.getElementById("todo-create-form");
  if (!(refresh instanceof HTMLButtonElement) || !(filter instanceof HTMLButtonElement) || !(form instanceof HTMLFormElement)) {
    throw new Error("Todo 页面缺少必要控件");
  }
  refresh.onclick = () => void refreshTodos("refresh");
  filter.onclick = () => void refreshTodos("filter");
  form.onsubmit = (event) => {
    event.preventDefault();
    void submitTodo(form);
  };
}

async function refreshTodos(trigger: TodoRefreshTrigger = "refresh"): Promise<void> {
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
  } catch (cause) {
    showResult(cause instanceof Error ? cause.message : "Todo 刷新失败", true);
  }
}

async function submitTodo(form: HTMLFormElement): Promise<void> {
  const title = valueOf("todo-create-title").trim();
  const targetRef = valueOf("todo-create-target");
  if (!title || !targetRef) return showResult("标题和目标不能为空", true);
  const button = form.querySelector<HTMLButtonElement>("button[type=submit]");
  if (button) button.disabled = true;
  try {
    await createTodo({
      title,
      target_ref: targetRef,
      detail: valueOf("todo-create-detail").trim() || null,
      due_date: valueOf("todo-create-due-date") || null,
      due_at: valueOf("todo-create-due-at") || null,
      reminder_at: valueOf("todo-create-reminder-at") || null,
      time_precision: valueOf("todo-create-time-precision"),
      recurrence_kind: valueOf("todo-create-recurrence-kind"),
      recurrence_interval: numberValue("todo-create-recurrence-interval"),
      recurrence_unit: valueOf("todo-create-recurrence-unit"),
    });
    form.reset();
    await refreshTodos("refresh");
    showResult("Todo 已创建", false);
  } catch (cause) {
    showResult(cause instanceof Error ? cause.message : "Todo 创建失败", true);
  } finally {
    if (button) button.disabled = false;
  }
}

async function loadMoreTargets(): Promise<void> {
  if (targetLoading || (pager.page > 0 && !hasMoreTargetPages(pager))) return;
  targetLoading = true;
  try {
    pager = appendTargetPage(pager, await listTodoTargets(pager.page + 1, TARGET_PAGE_SIZE));
    renderTargets();
  } catch (cause) {
    showResult(cause instanceof Error ? cause.message : "目标加载失败", true);
  } finally {
    targetLoading = false;
  }
}

function renderTargets(): void {
  const select = document.getElementById("todo-create-target");
  if (select instanceof HTMLSelectElement) {
    select.replaceChildren();
    if (pager.items.length === 0) {
      select.append(new Option("没有可用目标", ""));
      select.disabled = true;
    } else {
      select.disabled = false;
      select.append(new Option("选择目标…", ""));
      for (const target of pager.items) select.append(new Option(targetLabel(target), target.targetRef));
    }
    ensureLoadMoreButton(select);
  }
  const filter = document.getElementById("todo-target-filter");
  if (filter instanceof HTMLSelectElement) {
    filter.replaceChildren(new Option("全部目标", ""));
    for (const target of pager.items) filter.append(new Option(targetLabel(target), target.targetRef));
    ensureLoadMoreButton(filter);
  }
}

function ensureLoadMoreButton(select: HTMLSelectElement): void {
  const button = select.id === "todo-create-target"
    ? createLoadMore ??= loadMoreTargetsButton()
    : filterLoadMore ??= loadMoreTargetsButton();
  const container = select.parentElement;
  if (container && button.parentElement !== container.parentElement) container.after(button);
  button.hidden = !hasMoreTargetPages(pager);
}

function loadMoreTargetsButton(): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "secondary";
  button.textContent = "加载更多目标…";
  button.onclick = () => void loadMoreTargets();
  return button;
}

function targetLabel(target: TodoTargetOption): string {
  return `${target.platform} · ${target.scopeType} · ${target.userId ?? target.groupId ?? target.targetRef}`;
}

function renderTodos(): void {
  const list = document.getElementById("todo-list");
  if (!(list instanceof HTMLElement)) return;
  list.replaceChildren();
  if (todos.length === 0) {
    list.append(Object.assign(document.createElement("p"), { className: "hint", textContent: "当前筛选没有 Todo。" }));
    return;
  }
  for (const todo of todos) list.append(todoCard(todo));
}

function todoCard(todo: TodoItem): HTMLElement {
  const card = document.createElement("article");
  card.className = `todo-card todo-card--${todo.status}`;
  const heading = document.createElement("div");
  heading.className = "todo-card-heading";
  const title = document.createElement("h3");
  title.textContent = todo.title;
  const status = document.createElement("span");
  status.className = `config-badge ${todo.status === "completed" ? "config-badge-ok" : "config-badge-warn"}`;
  status.textContent = todo.status === "completed" ? "已完成" : "待处理";
  heading.append(title, status);
  const meta = document.createElement("p");
  meta.className = "todo-card-meta";
  const deadline = todo.dueAt ? `截止 ${todo.dueAt}` : todo.dueDate ? `截止 ${todo.dueDate}` : "无截止日期";
  const reminder = todo.reminderAt ? `提醒 ${todo.reminderAt}` : "无提醒";
  meta.textContent = [todo.target.platform, todo.target.scopeType, deadline, reminder].join(" · ");
  card.append(heading, meta);
  if (todo.detail) {
    const detail = document.createElement("p");
    detail.className = "todo-card-detail";
    detail.textContent = todo.detail;
    card.append(detail);
  }
  const actions = document.createElement("div");
  actions.className = "todo-card-actions";
  if (todo.status === "pending") actions.append(actionButton("标记完成", () => void changeTodoStatus(todo, "completed")));
  else actions.append(actionButton("恢复待处理", () => void changeTodoStatus(todo, "pending")));
  actions.append(actionButton("删除", () => void removeTodo(todo), "danger"));
  actions.append(actionButton("查看 / 编辑", () => void openEditor(todo)));
  card.append(actions);
  return card;
}

async function changeTodoStatus(todo: TodoItem, status: TodoStatus): Promise<void> {
  try {
    await updateTodo(todo.id, { status });
    await refreshTodos("refresh");
  } catch (cause) {
    showResult(cause instanceof Error ? cause.message : "Todo 更新失败", true);
  }
}

export async function loadTodoForEdit(
  id: string,
  get: (targetId: string) => Promise<TodoItem> = getTodo,
  onError: (message: string) => void = (message) => showResult(message, true),
): Promise<TodoItem | null> {
  try {
    return await get(id);
  } catch (cause) {
    onError(cause instanceof Error ? cause.message : "Todo 加载失败");
    return null;
  }
}

async function openEditor(todo: TodoItem): Promise<void> {
  const latest = await loadTodoForEdit(todo.id);
  if (latest === null) return;
  const title = window.prompt("Todo 标题", latest.title);
  if (title === null || !title.trim()) return;
  const detail = window.prompt("Todo 详情（留空清除）", latest.detail ?? "");
  if (detail === null) return;
  const dueDate = window.prompt("截止日期 YYYY-MM-DD（留空清除）", latest.dueDate ?? "");
  if (dueDate === null) return;
  const dueAt = window.prompt("截止时间 RFC3339/本地时间（留空清除）", latest.dueAt ?? "");
  if (dueAt === null) return;
  const reminderAt = window.prompt("提醒时间 RFC3339/本地时间（留空清除）", latest.reminderAt ?? "");
  if (reminderAt === null) return;
  const timePrecision = window.prompt("时间精度：none/date/date_time", latest.timePrecision);
  if (timePrecision === null) return;
  const recurrenceKind = window.prompt("重复类型：none/interval", latest.recurrenceKind);
  if (recurrenceKind === null) return;
  const recurrenceInterval = window.prompt("重复间隔", String(latest.recurrenceInterval || ""));
  if (recurrenceInterval === null) return;
  const recurrenceUnit = window.prompt("重复单位：day/week/month", latest.recurrenceUnit);
  if (recurrenceUnit === null) return;
  try {
    await updateTodo(latest.id, {
      title: title.trim(), detail: detail.trim() || null, due_date: dueDate.trim() || null, due_at: dueAt.trim() || null,
      reminder_at: reminderAt.trim() || null, time_precision: timePrecision, recurrence_kind: recurrenceKind,
      recurrence_interval: recurrenceInterval.trim() ? Number(recurrenceInterval) : null, recurrence_unit: recurrenceUnit,
    });
    await refreshTodos("refresh");
  } catch (cause) {
    showResult(cause instanceof Error ? cause.message : "Todo 更新失败", true);
  }
}

function renderPagination(current: number, totalPages: number): void {
  const list = document.getElementById("todo-pagination");
  if (!(list instanceof HTMLElement)) return;
  list.replaceChildren();
  if (totalPages <= 1) return;
  list.append(actionButton("上一页", () => { if (page > 1) { page -= 1; void refreshTodos("refresh"); } }));
  const label = document.createElement("span"); label.textContent = `${current} / ${totalPages}`; list.append(label);
  list.append(actionButton("下一页", () => { if (page < totalPages) { page += 1; void refreshTodos("refresh"); } }));
}

async function removeTodo(todo: TodoItem): Promise<void> {
  if (!window.confirm(`确定删除 Todo「${todo.title}」吗？`)) return;
  try {
    await deleteTodo(todo.id);
    await refreshTodos("refresh");
  } catch (cause) {
    showResult(cause instanceof Error ? cause.message : "Todo 删除失败", true);
  }
}

function actionButton(label: string, action: () => void, variant = "secondary"): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = variant;
  button.textContent = label;
  button.onclick = action;
  return button;
}

function valueOf(id: string): string {
  const element = document.getElementById(id);
  return element instanceof HTMLInputElement || element instanceof HTMLSelectElement || element instanceof HTMLTextAreaElement ? element.value : "";
}

function numberValue(id: string): number | null {
  const value = valueOf(id).trim();
  return value ? Number(value) : null;
}

function showResult(message: string, error: boolean): void {
  const result = document.getElementById("todo-result");
  if (!(result instanceof HTMLElement)) return;
  result.className = `status-message ${error ? "error" : "success"}`;
  result.textContent = message;
}
