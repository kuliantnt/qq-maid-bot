import test from "node:test";
import assert from "node:assert/strict";

import {
  ConsoleApiError,
  deleteKnowledgeFile,
  downloadKnowledgeFile,
  fetchKnowledgeCapabilities,
  filenameFromContentDisposition,
  listKnowledgeFiles,
  parseKnowledgeFileItem,
  retryKnowledgeFile,
  setCsrfToken,
  setUnauthorizedHandler,
  uploadKnowledgeFile,
} from "../dist/api.js";

function knowledgeItem(overrides = {}) {
  return {
    file_id: "file-1",
    filename: "a.md",
    content_type: "text/markdown",
    size: 12,
    source: "managed",
    source_label: "托管",
    status: "ready",
    uploaded_at: "2026-01-01T00:00:00Z",
    processing_started_at: "2026-01-01T00:00:01Z",
    processed_at: "2026-01-01T00:00:02Z",
    updated_at: "2026-01-01T00:00:02Z",
    error_code: null,
    error_summary: null,
    chunk_count: 1,
    embedding_count: 1,
    downloadable: true,
    download_url: "/download/file-1",
    ...overrides,
  };
}

function envelope(data) {
  return { ok: true, data, request_id: "test-request" };
}

function errorEnvelope(code, message) {
  return { ok: false, error: { code, message }, request_id: "test-request" };
}

function jsonResponse(data, status = 200, headers = {}) {
  return new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json", ...headers } });
}

async function withFetchMock(handler, fn) {
  const previousFetch = globalThis.fetch;
  const calls = [];
  globalThis.fetch = async (input, init = {}) => {
    calls.push({ input: String(input), init });
    return handler(input, init);
  };
  try {
    await fn(calls);
  } finally {
    globalThis.fetch = previousFetch;
  }
}

function listParams(status = "all") {
  return { page: 2, page_size: 20, search: "guide", status, sort: "updated_at", order: "desc" };
}

test("知识库 capabilities 成功解析服务端限制", async () => {
  await withFetchMock(async () => jsonResponse(envelope({
    supported_extensions: [".md", ".markdown"], max_file_bytes: 1024, max_filename_chars: 64,
  })), async () => {
    const result = await fetchKnowledgeCapabilities();
    assert.deepEqual(result, { supported_extensions: [".md", ".markdown"], max_file_bytes: 1024, max_filename_chars: 64 });
  });
});

test("知识库列表解析分页和缺失的可空字段", async () => {
  const { file_id, size, uploaded_at, processing_started_at, processed_at, error_code, error_summary, chunk_count, embedding_count, download_url, ...itemWithoutNullableFields } = knowledgeItem();
  await withFetchMock(async () => jsonResponse(envelope({
    items: [itemWithoutNullableFields],
    page: 2, page_size: 20, total: 1, total_pages: 0,
  })), async () => {
    const result = await listKnowledgeFiles(listParams());
    assert.equal(result.page, 2);
    assert.equal(result.page_size, 20);
    assert.equal(result.total, 1);
    assert.equal(result.total_pages, 0);
    assert.deepEqual(result.items[0], knowledgeItem({ file_id: null, size: null, uploaded_at: null, processing_started_at: null, processed_at: null, error_code: null, error_summary: null, chunk_count: null, embedding_count: null, download_url: null }));
  });
});

test("知识库列表仅在具体状态时发送 status", async () => {
  await withFetchMock(async () => jsonResponse(envelope({ items: [], page: 1, page_size: 20, total: 0, total_pages: 0 })), async (calls) => {
    await listKnowledgeFiles({ ...listParams(), page: 1 });
    await listKnowledgeFiles({ ...listParams("failed"), page: 1 });
    assert.deepEqual(JSON.parse(String(calls[0].init.body)), { page: 1, page_size: 20, search: "guide", sort: "updated_at", order: "desc" });
    assert.deepEqual(JSON.parse(String(calls[1].init.body)), { page: 1, page_size: 20, search: "guide", status: "failed", sort: "updated_at", order: "desc" });
  });
});

test("上传知识库文件使用受 CSRF 保护的 multipart 请求并返回 pending 项", async () => {
  setCsrfToken("csrf-test");
  await withFetchMock(async () => jsonResponse(envelope(knowledgeItem({ status: "pending" }))), async (calls) => {
    const result = await uploadKnowledgeFile(new File(["# guide"], "guide.md", { type: "text/markdown" }));
    assert.equal(result.status, "pending");
    assert.equal(calls[0].init.method, "POST");
    assert.equal(calls[0].init.credentials, "same-origin");
    assert.equal(calls[0].init.headers["X-CSRF-Token"], "csrf-test");
    assert.equal(calls[0].init.body.get("file").name, "guide.md");
  });
});

test("上传知识库文件保留 413 服务端错误状态和代码", async () => {
  await withFetchMock(async () => jsonResponse(errorEnvelope("knowledge_file_too_large", "文件过大"), 413), async () => {
    await assert.rejects(uploadKnowledgeFile(new File(["x"], "large.md")), (error) => (
      error instanceof ConsoleApiError && error.status === 413 && error.code === "knowledge_file_too_large"
    ));
  });
});

test("下载知识库文件使用受保护 POST 并优先使用 UTF-8 文件名", async () => {
  setCsrfToken("csrf-test");
  await withFetchMock(async () => new Response("document", {
    status: 200,
    headers: { "Content-Disposition": "attachment; filename=\"a.md\"; filename*=UTF-8''%E6%B5%8B%E8%AF%95.md" },
  }), async (calls) => {
    const result = await downloadKnowledgeFile({ file_id: "file-1", filename: "fallback.md" });
    assert.equal(result.blob instanceof Blob, true);
    assert.equal(result.filename, "测试.md");
    assert.equal(calls[0].init.method, "POST");
    assert.equal(calls[0].init.credentials, "same-origin");
    assert.equal(calls[0].init.headers["X-CSRF-Token"], "csrf-test");
  });
});

test("下载缺少 Content-Disposition 时回退 API 项文件名", async () => {
  await withFetchMock(async () => new Response("document", { status: 200 }), async () => {
    const result = await downloadKnowledgeFile({ file_id: "file-1", filename: "fallback.md" });
    assert.equal(result.filename, "fallback.md");
  });
});

test("删除知识库文件支持成功和 409 冲突错误", async () => {
  await withFetchMock(async () => jsonResponse(envelope({})), async () => {
    await deleteKnowledgeFile("file-1");
  });
  await withFetchMock(async () => jsonResponse(errorEnvelope("conflict", "文件正在处理中"), 409), async () => {
    await assert.rejects(deleteKnowledgeFile("file-1"), (error) => (
      error instanceof ConsoleApiError && error.status === 409 && error.code === "conflict"
    ));
  });
});

test("重新处理知识库文件返回 pending 并保留 404 错误", async () => {
  await withFetchMock(async () => jsonResponse(envelope(knowledgeItem({ status: "pending" }))), async () => {
    assert.equal((await retryKnowledgeFile("file-1")).status, "pending");
  });
  await withFetchMock(async () => jsonResponse(errorEnvelope("not_found", "文件不存在"), 404), async () => {
    await assert.rejects(retryKnowledgeFile("file-1"), (error) => (
      error instanceof ConsoleApiError && error.status === 404 && error.code === "not_found"
    ));
  });
});

test("认证和权限错误保留安全信息而不泄漏原始响应", async () => {
  for (const [status, code, message] of [[401, "unauthorized", "请先登录"], [403, "forbidden", "权限不足"]]) {
    await withFetchMock(async () => jsonResponse(errorEnvelope(code, message), status), async () => {
      await assert.rejects(fetchKnowledgeCapabilities(), (error) => (
        error instanceof ConsoleApiError
        && error.status === status
        && error.code === code
        && error.message === message
        && !error.message.includes("request_id")
      ));
    });
  }
});

test("401 会通知统一会话失效处理器", async () => {
  let notifications = 0;
  setUnauthorizedHandler(() => { notifications += 1; });
  try {
    await withFetchMock(async () => jsonResponse(errorEnvelope("unauthorized", "请先登录"), 401), async () => {
      await assert.rejects(fetchKnowledgeCapabilities(), (error) => error instanceof ConsoleApiError && error.status === 401);
    });
    assert.equal(notifications, 1);
  } finally {
    setUnauthorizedHandler(null);
  }
});

test("知识库 parser 拒绝缺失必填字段和未知状态", () => {
  assert.throws(() => parseKnowledgeFileItem(knowledgeItem({ filename: "" })), (error) => error instanceof ConsoleApiError && error.code === "invalid_response");
  assert.throws(() => parseKnowledgeFileItem(knowledgeItem({ status: "bogus" })), (error) => error instanceof ConsoleApiError && error.code === "invalid_response");
});

test("Content-Disposition 文件名解析覆盖空值、ASCII 和 UTF-8", () => {
  assert.equal(filenameFromContentDisposition(null), null);
  assert.equal(filenameFromContentDisposition("attachment; filename=\"a.md\""), "a.md");
  assert.equal(filenameFromContentDisposition("attachment; filename*=UTF-8''%E6%B5%8B%E8%AF%95.md"), "测试.md");
});
