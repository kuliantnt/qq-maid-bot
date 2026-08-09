import test from "node:test";
import assert from "node:assert/strict";

import {
  ConsoleApiError,
  archiveMemory,
  commitMemoryOperation,
  createMemory,
  getMemory,
  listMemoryTargets,
  listMemories,
  prepareMemoryOperation,
  restoreMemory,
  setCsrfToken,
  updateMemory,
} from "../dist/api.js";

const TARGET_REF = "memory_target:v1:target";
const MEMORY_REF = "memory:v1:item";

function targetSummary(overrides = {}) {
  return {
    target_ref: TARGET_REF,
    scope: "personal",
    platform: "qq_official",
    account_ref: "memory_account:v1:account",
    group_ref: null,
    subject_ref: null,
    ...overrides,
  };
}

function memoryItem(overrides = {}) {
  return {
    memory_ref: MEMORY_REF,
    version: 1,
    target: targetSummary(),
    content: "安全内容",
    kind: "personal",
    category: "note",
    visibility: "private",
    status: "active",
    pinned: false,
    created_at: "2026-08-09 10:00:00",
    updated_at: null,
    last_confirmed_at: "2026-08-09 10:00:00",
    source_type: "manual_import",
    capabilities: {
      can_update: true,
      can_archive: true,
      can_restore: false,
    },
    ...overrides,
  };
}

function jsonResponse(data, status = 200) {
  return new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json" } });
}

async function withFetchMock(handler, fn) {
  const previous = globalThis.fetch;
  const calls = [];
  globalThis.fetch = async (input, init = {}) => {
    calls.push({ input: String(input), init });
    return handler(input, init);
  };
  try {
    await fn(calls);
  } finally {
    globalThis.fetch = previous;
  }
}

test("Memory 列表发送 master 筛选字段并解析 opaque target", async () => {
  await withFetchMock(async () => jsonResponse({
    ok: true,
    data: { items: [memoryItem()], page: 2, page_size: 20, total: 1, total_pages: 1 },
  }), async (calls) => {
    const result = await listMemories({
      page: 2,
      pageSize: 20,
      scope: "personal",
      status: "active",
      category: "note",
      visibility: "private",
      pinned: "true",
      keyword: "安全",
      platform: "qq_official",
      accountRef: "memory_account:v1:account",
      groupRef: "",
      subjectRef: "",
    });
    assert.equal(result.items[0].target.accountRef, "memory_account:v1:account");
    assert.equal(result.items[0].target.scope, "personal");
    assert.equal(result.items[0].version, 1);
    assert.deepEqual(JSON.parse(String(calls[0].init.body)), {
      page: 2,
      page_size: 20,
      scope: "personal",
      status: "active",
      category: "note",
      visibility: "private",
      pinned: true,
      keyword: "安全",
      platform: "qq_official",
      account_ref: "memory_account:v1:account",
    });
  });
});

test("Memory target 列表解析 master 的直接 target summary", async () => {
  await withFetchMock(async () => jsonResponse({
    ok: true,
    data: { items: [targetSummary()], page: 1, page_size: 100, total: 1, total_pages: 1 },
  }), async () => {
    const result = await listMemoryTargets();
    assert.equal(result.items[0].targetRef, TARGET_REF);
    assert.equal(result.items[0].accountRef, "memory_account:v1:account");
  });
});

test("Memory CRUD 始终携带 target_ref 与 CAS version，解析 mutation envelope", async () => {
  await withFetchMock(async (input, init) => {
    const path = String(input);
    if (path.endsWith("/get")) return jsonResponse({ ok: true, data: memoryItem() });
    const body = JSON.parse(String(init.body));
    if (path.endsWith("/create")) return jsonResponse({ ok: true, data: { memory: memoryItem() } });
    if (path.endsWith("/update")) return jsonResponse({ ok: true, data: { memory: memoryItem({ version: body.expected_version + 1, content: "已更新" }) } });
    if (path.endsWith("/archive")) return jsonResponse({ ok: true, data: { memory: memoryItem({ version: body.expected_version + 1, status: "archived", capabilities: { can_update: false, can_archive: false, can_restore: true } }) } });
    return jsonResponse({ ok: true, data: { memory: memoryItem({ version: body.expected_version + 1 }) } });
  }, async (calls) => {
    await getMemory(TARGET_REF, MEMORY_REF);
    await createMemory({ targetRef: TARGET_REF, content: "新内容", category: "note", visibility: "private", pinned: false });
    await updateMemory({ targetRef: TARGET_REF, memoryRef: MEMORY_REF, expectedVersion: 1, patch: { content: "已更新" } });
    await archiveMemory({ targetRef: TARGET_REF, memoryRef: MEMORY_REF, expectedVersion: 2 });
    await restoreMemory({ targetRef: TARGET_REF, memoryRef: MEMORY_REF, expectedVersion: 3 });

    assert.equal(calls[0].input, "/api/v1/console/memories/get");
    assert.deepEqual(JSON.parse(String(calls[0].init.body)), { target_ref: TARGET_REF, memory_ref: MEMORY_REF });
    assert.deepEqual(JSON.parse(String(calls[2].init.body)), {
      target_ref: TARGET_REF,
      memory_ref: MEMORY_REF,
      expected_version: 1,
      patch: { content: "已更新" },
    });
    assert.equal(calls[3].input, "/api/v1/console/memories/archive");
    assert.equal(JSON.parse(String(calls[4].init.body)).expected_version, 3);
  });
});

test("Memory 范围确认使用新的 operation/token 协议并保留 CSRF", async () => {
  setCsrfToken("csrf-memory");
  await withFetchMock(async (_input, init) => {
    const body = JSON.parse(String(init.body));
    if (body.operation === "clear_target" && !body.confirmation_token) {
      return jsonResponse({ ok: true, data: {
        confirmation_token: "memory_confirmation:v1:token",
        operation: body.operation,
        target: targetSummary(),
        affected_count: 2,
        expires_at: 123,
      } });
    }
    return jsonResponse({ ok: true, data: {
      operation: body.operation,
      target: targetSummary(),
      affected_count: 2,
      capabilities: { can_clear_target: true, can_disable_group_profile: false },
    } });
  }, async (calls) => {
    const prepared = await prepareMemoryOperation({ operation: "clear_target", targetRef: TARGET_REF });
    assert.equal(prepared.confirmationToken, "memory_confirmation:v1:token");
    const committed = await commitMemoryOperation({
      operation: prepared.operation,
      targetRef: TARGET_REF,
      confirmationToken: prepared.confirmationToken,
    });
    assert.equal(committed.affectedCount, 2);
    assert.equal(calls[0].input, "/api/v1/console/memories/operations/prepare");
    assert.equal(calls[1].input, "/api/v1/console/memories/operations/commit");
    assert.equal(calls[0].init.headers["X-CSRF-Token"], "csrf-memory");
    assert.equal(calls[1].init.headers["X-CSRF-Token"], "csrf-memory");
  });
});

test("Memory parser 保留 409 冲突，不把失败伪装成成功", async () => {
  await withFetchMock(async () => jsonResponse({ ok: false, error: { code: "conflict", message: "记忆已变化" } }, 409), async () => {
    await assert.rejects(
      commitMemoryOperation({
        operation: "clear_target",
        targetRef: TARGET_REF,
        confirmationToken: "memory_confirmation:v1:stale",
      }),
      (error) => error instanceof ConsoleApiError && error.status === 409 && error.code === "conflict",
    );
  });
});
