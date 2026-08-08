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
import { disposeKnowledge, initializeKnowledge, refreshKnowledgeList } from "../dist/views/knowledge/knowledge.js";
import { ConsoleApiError } from "../dist/api.js";
import { formatFileSizeLimit, installKnowledgeUpload, validateKnowledgeFile } from "../dist/views/knowledge/knowledge-upload.js";
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
  const readyStatus = target.children[0].children[1].children[0].children[5];
  assert.equal(readyStatus.className, "");
  assert.equal(readyStatus.children[0].className, "knowledge-status knowledge-status--ready");
  buttons[0].onclick();
  buttons[1].onclick();
  buttons[2].onclick();
  assert.deepEqual(calls, [["download", ready], ["delete", ready], ["retry", failed]]);
});

function setupKnowledgePage() {
  disposeKnowledge();
  const fake = createFakeDom();
  installDomGlobals(fake);
  document.body = document.createElement("div");
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

function knowledgeCapabilities() {
  return { supported_extensions: [".md", ".markdown"], max_file_bytes: 1024, max_filename_chars: 16 };
}

function file(name, size) {
  return new File(["x".repeat(size)], name, { type: "text/markdown" });
}

function setupUpload(upload, getCapabilities = knowledgeCapabilities) {
  const fake = createFakeDom();
  installDomGlobals(fake);
  document.body = document.createElement("div");
  const button = document.registerStaticId("knowledge-upload-open", "button");
  const statuses = [];
  let uploaded = 0;
  installKnowledgeUpload({
    inputId: "knowledge-upload-input",
    buttonId: "knowledge-upload-open",
    setStatus: (text) => statuses.push(text),
    getCapabilities,
    upload,
    onUploaded: () => { uploaded += 1; },
  });
  const input = document.getElementById("knowledge-upload-input");
  return { button, input, statuses, uploaded: () => uploaded };
}

function triggerInputChange(input, selectedFile) {
  input.files = [selectedFile];
  for (const listener of input.listeners.get("change")) listener();
}

test("知识库上传预检覆盖格式、大小、文件名和大小写边界", () => {
  const capabilities = knowledgeCapabilities();
  const validFile = file("guide.md", 100);
  const validResult = validateKnowledgeFile(validFile, capabilities);
  assert.equal(validResult.ok, true);
  if (validResult.ok) assert.equal(validResult.file, validFile);
  assert.equal(validateKnowledgeFile(file("guide.txt", 100), capabilities).reason, "仅支持 .md / .markdown 文件");
  assert.equal(validateKnowledgeFile(file("guide.md", 1025), capabilities).reason, "文件大小超过上限（1 KB）");
  assert.equal(validateKnowledgeFile(file("a".repeat(14) + ".md", 100), capabilities).reason, "文件名过长");
  assert.equal(validateKnowledgeFile(file("GUIDE.MARKDOWN", 100), capabilities).ok, true);
});

test("知识库上传大小上限格式化保持可读", () => {
  assert.equal(formatFileSizeLimit(50 * 1024 * 1024), "50 MB");
  assert.equal(formatFileSizeLimit(1536 * 1024), "1.5 MB");
  assert.equal(formatFileSizeLimit(1024), "1 KB");
});

test("知识库上传禁用按钮至成功并按能力生成 accept", async () => {
  let resolveUpload;
  const flow = setupUpload(() => new Promise((resolve) => { resolveUpload = resolve; }));
  let clicked = false;
  flow.input.click = () => { clicked = true; };
  flow.button.onclick();
  assert.equal(clicked, true);
  assert.equal(flow.input.accept, ".md,.markdown");
  triggerInputChange(flow.input, file("guide.md", 100));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(flow.button.disabled, true);
  assert.equal(flow.statuses.at(-1), "上传中…");
  resolveUpload(knowledgeItem({ status: "pending" }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(flow.statuses.at(-1), "文件已上传，正在等待处理");
  assert.equal(flow.uploaded(), 1);
  assert.equal(flow.button.disabled, false);
});

test("知识库上传失败显示安全代码并恢复按钮", async () => {
  const flow = setupUpload(async () => { throw new ConsoleApiError("文件过大", "knowledge_file_too_large", 413); });
  triggerInputChange(flow.input, file("guide.md", 100));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.match(flow.statuses.at(-1), /knowledge_file_too_large/);
  assert.equal(flow.uploaded(), 0);
  assert.equal(flow.button.disabled, false);
});

test("知识库上传格式预检会阻止请求", async () => {
  const files = [];
  const flow = setupUpload(async (selectedFile) => { files.push(selectedFile); return knowledgeItem({ status: "pending" }); });
  for (const [invalidFile, reason] of [
    [file("guide.txt", 100), "仅支持 .md / .markdown 文件"],
    [file("guide.md", 1025), "文件大小超过上限（1 KB）"],
    [file(`${"a".repeat(14)}.md`, 100), "文件名过长"],
  ]) {
    triggerInputChange(flow.input, invalidFile);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(files.length, 0, `${reason} 不应发送上传请求`);
    assert.equal(flow.statuses.at(-1), `上传已阻止：${reason}`);
  }

  const serverAuthoritative = setupUpload(async (selectedFile) => { files.push(selectedFile); return knowledgeItem({ status: "pending" }); }, () => null);
  triggerInputChange(serverAuthoritative.input, file("guide.txt", 100));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(files.length, 1, "能力尚未加载时应交由服务端校验");
  assert.equal(serverAuthoritative.statuses.at(-1), "文件已上传，正在等待处理");
});

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

test("知识库状态筛选同步到轮询请求且不会覆盖筛选结果", async () => {
  setupKnowledgePage();
  const timers = new Map();
  const requests = [];
  let nextTimerId = 1;
  window.setTimeout = (callback) => {
    const timerId = nextTimerId++;
    timers.set(timerId, callback);
    return timerId;
  };
  window.clearTimeout = (timerId) => timers.delete(timerId);
  globalThis.fetch = async (_input, init) => {
    const body = JSON.parse(String(init.body));
    requests.push(body);
    if (requests.length === 1) return new Response(JSON.stringify({ ok: true, data: knowledgeCapabilities() }));
    if (body.status === "processing") return new Response(JSON.stringify(knowledgeResponse([knowledgeItem({ filename: "processing.md", status: "processing" })])));
    return new Response(JSON.stringify(knowledgeResponse([knowledgeItem({ filename: "pending.md", status: "pending" })])));
  };

  await initializeKnowledge();
  const status = document.getElementById("knowledge-status-filter");
  status.value = "processing";
  document.getElementById("knowledge-filter-submit").onclick();
  await new Promise((resolve) => setTimeout(resolve, 0));

  const scheduled = timers.entries().next().value;
  assert.ok(scheduled, "筛选后的待处理列表应启动轮询");
  timers.delete(scheduled[0]);
  scheduled[1]();
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.equal(requests[2].status, "processing");
  assert.equal(requests[3].status, "processing");
  assert.equal(document.getElementById("knowledge-list").children[0].children[1].children[0].children[0].textContent, "processing.md");
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

test("轮询响应不会淘汰同时进行的主动刷新", async () => {
  setupKnowledgePage();
  const timers = new Map();
  let nextTimerId = 1;
  window.setTimeout = (callback) => {
    const timerId = nextTimerId++;
    timers.set(timerId, callback);
    return timerId;
  };
  window.clearTimeout = (timerId) => timers.delete(timerId);
  let listCalls = 0;
  let resolvePoll;
  let resolveRefresh;
  globalThis.fetch = async (_input, init) => {
    const body = JSON.parse(String(init.body));
    if (body.page === undefined) return new Response(JSON.stringify({ ok: true, data: knowledgeCapabilities() }));
    listCalls += 1;
    if (listCalls === 1) return new Response(JSON.stringify(knowledgeResponse([knowledgeItem({ status: "pending", filename: "initial.md" })])));
    if (listCalls === 2) return new Promise((resolve) => { resolvePoll = () => resolve(new Response(JSON.stringify(knowledgeResponse([knowledgeItem({ status: "processing", filename: "stale-poll.md" })])))); });
    return new Promise((resolve) => { resolveRefresh = () => resolve(new Response(JSON.stringify(knowledgeResponse([knowledgeItem({ status: "ready", filename: "fresh.md" })])))); });
  };

  await initializeKnowledge();
  const scheduled = timers.entries().next().value;
  assert.ok(scheduled, "初始待处理文件应安排轮询");
  timers.delete(scheduled[0]);
  scheduled[1]();
  await new Promise((resolve) => setTimeout(resolve, 0));

  const refresh = refreshKnowledgeList("refresh");
  await new Promise((resolve) => setTimeout(resolve, 0));
  resolveRefresh();
  await refresh;
  resolvePoll();
  await new Promise((resolve) => setTimeout(resolve, 0));

  const refreshedRows = document.getElementById("knowledge-list").children[0].children[1].children;
  assert.equal(refreshedRows.some((row) => row.children[0].textContent === "fresh.md"), true);
  assert.equal(refreshedRows.some((row) => row.children[0].textContent === "stale-poll.md"), false);
  delete globalThis.fetch;
});

test("刷新淘汰加载更多后会恢复分页按钮且不追加旧页", async () => {
  setupKnowledgePage();
  let resolvePageTwo;
  let resolveRefresh;
  let listCalls = 0;
  globalThis.fetch = async (_input, init) => {
    const body = JSON.parse(String(init.body));
    if (body.page === undefined) return new Response(JSON.stringify({ ok: true, data: knowledgeCapabilities() }));
    listCalls += 1;
    if (listCalls === 1) return new Response(JSON.stringify({ ok: true, data: { items: [knowledgeItem({ filename: "page-1.md" })], page: 1, page_size: 20, total: 2, total_pages: 2 } }));
    if (listCalls === 2) return new Promise((resolve) => { resolvePageTwo = () => resolve(new Response(JSON.stringify({ ok: true, data: { items: [knowledgeItem({ filename: "stale-page-2.md" })], page: 2, page_size: 20, total: 2, total_pages: 2 } }))); });
    return new Promise((resolve) => { resolveRefresh = () => resolve(new Response(JSON.stringify({ ok: true, data: { items: [knowledgeItem({ filename: "refreshed-page-1.md" })], page: 1, page_size: 20, total: 2, total_pages: 2 } }))); });
  };

  await initializeKnowledge();
  document.getElementById("knowledge-pagination").children[0].onclick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  const refresh = refreshKnowledgeList("refresh");
  await new Promise((resolve) => setTimeout(resolve, 0));
  resolveRefresh();
  await refresh;
  resolvePageTwo();
  await new Promise((resolve) => setTimeout(resolve, 0));

  const pagination = document.getElementById("knowledge-pagination");
  assert.equal(pagination.children.length, 1);
  assert.equal(pagination.children[0].disabled, false, "被淘汰的加载更多请求不能永久锁住按钮");
  const refreshedRows = document.getElementById("knowledge-list").children[0].children[1].children;
  assert.equal(refreshedRows.some((row) => row.children[0].textContent === "stale-page-2.md"), false);
  delete globalThis.fetch;
});

test("删除后的刷新重建第一页并清除已经不存在的后续页", async () => {
  setupKnowledgePage();
  let listCalls = 0;
  globalThis.fetch = async (_input, init) => {
    const body = JSON.parse(String(init.body));
    if (body.page === undefined) return new Response(JSON.stringify({ ok: true, data: knowledgeCapabilities() }));
    listCalls += 1;
    if (listCalls === 1) return new Response(JSON.stringify({ ok: true, data: { items: [knowledgeItem({ filename: "page-1.md" })], page: 1, page_size: 20, total: 2, total_pages: 2 } }));
    if (body.page === 2) return new Response(JSON.stringify({ ok: true, data: { items: [knowledgeItem({ filename: "page-2.md" })], page: 2, page_size: 20, total: 2, total_pages: 2 } }));
    return new Response(JSON.stringify({ ok: true, data: { items: [knowledgeItem({ filename: "remaining.md" })], page: 1, page_size: 20, total: 1, total_pages: 1 } }));
  };

  await initializeKnowledge();
  document.getElementById("knowledge-pagination").children[0].onclick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await refreshKnowledgeList("delete");

  const rows = document.getElementById("knowledge-list").children[0].children[1].children;
  assert.equal(rows.some((row) => row.children[0].textContent === "remaining.md"), true);
  assert.equal(rows.some((row) => row.children[0].textContent === "page-2.md"), false);
  assert.equal(document.getElementById("knowledge-pagination").children.length, 0);
  delete globalThis.fetch;
});

test("加载两页后轮询不会丢失后续页", async () => {
  setupKnowledgePage();
  const timers = new Map();
  let nextTimerId = 1;
  window.setTimeout = (callback) => {
    const timerId = nextTimerId++;
    timers.set(timerId, callback);
    return timerId;
  };
  window.clearTimeout = (timerId) => timers.delete(timerId);
  const requests = [];
  globalThis.fetch = async (_input, init) => {
    const body = JSON.parse(String(init.body));
    requests.push(body);
    if (requests.length === 1) return new Response(JSON.stringify({ ok: true, data: knowledgeCapabilities() }));
    const pageNumber = body.page;
    const itemName = pageNumber === 1 ? "page-1.md" : "page-2.md";
    return new Response(JSON.stringify({ ok: true, data: { items: [knowledgeItem({ filename: itemName, file_id: `file-${pageNumber}`, status: pageNumber === 2 ? "processing" : "ready" })], page: pageNumber, page_size: 20, total: 2, total_pages: 2 } }));
  };
  await initializeKnowledge();
  document.getElementById("knowledge-pagination").children[0].onclick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(requests.some((request) => request.page === 2), true);
  let body = document.getElementById("knowledge-list").children[0].children[1];
  assert.equal(body.children.some((row) => row.children[0].textContent === "page-2.md"), true);
  const scheduled = timers.entries().next().value;
  assert.ok(scheduled, "第二页仍在处理中时应安排轮询");
  timers.delete(scheduled[0]);
  scheduled[1]();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(requests.slice(-2).map((request) => request.page), [1, 2]);
  body = document.getElementById("knowledge-list").children[0].children[1];
  assert.equal(body.children.some((row) => row.children[0].textContent === "page-2.md"), true, "轮询完成后必须保留已加载的第二页");
  delete globalThis.fetch;
});

test("连续点击加载更多只请求一次并且不会重复追加", async () => {
  setupKnowledgePage();
  const requests = [];
  let resolvePageTwo;
  globalThis.fetch = async (_input, init) => {
    const body = JSON.parse(String(init.body));
    requests.push(body);
    if (requests.length === 1) return new Response(JSON.stringify({ ok: true, data: knowledgeCapabilities() }));
    if (body.page === 2) return new Promise((resolve) => { resolvePageTwo = () => resolve(new Response(JSON.stringify({ ok: true, data: { items: [knowledgeItem({ file_id: "file-2", filename: "page-2.md" })], page: 2, page_size: 20, total: 2, total_pages: 2 } }))); });
    return new Response(JSON.stringify({ ok: true, data: { items: [knowledgeItem({ filename: "page-1.md" })], page: 1, page_size: 20, total: 2, total_pages: 2 } }));
  };
  await initializeKnowledge();
  const loadMore = document.getElementById("knowledge-pagination").children[0];
  loadMore.onclick();
  loadMore.onclick();
  assert.equal(document.getElementById("knowledge-pagination").children[0].disabled, true);
  assert.equal(requests.filter((request) => request.page === 2).length, 1);
  resolvePageTwo();
  await new Promise((resolve) => setTimeout(resolve, 0));
  const rows = document.getElementById("knowledge-list").children[0].children[1].children;
  assert.deepEqual(rows.map((row) => row.children[0].textContent), ["page-1.md", "page-2.md"]);
  assert.equal(document.getElementById("knowledge-pagination").children.length, 0, "最后一页完成后加载更多按钮应消失");
  delete globalThis.fetch;
});

test("登出停止轮询，重新登录不重复绑定控件并重建上传输入", async () => {
  setupKnowledgePage();
  const timers = new Map();
  let nextTimerId = 1;
  window.setTimeout = (callback) => {
    const timerId = nextTimerId++;
    timers.set(timerId, callback);
    return timerId;
  };
  window.clearTimeout = (timerId) => timers.delete(timerId);
  const requests = [];
  globalThis.fetch = async (_input, init) => {
    const body = JSON.parse(String(init.body));
    requests.push(body);
    if (body.page === undefined) return new Response(JSON.stringify({ ok: true, data: knowledgeCapabilities() }));
    return new Response(JSON.stringify(knowledgeResponse([knowledgeItem({ status: "pending" })])));
  };
  await initializeKnowledge();
  assert.equal(timers.size, 1, "待处理文件应启动轮询");
  const firstInput = document.getElementById("knowledge-upload-input");
  let removed = false;
  firstInput.remove = () => { removed = true; document.registry.delete("knowledge-upload-input"); };
  disposeKnowledge();
  assert.equal(timers.size, 0, "登出必须清除已安排的轮询");
  assert.equal(removed, true, "登出必须移除上传输入");
  assert.equal(document.getElementById("knowledge-upload-input"), null);
  await initializeKnowledge();
  assert.equal(requests.length, 4, "重新登录应重新加载 capabilities 和第一页列表");
  assert.notEqual(document.getElementById("knowledge-upload-input"), firstInput, "重新登录必须创建新的上传输入");
  const beforeFilter = requests.length;
  document.getElementById("knowledge-search").value = "relogin";
  for (const listener of document.getElementById("knowledge-search").listeners.get("keydown")) listener({ key: "Enter", preventDefault: () => undefined });
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(requests.length - beforeFilter, 1, "重新登录后的 Enter 筛选只能发送一次列表请求");
  disposeKnowledge();
  delete globalThis.fetch;
});

test("首屏列表失败退出加载态并可重试恢复列表", async () => {
  setupKnowledgePage();
  let attempts = 0;
  globalThis.fetch = async () => {
    attempts += 1;
    if (attempts === 1) throw new Error("列表不可用");
    return new Response(JSON.stringify(knowledgeResponse([knowledgeItem({ filename: "retry.md" })])));
  };
  await refreshKnowledgeList("refresh");
  assert.equal(document.getElementById("knowledge-list").textContent.includes("正在加载知识库…"), false);
  assert.equal(document.getElementById("knowledge-list").children[0].textContent, "知识库列表加载失败");
  assert.match(document.getElementById("knowledge-result").textContent, /列表不可用/);
  const retry = document.getElementById("knowledge-list").children[1];
  assert.equal(retry.textContent, "重试");
  retry.onclick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  const rows = document.getElementById("knowledge-list").children[0].children[1].children;
  assert.equal(rows.some((row) => row.children[0].textContent === "retry.md"), true);
  assert.equal(document.getElementById("knowledge-list").textContent.includes("正在加载知识库…"), false);
  delete globalThis.fetch;
});
