import test from "node:test";
import assert from "node:assert/strict";

import { KnowledgePollingController } from "../dist/views/knowledge/knowledge-polling.js";

function item(status, fileId = "file-1") {
  return { file_id: fileId, status };
}

function page(items) {
  return { items, page: 1, page_size: 20, total: items.length, total_pages: 1 };
}

function pollingFixture(overrides = {}) {
  const timers = new Map();
  let nextId = 1;
  const updates = [];
  const errors = [];
  const transitions = [];
  const deps = {
    isVisible: () => true,
    setTimeout: (fn) => { const id = nextId++; timers.set(id, fn); return id; },
    clearTimeout: (id) => timers.delete(id),
    fetchPage: async () => page([item("pending")]),
    onUpdate: (value) => updates.push(value),
    onTransientError: (message) => errors.push(message),
    onTerminalTransition: (message) => transitions.push(message),
    ...overrides,
  };
  const controller = new KnowledgePollingController(deps);
  return { controller, timers, updates, errors, transitions };
}

async function runTimer(timers) {
  const entry = timers.entries().next().value;
  assert.ok(entry, "expected a scheduled poll");
  timers.delete(entry[0]);
  entry[1]();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

test("polling fetches active pages and ignores stale responses", async () => {
  let resolveFirst;
  let resolveSecond;
  let calls = 0;
  const fixture = pollingFixture({
    fetchPage: () => new Promise((resolve) => {
      calls += 1;
      if (calls === 1) resolveFirst = resolve;
      else resolveSecond = resolve;
    }),
  });
  fixture.controller.setPages([item("pending")]);
  fixture.controller.start({ page: 1, page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" });
  await runTimer(fixture.timers);
  fixture.controller.notifyChange();
  await runTimer(fixture.timers);
  resolveSecond(page([item("ready")]));
  await new Promise((resolve) => setTimeout(resolve, 0));
  resolveFirst(page([item("failed")]));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(fixture.updates.length, 1);
  assert.equal(fixture.updates[0].items[0].status, "ready");
});

test("polling skips hidden pages and stops after terminal result", async () => {
  let fetches = 0;
  const fixture = pollingFixture({ isVisible: () => false, fetchPage: async () => { fetches += 1; return page([item("ready")]); } });
  fixture.controller.setPages([item("pending")]);
  fixture.controller.start({ page: 1, page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" });
  await runTimer(fixture.timers);
  assert.equal(fetches, 0);
  assert.equal(fixture.timers.size, 1);
  fixture.controller.stop();
  fixture.controller.setPages([item("pending")]);
  const terminal = pollingFixture({ fetchPage: async () => page([item("ready")]) });
  terminal.controller.setPages([item("pending")]);
  terminal.controller.start({ page: 1, page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" });
  await runTimer(terminal.timers);
  assert.equal(terminal.timers.size, 0);
});

test("polling stops after three failures and reports terminal transitions", async () => {
  const fixture = pollingFixture({ fetchPage: async () => { throw new Error("offline"); } });
  fixture.controller.setPages([item("pending")]);
  fixture.controller.start({ page: 1, page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" });
  await runTimer(fixture.timers);
  await runTimer(fixture.timers);
  await runTimer(fixture.timers);
  assert.deepEqual(fixture.errors, ["状态刷新失败", "状态刷新失败", "状态刷新失败", "状态刷新多次失败，请手动刷新"]);
  assert.equal(fixture.timers.size, 0);

  const transitions = pollingFixture({ fetchPage: async () => page([item("ready"), item("failed", "file-2")]) });
  transitions.controller.setPages([item("pending"), item("processing", "file-2")]);
  transitions.controller.start({ page: 1, page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" });
  await runTimer(transitions.timers);
  assert.deepEqual(transitions.transitions, ["文件处理完成", "文件处理失败"]);
});

test("notifyChange clears the prior poll timer", () => {
  const fixture = pollingFixture();
  fixture.controller.setPages([item("pending")]);
  fixture.controller.start({ page: 1, page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" });
  const first = [...fixture.timers.keys()][0];
  fixture.controller.notifyChange();
  assert.equal(fixture.timers.has(first), false);
  assert.equal(fixture.timers.size, 1);
});

test("updateParams replaces the next polling request and resets its timer", async () => {
  const requests = [];
  const fixture = pollingFixture({
    fetchPage: async (params) => {
      requests.push(params);
      return page([item("pending")]);
    },
  });
  const initialParams = { page: 1, page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" };
  const filteredParams = { ...initialParams, search: "失败文件", status: "failed" };
  fixture.controller.setPages([item("pending")]);
  fixture.controller.start(initialParams);
  const initialTimer = [...fixture.timers.keys()][0];

  fixture.controller.updateParams(filteredParams);

  assert.equal(fixture.timers.has(initialTimer), false);
  assert.equal(fixture.timers.size, 1);
  await runTimer(fixture.timers);
  assert.deepEqual(requests, [filteredParams]);
});
