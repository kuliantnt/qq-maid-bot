import test, { afterEach } from "node:test";
import assert from "node:assert/strict";

import { setUnauthorizedHandler } from "../dist/api.js";
import { disposeMemory, initializeMemory } from "../dist/views/memory/memory.js";
import { clearDomGlobals, createFakeDom, flushMicrotasks, installDomGlobals, jsonResponse } from "./helpers/fake-dom.mjs";

const TARGET_REF = "memory_target:v1:target-1";

function rawTarget(targetRef = TARGET_REF, overrides = {}) {
  const scope = overrides.scope ?? "personal";
  return {
    target_ref: targetRef,
    scope,
    platform: "qq_official",
    account_ref: "memory_account:v1:account-1",
    group_ref: scope === "personal" ? null : "memory_group:v1:group-1",
    subject_ref: scope === "group_profile" ? "memory_subject:v1:subject-1" : null,
    capabilities: {
      can_clear_target: true,
      can_disable_group_profile: scope === "group_profile",
    },
    ...overrides,
  };
}

function rawMemory(overrides = {}) {
  const target = overrides.target ?? rawTarget();
  return {
    memory_ref: "memory:v1:item-1",
    target,
    version: 1,
    content: "服务端确认的记忆内容",
    kind: target.scope,
    category: "note",
    visibility: target.scope === "personal" ? "private" : "group_members",
    status: "active",
    pinned: false,
    created_at: "2026-08-10T10:00:00",
    updated_at: null,
    last_confirmed_at: null,
    source_type: "manual_import",
    capabilities: {
      can_update: true,
      can_archive: true,
      can_restore: false,
    },
    ...overrides,
  };
}

function memoryPage(items = [], overrides = {}) {
  return jsonResponse({
    ok: true,
    data: {
      items,
      page: 1,
      page_size: 20,
      total: items.length,
      total_pages: 1,
      ...overrides,
    },
  });
}

function targetPage(page, totalPages, items = [], total = items.length) {
  return jsonResponse({
    ok: true,
    data: { items, page, page_size: 100, total, total_pages: totalPages },
  });
}

function setupMemoryPage() {
  const previousDocument = globalThis.document;
  if (previousDocument && globalThis.HTMLElement) disposeMemory();
  const fake = createFakeDom();
  installDomGlobals(fake);
  Object.defineProperty(fake.FakeHTMLElement.prototype, "childElementCount", {
    configurable: true,
    get() { return this.children.length; },
  });
  globalThis.HTMLFormElement = fake.FakeHTMLElement;
  globalThis.HTMLTextAreaElement = fake.FakeHTMLElement;
  globalThis.Option = class TestOption extends fake.FakeHTMLOptionElement {
    constructor(text, value) {
      super("option");
      this.textContent = text;
      this.value = value;
    }
  };
  document.body = document.createElement("div");
  const selectIds = new Set([
    "memory-kind-filter",
    "memory-status-filter",
    "memory-type-filter",
    "memory-pinned-filter",
    "memory-account-filter",
    "memory-group-filter",
    "memory-user-filter",
    "memory-visibility-filter",
    "memory-create-target",
    "memory-create-type",
    "memory-create-visibility",
  ]);
  const inputIds = new Set(["memory-query-filter", "memory-platform-filter", "memory-create-pinned"]);
  const buttonIds = new Set(["memory-refresh", "memory-filter-submit", "memory-filter-reset"]);
  for (const id of [
    "memory-refresh",
    "memory-filter-submit",
    "memory-filter-reset",
    "memory-kind-filter",
    "memory-status-filter",
    "memory-type-filter",
    "memory-pinned-filter",
    "memory-query-filter",
    "memory-platform-filter",
    "memory-account-filter",
    "memory-group-filter",
    "memory-user-filter",
    "memory-visibility-filter",
    "memory-create-form",
    "memory-create-target",
    "memory-create-type",
    "memory-create-visibility",
    "memory-create-content",
    "memory-create-pinned",
    "memory-result",
    "memory-targets",
    "memory-list",
    "memory-pagination",
  ]) {
    const tag = selectIds.has(id) ? "select" : inputIds.has(id) ? "input" : buttonIds.has(id) ? "button" : "div";
    const element = document.registerStaticId(id, tag);
    if (id === "memory-create-form") element.reset = () => undefined;
  }
  document.getElementById("memory-kind-filter").value = "all";
  document.getElementById("memory-status-filter").value = "active";
  document.getElementById("memory-pinned-filter").value = "all";
  document.getElementById("memory-visibility-filter").value = "all";
  document.getElementById("memory-create-type").value = "note";
  globalThis.window.confirm = () => false;
  globalThis.window.prompt = () => null;
  return fake;
}

function cleanupMemoryPage() {
  if (globalThis.document && globalThis.HTMLElement) disposeMemory();
  delete globalThis.Option;
  delete globalThis.HTMLFormElement;
  delete globalThis.HTMLTextAreaElement;
  clearDomGlobals();
}

afterEach(() => {
  setUnauthorizedHandler(null);
  cleanupMemoryPage();
});

async function withMemoryFetch(handler, callback) {
  const calls = [];
  globalThis.fetch = async (input, init = {}) => {
    const path = String(input);
    const body = init.body ? JSON.parse(String(init.body)) : {};
    calls.push({ path, body });
    return handler(path, body, calls);
  };
  try {
    return await callback(calls);
  } finally {
    delete globalThis.fetch;
  }
}

function targetCalls(calls) {
  return calls.filter(({ path }) => path.endsWith("/memories/targets"));
}

function memoryCalls(calls) {
  return calls.filter(({ path }) => path.endsWith("/memories/list"));
}

function buttonWithText(container, text) {
  return container.querySelectorAll("button").find((button) => button.textContent === text);
}

function treeText(element) {
  return [element.textContent ?? "", ...(element.children ?? []).map(treeText)].join("");
}

test("Memory targets 只加载第一页，点击加载更多才请求下一页", async () => {
  setupMemoryPage();
  await withMemoryFetch(async (path, body) => {
    if (path.endsWith("/targets")) return targetPage(body.page, 3, [rawTarget(`memory_target:v1:target-${body.page}`)], 3);
    return memoryPage([rawMemory()]);
  }, async (calls) => {
    await initializeMemory();
    await flushMicrotasks();
    assert.deepEqual(targetCalls(calls).map(({ body }) => body.page), [1]);
    const loadMore = document.getElementById("memory-target-load-more");
    assert.ok(loadMore);
    loadMore.onclick();
    await flushMicrotasks();
    assert.deepEqual(targetCalls(calls).map(({ body }) => body.page), [1, 2]);
  });
});

test("后续 target 页失败不影响已经成功的 Memory 列表", async () => {
  setupMemoryPage();
  await withMemoryFetch(async (path, body) => {
    if (path.endsWith("/targets")) {
      if (body.page === 2) throw new Error("后续范围暂不可用");
      return targetPage(1, 2, [rawTarget()], 2);
    }
    return memoryPage([rawMemory()]);
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    document.getElementById("memory-target-load-more").onclick();
    await flushMicrotasks();
    assert.match(treeText(document.getElementById("memory-list")), /服务端确认的记忆内容/);
    assert.match(treeText(document.getElementById("memory-targets")), /授权范围加载失败/);
  });
});

test("Memory 刷新、筛选和翻页不重新请求 target 页面", async () => {
  setupMemoryPage();
  await withMemoryFetch(async (path, body) => {
    if (path.endsWith("/targets")) return targetPage(1, 2, [rawTarget()], 2);
    return memoryPage([rawMemory()], { page: body.page, total: 2, total_pages: 2 });
  }, async (calls) => {
    await initializeMemory();
    await flushMicrotasks();
    document.getElementById("memory-pagination").children[2].onclick();
    await flushMicrotasks();
    document.getElementById("memory-query-filter").value = "筛选";
    document.getElementById("memory-filter-submit").onclick();
    await flushMicrotasks();
    document.getElementById("memory-refresh").onclick();
    await flushMicrotasks();
    assert.equal(targetCalls(calls).length, 1);
    assert.equal(memoryCalls(calls).length, 4);
  });
});

test("Memory 空状态正常显示", async () => {
  setupMemoryPage();
  await withMemoryFetch(async (path) => path.endsWith("/targets") ? targetPage(1, 1) : memoryPage(), async () => {
    await initializeMemory();
    await flushMicrotasks();
    assert.equal(document.getElementById("memory-list").children[0].textContent, "当前筛选没有可展示的记忆。");
  });
});

test("Memory API error 状态显示重试而不是空成功", async () => {
  setupMemoryPage();
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/targets")) return targetPage(1, 1);
    return jsonResponse({ ok: false, error: { code: "unavailable", message: "Memory 服务暂不可用" } }, 503);
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    assert.match(treeText(document.getElementById("memory-list")), /Memory 列表加载失败/);
    assert.equal(document.getElementById("memory-result").classList.contains("error"), true);
  });
});

test("Memory 刷新失败会清理旧分页并禁用写控件", async () => {
  setupMemoryPage();
  let failList = false;
  await withMemoryFetch(async (path, body) => {
    if (path.endsWith("/targets")) return targetPage(1, 1, [rawTarget()], 1);
    if (failList) return jsonResponse({ ok: false, error: { code: "unavailable", message: "Memory 服务暂不可用" } }, 503);
    return memoryPage([rawMemory()], { page: body.page, total: 2, total_pages: 2 });
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    const pagination = document.getElementById("memory-pagination");
    const oldNext = pagination.children[2];
    assert.ok(oldNext);
    assert.equal(oldNext.disabled, false);

    failList = true;
    document.getElementById("memory-refresh").onclick();
    await flushMicrotasks();

    assert.equal(pagination.children.length, 0);
    assert.equal(oldNext.disabled, true);
    assert.equal(oldNext.onclick, null);
    assert.equal(document.getElementById("memory-create-target").disabled, true);
    assert.equal(document.getElementById("memory-create-content").disabled, true);
    assert.equal(buttonWithText(document.getElementById("memory-targets"), "清空此范围").disabled, true);
    assert.match(treeText(document.getElementById("memory-list")), /Memory 列表加载失败/);
  });
});

test("Memory 409 conflict 不显示成功文案", async () => {
  setupMemoryPage();
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/targets")) return targetPage(1, 1);
    if (path.endsWith("/archive")) return jsonResponse({ ok: false, error: { code: "conflict", message: "版本已变化" } }, 409);
    return memoryPage([rawMemory()]);
  }, async (calls) => {
    await initializeMemory();
    await flushMicrotasks();
    window.confirm = () => true;
    assert.equal(memoryCalls(calls).length, 1);
    assert.equal(document.getElementById("memory-list").children.length, 1);
    assert.deepEqual(document.getElementById("memory-list").querySelectorAll("button").map((button) => button.textContent), ["纠正内容", "归档"]);
    buttonWithText(document.getElementById("memory-list"), "归档").onclick();
    await flushMicrotasks();
    assert.match(document.getElementById("memory-result").textContent, /版本已变化/);
    assert.equal(document.getElementById("memory-result").textContent.includes("已由服务端确认归档"), false);
  });
});

test("401/session expired 后清理 Memory 内容", async () => {
  setupMemoryPage();
  let refresh = false;
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/targets")) return targetPage(1, 1);
    if (refresh) return jsonResponse({ ok: false, error: { code: "unauthorized", message: "会话已过期" } }, 401);
    return memoryPage([rawMemory()]);
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    setUnauthorizedHandler(() => disposeMemory());
    refresh = true;
    document.getElementById("memory-refresh").onclick();
    await flushMicrotasks();
    assert.equal(document.getElementById("memory-list").children.length, 0);
    assert.equal(document.getElementById("memory-result").textContent, "");
  });
});

test("prepare 后取消不会 commit", async () => {
  setupMemoryPage();
  const profile = rawTarget("memory_target:v1:profile-1", { scope: "group_profile" });
  let prepares = 0;
  let commits = 0;
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/targets")) return targetPage(1, 1, [profile]);
    if (path.endsWith("/prepare")) {
      prepares += 1;
      return jsonResponse({ ok: true, data: {
        confirmation_token: "memory_confirmation:v1:token-1",
        operation: "disable_group_profile",
        target: profile,
        affected_count: 1,
        expires_at: 999,
      } });
    }
    if (path.endsWith("/commit")) commits += 1;
    return memoryPage();
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    window.confirm = () => false;
    buttonWithText(document.getElementById("memory-targets"), "停止画像").onclick();
    await flushMicrotasks();
    assert.equal(prepares, 1);
    assert.equal(commits, 0);
  });
});

test("prepare → confirm → commit 只提交一次并应用服务端 disabled capability", async () => {
  setupMemoryPage();
  const enabled = rawTarget("memory_target:v1:profile-1", { scope: "group_profile" });
  const disabled = rawTarget("memory_target:v1:profile-1", {
    scope: "group_profile",
    capabilities: { can_clear_target: true, can_disable_group_profile: false },
  });
  let prepares = 0;
  let commits = 0;
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/targets")) return targetPage(1, 1, [enabled]);
    if (path.endsWith("/prepare")) {
      prepares += 1;
      return jsonResponse({ ok: true, data: {
        confirmation_token: "memory_confirmation:v1:token-1",
        operation: "disable_group_profile",
        target: enabled,
        affected_count: 1,
        expires_at: 999,
      } });
    }
    if (path.endsWith("/commit")) {
      commits += 1;
      return jsonResponse({ ok: true, data: {
        operation: "disable_group_profile",
        target: disabled,
        affected_count: 1,
        capabilities: disabled.capabilities,
      } });
    }
    return memoryPage();
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    window.confirm = () => true;
    buttonWithText(document.getElementById("memory-targets"), "停止画像").onclick();
    await flushMicrotasks();
    assert.equal(prepares, 1);
    assert.equal(commits, 1);
    assert.equal(buttonWithText(document.getElementById("memory-targets"), "停止画像"), undefined);
    assert.equal(document.getElementById("memory-create-target").disabled, true);
  });
});

test("disabled group_profile 不展示停止画像且不可作为创建 target", async () => {
  setupMemoryPage();
  const disabled = rawTarget("memory_target:v1:profile-1", {
    scope: "group_profile",
    capabilities: { can_clear_target: true, can_disable_group_profile: false },
  });
  await withMemoryFetch(async (path) => path.endsWith("/targets") ? targetPage(1, 1, [disabled]) : memoryPage(), async () => {
    await initializeMemory();
    await flushMicrotasks();
    assert.equal(buttonWithText(document.getElementById("memory-targets"), "停止画像"), undefined);
    assert.equal(document.getElementById("memory-create-target").options.length, 1);
    assert.equal(document.getElementById("memory-create-target").disabled, true);
  });
});
