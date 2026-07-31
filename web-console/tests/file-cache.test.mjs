import assert from "node:assert/strict";
import test from "node:test";

import {
  CACHE_NAME,
  cacheFileBlob,
  clearFileBlobCache,
  deleteCachedFileBlob,
  fileCacheRequest,
  readCachedFileBlob,
} from "../dist/file-cache.js";

test("fileCacheRequest 构造 Cache API 支持的 GET 匹配键", () => {
  const request = fileCacheRequest("https://console.example/files/a.png");
  assert.equal(request.method, "GET");
  assert.equal(request.url, "https://console.example/files/a.png");
});

test("没有 caches API 时全部降级为网络回退", async () => {
  const previous = globalThis.caches;
  globalThis.caches = undefined;
  try {
    assert.equal(await cacheFileBlob("https://console.example/files/a.png", new Blob(["x"])), false);
    assert.equal(await readCachedFileBlob("https://console.example/files/a.png"), null);
    assert.equal(await deleteCachedFileBlob("https://console.example/files/a.png"), false);
    assert.equal(await clearFileBlobCache(), false);
  } finally {
    globalThis.caches = previous;
  }
});

test("有 caches API 时写入、读取、删除与清空走 GET 请求键", async () => {
  const entries = new Map();
  const putRequests = [];
  const deleteRequests = [];
  const cache = {
    put: async (request, response) => {
      putRequests.push(request);
      entries.set(request.url, response);
    },
    match: async (request) => entries.get(request.url),
    delete: async (request) => {
      deleteRequests.push(request);
      return entries.delete(request.url);
    },
  };
  const storage = {
    open: async (name) => {
      assert.equal(name, CACHE_NAME);
      return cache;
    },
    delete: async (name) => {
      assert.equal(name, CACHE_NAME);
      entries.clear();
      return true;
    },
  };
  const previous = globalThis.caches;
  globalThis.caches = storage;
  try {
    const blob = new Blob(["hello"], { type: "image/png" });
    assert.equal(await cacheFileBlob("https://console.example/files/a.png", blob), true);
    assert.equal(putRequests.length, 1);
    assert.equal(putRequests[0].method, "GET");
    assert.equal(putRequests[0].url, "https://console.example/files/a.png");

    const cached = await readCachedFileBlob("https://console.example/files/a.png");
    assert.ok(cached instanceof Blob);
    assert.equal(await cached.text(), "hello");

    assert.equal(await deleteCachedFileBlob("https://console.example/files/a.png"), true);
    assert.equal(deleteRequests.length, 1);
    assert.equal(deleteRequests[0].method, "GET");
    assert.equal(await readCachedFileBlob("https://console.example/files/a.png"), null);

    // 条目不存在时删除同样视为成功，仅 Cache API 失败才返回 false。
    assert.equal(await deleteCachedFileBlob("https://console.example/files/a.png"), true);

    assert.equal(await clearFileBlobCache(), true);
    assert.equal(entries.size, 0);
  } finally {
    globalThis.caches = previous;
  }
});
