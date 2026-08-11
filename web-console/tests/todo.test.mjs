import test, { afterEach } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

import { listTodoTargets } from "../dist/api.js";
import {
  appendTargetPage,
  hasMoreTargetPages,
  initialRefreshPage,
  initialTargetPager,
  pageAfterDelete,
} from "../dist/views/todo/todo-paging.js";
import { disposeTodo, filterResetDefaults, initializeTodo, loadTodoForEdit, refreshTodos } from "../dist/views/todo/todo.js";
import { todoDeadlineFields, todoDeadlineFromParts } from "../dist/views/todo/todo-form.js";
import { clearDomGlobals, createFakeDom, flushMicrotasks, installDomGlobals, jsonResponse } from "./helpers/fake-dom.mjs";

function targetOption(targetRef, overrides = {}) {
  return {
    target_ref: targetRef,
    platform: "onebot",
    account_id: "bot-1",
    scope_type: "private",
    user_id: "user-1",
    group_id: null,
    reminder_supported: true,
    ...overrides,
  };
}

function parsedTargetOption(targetRef) {
  return {
    targetRef,
    platform: "onebot",
    accountId: "bot-1",
    scopeType: "private",
    userId: "user-1",
    groupId: null,
    reminderSupported: true,
  };
}

function targetPageResponse(page, totalPages, items = []) {
  return {
    ok: true,
    data: {
      items,
      page,
      page_size: 100,
      total: items.length,
      total_pages: totalPages,
    },
    request_id: "test-request",
  };
}

function todoItem() {
  return {
    id: "todo-old",
    title: "旧 Todo",
    detail: null,
    due_date: null,
    due_at: null,
    reminder_at: null,
    time_precision: "none",
    recurrence_kind: "none",
    recurrence_interval_days: 0,
    recurrence_interval: 0,
    recurrence_unit: "day",
    status: "pending",
    created_at: "2026-08-10T10:00:00",
    updated_at: "2026-08-10T10:00:00",
    completed_at: null,
    target: {
      target_ref: "target-a",
      platform: "onebot",
      scope_type: "private",
      user_id: "user-1",
      group_id: null,
      account_id: "bot-1",
      reminder_supported: true,
      diagnostic: null,
    },
  };
}

function setupTodoPage() {
  const fake = createFakeDom();
  installDomGlobals(fake);
  globalThis.HTMLFormElement = fake.FakeHTMLElement;
  globalThis.HTMLDialogElement = fake.FakeHTMLElement;
  globalThis.HTMLTextAreaElement = fake.FakeHTMLElement;
  globalThis.Option = class TestOption extends fake.FakeHTMLOptionElement {
    constructor(text, value) {
      super("option");
      this.textContent = text;
      this.value = value;
    }
  };
  const selectIds = new Set([
    "todo-status-filter", "todo-time-filter", "todo-recurring-filter", "todo-target-filter",
    "todo-platform-filter", "todo-account-filter", "todo-user-filter", "todo-scope-filter",
  ]);
  const buttonIds = new Set([
    "todo-refresh", "todo-filter-submit", "todo-filter-reset", "todo-advanced-toggle",
    "todo-create-open", "todo-create-close",
  ]);
  for (const id of [
    "todo-refresh", "todo-filter-submit", "todo-filter-reset", "todo-advanced-toggle",
    "todo-create-open", "todo-create-close", "todo-advanced-filter", "todo-create-dialog",
    "todo-create-form", "todo-create-target", "todo-target-filter", "todo-list", "todo-pagination",
    "todo-result", "todo-create-error",
    ...selectIds,
  ]) {
    const tag = id === "todo-create-target" || selectIds.has(id) ? "select" : buttonIds.has(id) ? "button" : "div";
    const element = fake.document.registerStaticId(id, tag);
    if (id === "todo-create-form") element.reset = () => undefined;
  }
  return fake;
}

function cleanupTodoPage() {
  if (globalThis.document && globalThis.HTMLElement && globalThis.HTMLTextAreaElement) disposeTodo();
  delete globalThis.Option;
  delete globalThis.HTMLFormElement;
  delete globalThis.HTMLDialogElement;
  delete globalThis.HTMLTextAreaElement;
  clearDomGlobals();
}

afterEach(() => {
  cleanupTodoPage();
  delete globalThis.fetch;
});

async function withFetchMock(response, fn) {
  const calls = [];
  globalThis.fetch = async (_input, init) => {
    calls.push(JSON.parse(String(init.body)));
    return new Response(JSON.stringify(response), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };
  try {
    await fn(calls);
  } finally {
    delete globalThis.fetch;
  }
}

test("listTodoTargets 默认请求第一页并解析分页元数据", async () => {
  await withFetchMock(targetPageResponse(1, 1, [targetOption("target-a")]), async (calls) => {
    const result = await listTodoTargets();
    assert.deepEqual(calls, [{ page: 1, page_size: 100 }]);
    assert.equal(result.page, 1);
    assert.equal(result.pageSize, 100);
    assert.equal(result.total, 1);
    assert.equal(result.totalPages, 1);
    assert.equal(result.items[0].targetRef, "target-a");
    assert.equal(result.items[0].scopeType, "private");
  });
});

test("listTodoTargets 支持请求后续页", async () => {
  await withFetchMock(targetPageResponse(2, 3, [targetOption("target-b")]), async (calls) => {
    const result = await listTodoTargets(2, 100);
    assert.deepEqual(calls, [{ page: 2, page_size: 100 }]);
    assert.equal(result.page, 2);
    assert.equal(result.totalPages, 3);
  });
});

test("目标分页模型按页累积且只有未加载完时才继续", () => {
  let pager = initialTargetPager();
  assert.equal(hasMoreTargetPages(pager), false);

  pager = appendTargetPage(pager, { items: [parsedTargetOption("target-1")], page: 1, pageSize: 100, total: 250, totalPages: 3 });
  assert.equal(pager.items.length, 1);
  assert.equal(hasMoreTargetPages(pager), true);

  pager = appendTargetPage(pager, { items: [parsedTargetOption("target-2")], page: 2, pageSize: 100, total: 250, totalPages: 3 });
  assert.deepEqual(pager.items.map((item) => item.targetRef), ["target-1", "target-2"]);
  assert.equal(hasMoreTargetPages(pager), true);

  pager = appendTargetPage(pager, { items: [parsedTargetOption("target-3")], page: 3, pageSize: 100, total: 250, totalPages: 3 });
  assert.equal(pager.items.length, 3);
  assert.equal(hasMoreTargetPages(pager), false);
});

test("应用筛选把 Todo 列表重置到第一页", () => {
  assert.equal(initialRefreshPage("filter", 7), 1);
  assert.equal(initialRefreshPage("filter", 1), 1);
  assert.equal(initialRefreshPage("refresh", 3), 3);
  assert.equal(initialRefreshPage("refresh", 0), 1);
});

test("删除当前页最后一项后回退到有效页", () => {
  assert.equal(pageAfterDelete(3, 2), 2);
  assert.equal(pageAfterDelete(2, 2), 2);
  assert.equal(pageAfterDelete(2, 0), 1);
  assert.equal(pageAfterDelete(1, 0), 1);
});

test("Todo 会话清理会清空列表、分页和 target，并在重新初始化时重新加载第一页", async () => {
  setupTodoPage();
  const targetRequests = [];
  let listRequest = 0;
  globalThis.fetch = async (input, init) => {
    const path = String(input);
    const body = JSON.parse(String(init.body));
    if (path.endsWith("/targets")) {
      targetRequests.push(body.page);
      return jsonResponse(targetPageResponse(1, 2, [targetOption("target-a")]));
    }
    listRequest += 1;
    return jsonResponse({
      ok: true,
      data: { items: [todoItem()], page: 1, page_size: 50, total: 2, total_pages: 2 },
    });
  };

  await initializeTodo();
  await flushMicrotasks();
  assert.deepEqual(targetRequests, [1]);
  assert.equal(listRequest, 1);
  assert.equal(document.getElementById("todo-list").children[0].children[0].children[0].textContent, "旧 Todo");
  assert.equal(document.getElementById("todo-pagination").children.length, 3);

  disposeTodo();
  assert.equal(document.getElementById("todo-list").children.length, 0);
  assert.equal(document.getElementById("todo-pagination").children.length, 0);
  assert.equal(document.getElementById("todo-create-target").children.length, 0);
  assert.equal(document.getElementById("todo-target-filter").children.length, 0);

  await initializeTodo();
  await flushMicrotasks();
  assert.deepEqual(targetRequests, [1, 1]);
  assert.equal(listRequest, 2);
});

test("Todo 列表刷新失败会清空旧卡片和分页", async () => {
  setupTodoPage();
  let failList = false;
  globalThis.fetch = async (input, init) => {
    const path = String(input);
    if (path.endsWith("/targets")) return jsonResponse(targetPageResponse(1, 1, [targetOption("target-a")]));
    if (failList) return jsonResponse({ ok: false, error: { code: "request_failed", message: "Todo 服务暂不可用" } }, 503);
    return jsonResponse({
      ok: true,
      data: { items: [todoItem()], page: 1, page_size: 50, total: 2, total_pages: 2 },
    });
  };

  await initializeTodo();
  await flushMicrotasks();
  failList = true;
  await refreshTodos("refresh");
  assert.equal(document.getElementById("todo-list").children[0].textContent, "Todo 列表加载失败，请重试。");
  assert.equal(document.getElementById("todo-pagination").children.length, 0);
  assert.equal(document.getElementById("todo-result").textContent, "Todo 服务暂不可用");
});

test("getTodo 失败时通过 showResult 回调显示错误且不抛出", async () => {
  const messages = [];
  const todo = await loadTodoForEdit(
    "123",
    async () => { throw new Error("Todo 不存在"); },
    (message) => messages.push(message),
  );
  assert.equal(todo, null);
  assert.deepEqual(messages, ["Todo 不存在"]);
});

test("getTodo 成功时返回最新 Todo 供编辑器使用", async () => {
  const todo = await loadTodoForEdit("123", async () => ({ id: "123", title: "准备周报" }), () => {});
  assert.equal(todo.title, "准备周报");
});

test("重置筛选默认值：状态/时间/重复恢复 all，其余清空", () => {
  const defaults = filterResetDefaults();
  assert.equal(defaults["todo-status-filter"], "all");
  assert.equal(defaults["todo-time-filter"], "all");
  assert.equal(defaults["todo-recurring-filter"], "all");
  assert.equal(defaults["todo-keyword-filter"], "");
  assert.equal(defaults["todo-target-filter"], "");
  assert.equal(defaults["todo-scope-filter"], "");
  assert.equal(defaults["todo-date-start"], "");
});

test("单个截止日期时间同步生成一致的后端日期、时间和精度", () => {
  assert.deepEqual(todoDeadlineFields("2026-08-01T10:00"), {
    dueDate: "2026-08-01",
    dueAt: "2026-08-01T10:00",
    timePrecision: "date_time",
  });
  assert.deepEqual(todoDeadlineFields("2026-08-01"), {
    dueDate: "2026-08-01",
    dueAt: null,
    timePrecision: "date",
  });
  assert.deepEqual(todoDeadlineFields(null), {
    dueDate: null,
    dueAt: null,
    timePrecision: "none",
  });
});

test("创建表单的可选时间支持仅日期与日期时间两种语义", () => {
  assert.deepEqual(todoDeadlineFromParts("2026-08-01", ""), {
    dueDate: "2026-08-01",
    dueAt: null,
    timePrecision: "date",
  });
  assert.deepEqual(todoDeadlineFromParts("2026-08-01", "10:30"), {
    dueDate: "2026-08-01",
    dueAt: "2026-08-01T10:30",
    timePrecision: "date_time",
  });
});

test("创建表单把截止日期与可选时间收拢在同一字段组", async () => {
  const html = await readFile(new URL("../dist/index.html", import.meta.url), "utf8");
  assert.match(html, /todo-create-deadline-fields/);
  assert.match(html, /id="todo-create-due-date"[^>]*type="date"/);
  assert.match(html, /id="todo-create-due-time"[^>]*type="time"/);
  assert.doesNotMatch(html, /todo-create-due-at|todo-create-time-precision/);
});

test("Todo 卡片操作按钮顺序：删除恒为最后，且查看/编辑已接线", async () => {
  const { installDomGlobals, createFakeDom, flushMicrotasks } = await import("./helpers/fake-dom.mjs");
  installDomGlobals(createFakeDom());
  const { todoCard } = await import("../dist/views/todo/todo-card.js");
  const previousPrompt = globalThis.window.prompt;
  globalThis.window.prompt = () => null;
  try {
    const card = todoCard({
      id: "t-1",
      title: "测试",
      detail: null,
      dueDate: null,
      dueAt: null,
      reminderAt: null,
      timePrecision: "none",
      recurrenceKind: "none",
      recurrenceInterval: 0,
      recurrenceUnit: "day",
      status: "pending",
      createdAt: "",
      updatedAt: "",
      completedAt: null,
      target: { targetRef: "r", platform: "p", scopeType: "private", userId: "u", groupId: null, accountId: null, reminderSupported: false, diagnostic: null },
    });
    const buttons = [...card.querySelectorAll("button")].map((button) => button.textContent);
    assert.deepEqual(buttons, ["标记完成", "查看 / 编辑", "删除"]);
    const editButton = card.querySelectorAll("button")[1];
    assert.equal(typeof editButton.onclick, "function");
    editButton.onclick();
    await flushMicrotasks();
  } finally {
    globalThis.window.prompt = previousPrompt;
  }
});
