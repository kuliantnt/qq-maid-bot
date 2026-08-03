import test from "node:test";
import assert from "node:assert/strict";

import { formatBytes, formatDateTime, knowledgeStatusMeta } from "../dist/views/knowledge/knowledge-status.js";
import {
  appendKnowledgePage,
  hasMoreKnowledgePages,
  initialKnowledgePager,
  pageAfterKnowledgeDelete,
} from "../dist/views/knowledge/knowledge-paging.js";
import { renderKnowledgeList } from "../dist/views/knowledge/knowledge-list.js";
import { initializeKnowledge, refreshKnowledgeList } from "../dist/views/knowledge/knowledge.js";
import { createFakeDom, installDomGlobals } from "./helpers/fake-dom.mjs";

function knowledgeItem(overrides = {}) {
  return {
    file_id: "file-1",
    filename: "guide.md",
    content_type: ".md",
    size: 2048,
    source: "managed",
    source_label: "托管文件",
    status: "ready",
    uploaded_at: "2026-08-04T09:30:00",
    processing_started_at: "2026-08-04T09:31:00",
    processed_at: "2026-08-04T09:32:00",
    updated_at: "2026-08-04T09:32:00",
    error_code: null,
    error_summary: null,
    chunk_count: 1,
    embedding_count: 1,
    downloadable: true,
    download_url: null,
    ...overrides,
  };
}

test("知识库状态显示四种稳定文案和类名", () => {
  assert.deepEqual(knowledgeStatusMeta("pending"), { label: "等待处理", className: "knowledge-status--pending" });
  assert.deepEqual(knowledgeStatusMeta("processing"), { label: "处理中", className: "knowledge-status--processing" });
  assert.deepEqual(knowledgeStatusMeta("ready"), { label: "已完成", className: "knowledge-status--ready" });
  assert.deepEqual(knowledgeStatusMeta("failed"), { label: "处理失败", className: "knowledge-status--failed" });
});

test("知识库大小和时间格式化保持确定性", () => {
  assert.equal(formatBytes(null), "—");
  assert.equal(formatBytes(0), "—");
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(2048), "2 KB");
  assert.equal(formatBytes(5 * 1024 * 1024), "5 MB");
  assert.equal(formatDateTime(null), "—");
  assert.equal(formatDateTime("2026-08-04T09:30:00"), "2026-08-04 09:30");
});

test("知识库分页累积、继续加载与删除计数", () => {
  let pager = initialKnowledgePager();
  assert.deepEqual(pager, { page: 1, totalPages: 0, loadedCount: 0, hasMore: false });
  pager = appendKnowledgePage(pager, { items: [knowledgeItem()], page: 1, page_size: 20, total: 2, total_pages: 2 });
  assert.deepEqual(pager, { page: 1, totalPages: 2, loadedCount: 1, hasMore: true });
  assert.equal(hasMoreKnowledgePages(pager), true);
  pager = pageAfterKnowledgeDelete(pager, 5);
  assert.equal(pager.loadedCount, 0);
  assert.equal(hasMoreKnowledgePages({ ...pager, hasMore: false }), false);
});

test("知识库列表按来源和状态展示安全操作并触发回调", () => {
  installDomGlobals(createFakeDom());
  const target = document.createElement("div");
  const calls = [];
  const ready = knowledgeItem();
  const failed = knowledgeItem({ file_id: "file-2", status: "failed", downloadable: false });
  const processing = knowledgeItem({ file_id: "file-3", status: "processing" });
  const directory = knowledgeItem({ file_id: null, source: "directory", status: "ready", error_summary: undefined });
  renderKnowledgeList(target, [ready, failed, processing, directory], {
    onDownload: (item) => calls.push(["download", item]),
    onDelete: (item) => calls.push(["delete", item]),
    onRetry: (item) => calls.push(["retry", item]),
  });
  const buttons = target.querySelectorAll("button");
  assert.deepEqual(buttons.map((button) => button.textContent), ["下载", "删除", "重新处理", "删除", "下载"]);
  assert.equal(target.querySelectorAll("button[data-file-id=\"file-3\"]").some((button) => button.textContent === "删除"), false);
  assert.equal(target.querySelectorAll("button[data-file-id]").length, 5);
  const directoryActions = target.children[0].children[1].children[3].children[7];
  assert.equal(directoryActions.textContent, "—");
  buttons[0].onclick();
  buttons[1].onclick();
  buttons[2].onclick();
  assert.deepEqual(calls, [["download", ready], ["delete", ready], ["retry", failed]]);
});

function setupKnowledgePage() {
  const fake = createFakeDom();
  installDomGlobals(fake);
  document.registerStaticId("knowledge-search", "input");
  document.registerStaticId("knowledge-status-filter", "select");
  document.registerStaticId("knowledge-filter-submit", "button");
  document.registerStaticId("knowledge-filter-reset", "button");
  document.registerStaticId("knowledge-refresh", "button");
  document.registerStaticId("knowledge-upload-open", "button");
  document.registerStaticId("knowledge-result", "p");
  document.registerStaticId("knowledge-list", "div");
  document.registerStaticId("knowledge-pagination", "div");
}

function knowledgeResponse(items = []) {
  return { ok: true, data: { items, page: 1, page_size: 20, total: items.length, total_pages: 1 }, request_id: "test" };
}

test("知识库刷新展示加载和空态，筛选 all 不发送 status", async () => {
  setupKnowledgePage();
  const calls = [];
  let resolveList;
  globalThis.fetch = async (_input, init) => {
    const body = JSON.parse(String(init.body));
    calls.push(body);
    if (calls.length === 1) return new Response(JSON.stringify({ ok: true, data: { supported_extensions: [".md"], max_file_bytes: 1, max_filename_chars: 1 } }));
    return new Promise((resolve) => { resolveList = () => resolve(new Response(JSON.stringify(knowledgeResponse()))); });
  };
  const initialized = initializeKnowledge();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(document.getElementById("knowledge-list").children[0].textContent, "正在加载知识库…");
  resolveList();
  await initialized;
  assert.equal(document.getElementById("knowledge-list").children[0].textContent, "暂无知识库文件");
  assert.equal("status" in calls[1], false);
  const search = document.getElementById("knowledge-search");
  const status = document.getElementById("knowledge-status-filter");
  search.value = "old";
  status.value = "failed";
  document.getElementById("knowledge-filter-reset").onclick();
  resolveList();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(search.value, "");
  assert.equal(status.value, "all");
  delete globalThis.fetch;
});

test("知识库刷新失败保留现有列表并显示安全错误", async () => {
  setupKnowledgePage();
  globalThis.fetch = async () => new Response(JSON.stringify(knowledgeResponse([knowledgeItem()])), { status: 200 });
  await refreshKnowledgeList("refresh");
  const existing = document.getElementById("knowledge-list").children[0];
  globalThis.fetch = async () => new Response(JSON.stringify({ ok: false, error: { message: "会话已过期" } }), { status: 401 });
  await refreshKnowledgeList("upload");
  assert.equal(document.getElementById("knowledge-list").children[0], existing);
  assert.match(document.getElementById("knowledge-result").textContent, /会话已过期|HTTP 401/);
  delete globalThis.fetch;
});
