import test from "node:test";
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
import { filterResetDefaults, loadTodoForEdit } from "../dist/views/todo/todo.js";
import { todoDeadlineFields } from "../dist/views/todo/todo-form.js";

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

test("创建表单只保留一个截止日期与时间控件", async () => {
  const html = await readFile(new URL("../dist/index.html", import.meta.url), "utf8");
  assert.match(html, /截止日期与时间<input id="todo-create-deadline"[^>]*type="datetime-local">/);
  assert.doesNotMatch(html, /todo-create-due-date|todo-create-due-at|todo-create-time-precision/);
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
