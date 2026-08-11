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
      can_delete: true,
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
    const createTarget = document.getElementById("memory-create-target");
    createTarget.value = "memory_target:v1:target-1";
    loadMore.onclick();
    await flushMicrotasks();
    assert.deepEqual(targetCalls(calls).map(({ body }) => body.page), [1, 2]);
    assert.equal(createTarget.value, "memory_target:v1:target-1");
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

test("Memory 筛选和翻页不重新请求 target 页面，显式刷新会更新 target", async () => {
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
    assert.equal(targetCalls(calls).length, 2);
    assert.equal(memoryCalls(calls).length, 4);
  });
});

test("写请求期间普通刷新后仍按真实结果重新同步列表", async () => {
  setupMemoryPage();
  let resolveArchive;
  let listRequest = 0;
  const archived = rawMemory({
    status: "archived",
    capabilities: { can_update: false, can_archive: false, can_restore: true, can_delete: false },
  });
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/targets")) return targetPage(1, 1, [rawTarget()], 1);
    if (path.endsWith("/archive")) {
      return new Promise((resolve) => {
        resolveArchive = () => resolve(jsonResponse({ ok: true, data: { memory: archived } }));
      });
    }
    if (path.endsWith("/list")) {
      listRequest += 1;
      return listRequest >= 3 ? memoryPage([archived]) : memoryPage([rawMemory()]);
    }
    return memoryPage();
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    window.confirm = () => true;
    buttonWithText(document.getElementById("memory-list"), "归档").onclick();
    await flushMicrotasks();
    assert.equal(typeof resolveArchive, "function");

    document.getElementById("memory-refresh").onclick();
    await flushMicrotasks();
    assert.equal(listRequest, 2);
    resolveArchive();
    await flushMicrotasks();
    assert.equal(listRequest, 3);
    assert.match(treeText(document.getElementById("memory-list")), /ARCHIVED/);
  });
});

test("刷新前保存第二页账号、群组和用户 opaque 筛选", async () => {
  setupMemoryPage();
  const first = rawTarget("memory_target:v1:target-1", {
    account_ref: "memory_account:v1:first",
    group_ref: "memory_group:v1:first",
    subject_ref: "memory_subject:v1:first",
  });
  const second = rawTarget("memory_target:v1:target-2", {
    scope: "group_profile",
    account_ref: "memory_account:v1:second",
    group_ref: "memory_group:v1:second",
    subject_ref: "memory_subject:v1:second",
  });
  let targetRequest = 0;
  const listBodies = [];
  await withMemoryFetch(async (path, body) => {
    if (path.endsWith("/targets")) {
      targetRequest += 1;
      return body.page === 2
        ? targetPage(2, 2, [second], 2)
        : targetPage(1, 2, [first], 2);
    }
    if (path.endsWith("/list")) {
      listBodies.push(body);
      return memoryPage([rawMemory()]);
    }
    return memoryPage();
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    document.getElementById("memory-target-load-more").onclick();
    await flushMicrotasks();
    document.getElementById("memory-account-filter").value = second.account_ref;
    document.getElementById("memory-group-filter").value = second.group_ref;
    document.getElementById("memory-user-filter").value = second.subject_ref;

    document.getElementById("memory-refresh").onclick();
    await flushMicrotasks();
    assert.equal(targetRequest, 3);
    assert.equal(listBodies.at(-1).account_ref, second.account_ref);
    assert.equal(listBodies.at(-1).group_ref, second.group_ref);
    assert.equal(listBodies.at(-1).subject_ref, second.subject_ref);
    assert.equal(document.getElementById("memory-account-filter").value, second.account_ref);
    assert.equal(document.getElementById("memory-group-filter").value, second.group_ref);
    assert.equal(document.getElementById("memory-user-filter").value, second.subject_ref);
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
    assert.deepEqual(document.getElementById("memory-list").querySelectorAll("button").map((button) => button.textContent), ["纠正内容", "归档", "永久删除"]);
    buttonWithText(document.getElementById("memory-list"), "归档").onclick();
    await flushMicrotasks();
    assert.match(document.getElementById("memory-result").textContent, /版本已变化/);
    assert.equal(document.getElementById("memory-result").textContent.includes("已由服务端确认归档"), false);
  });
});

test("Memory 删除 prepare 后取消不会 commit", async () => {
  setupMemoryPage();
  let prepares = 0;
  let commits = 0;
  const profile = rawTarget();
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/prepare")) {
      prepares += 1;
      return jsonResponse({ ok: true, data: {
        confirmation_token: "memory_confirmation:v1:delete-1",
        operation: "delete_memory",
        target: profile,
        affected_count: 1,
        expires_at: 999,
      } });
    }
    if (path.endsWith("/commit")) commits += 1;
    if (path.endsWith("/targets")) return targetPage(1, 1);
    return memoryPage([rawMemory()]);
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    window.confirm = () => false;
    buttonWithText(document.getElementById("memory-list"), "永久删除").onclick();
    await flushMicrotasks();
    assert.equal(prepares, 1);
    assert.equal(commits, 0);
  });
});

test("Memory 删除成功后刷新列表，409 不显示成功文案", async () => {
  setupMemoryPage();
  let mode = "success";
  let prepares = 0;
  let commits = 0;
  let deleted = false;
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/targets")) return targetPage(1, 1);
    if (path.endsWith("/prepare")) {
      prepares += 1;
      return jsonResponse({ ok: true, data: {
        confirmation_token: `memory_confirmation:v1:delete-${prepares}`,
        operation: "delete_memory",
        target: rawTarget(),
        affected_count: 1,
        expires_at: 999,
      } });
    }
    if (path.endsWith("/commit")) {
      commits += 1;
      if (mode === "success") deleted = true;
      return mode === "success"
        ? jsonResponse({ ok: true, data: {
          operation: "delete_memory",
          target: rawTarget(),
          affected_count: 1,
          capabilities: { can_clear_target: true, can_disable_group_profile: false },
          deleted: true,
          memory_ref: "memory:v1:item-1",
        } })
        : jsonResponse({ ok: false, error: { code: "conflict", message: "删除时版本已变化" } }, 409);
    }
    return mode === "success" && deleted ? memoryPage([]) : memoryPage([rawMemory()]);
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    window.confirm = () => true;
    buttonWithText(document.getElementById("memory-list"), "永久删除").onclick();
    await flushMicrotasks();
    assert.equal(prepares, 1);
    assert.equal(commits, 1);
    assert.equal(document.getElementById("memory-list").children[0].textContent, "当前筛选没有可展示的记忆。");

    mode = "conflict";
    disposeMemory();
    await initializeMemory();
    await flushMicrotasks();
    buttonWithText(document.getElementById("memory-list"), "永久删除").onclick();
    await flushMicrotasks();
    assert.match(document.getElementById("memory-result").textContent, /删除时版本已变化/);
    assert.equal(document.getElementById("memory-result").textContent.includes("已由服务端确认删除"), false);
    assert.equal(prepares, 2);
    assert.equal(commits, 2);
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

test("Memory 详情请求在登出后返回不会弹出旧正文", async () => {
  setupMemoryPage();
  let resolveGet;
  const getResponse = new Promise((resolve) => { resolveGet = resolve; });
  let promptCalls = 0;
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/targets")) return targetPage(1, 1);
    if (path.endsWith("/get")) return getResponse;
    return memoryPage([rawMemory()]);
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    window.prompt = () => {
      promptCalls += 1;
      return "不应出现的旧正文";
    };
    buttonWithText(document.getElementById("memory-list"), "纠正内容").onclick();
    await flushMicrotasks();
    disposeMemory();
    resolveGet(jsonResponse({ ok: true, data: rawMemory({ content: "旧会话正文" }) }));
    await flushMicrotasks();
    assert.equal(promptCalls, 0);
    assert.equal(document.getElementById("memory-result").textContent, "");
  });
});

test("Memory 刷新使用服务端最新 target capability", async () => {
  setupMemoryPage();
  const disabled = rawTarget("memory_target:v1:profile-1", {
    scope: "group_profile",
    capabilities: { can_clear_target: true, can_disable_group_profile: false },
  });
  const enabled = rawTarget("memory_target:v1:profile-1", {
    scope: "group_profile",
    capabilities: { can_clear_target: true, can_disable_group_profile: true },
  });
  let targetRequest = 0;
  await withMemoryFetch(async (path) => {
    if (path.endsWith("/targets")) {
      targetRequest += 1;
      return targetPage(1, 1, [targetRequest === 1 ? disabled : enabled]);
    }
    return memoryPage();
  }, async () => {
    await initializeMemory();
    await flushMicrotasks();
    assert.equal(buttonWithText(document.getElementById("memory-targets"), "停止画像"), undefined);
    document.getElementById("memory-refresh").onclick();
    await flushMicrotasks();
    assert.equal(targetRequest, 2);
    assert.ok(buttonWithText(document.getElementById("memory-targets"), "停止画像"));
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
