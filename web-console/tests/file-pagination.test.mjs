import assert from "node:assert/strict";
import test from "node:test";

import { collectAllUserFiles } from "../dist/api.js";
import { createBackgroundController } from "../dist/background.js";

function fileEntry(fileId) {
  return {
    fileId,
    filename: `${fileId}.png`,
    contentType: "image/png",
    size: 1,
    createdAt: "",
    url: `/api/v1/console/files/get/${fileId}`,
  };
}

test("超过 100 个用户文件时按分页元数据完整收集，第二页活动背景可解析", async () => {
  const total = 150;
  const items = Array.from({ length: total }, (_, index) => fileEntry(`file-${index}`));
  const pages = new Map([
    [1, { items: items.slice(0, 100), page: 1, pageSize: 100, total, totalPages: 2 }],
    [2, { items: items.slice(100), page: 2, pageSize: 100, total, totalPages: 2 }],
  ]);
  const requestedPages = [];

  const collected = await collectAllUserFiles(async (page) => {
    requestedPages.push(page);
    return pages.get(page);
  });

  assert.deepEqual(requestedPages, [1, 2]);
  assert.equal(collected.length, total);
  assert.ok(collected.includes(collected.find((entry) => entry.fileId === "file-120")), "第二页的活动背景必须能找到");
  assert.equal(collected[100].fileId, "file-100");
});

test("空列表或单页列表不会额外请求下一页", async () => {
  const single = await collectAllUserFiles(async (page) => {
    assert.equal(page, 1);
    return { items: [fileEntry("only")], page: 1, pageSize: 100, total: 1, totalPages: 1 };
  });
  assert.equal(single.length, 1);

  const empty = await collectAllUserFiles(async (page) => {
    assert.equal(page, 1);
    return { items: [], page: 1, pageSize: 100, total: 0, totalPages: 0 };
  });
  assert.deepEqual(empty, []);
});

test("活动背景位于第二页时，完整收集文件后 hydrate 仍能恢复该背景", async () => {
  const total = 150;
  const items = Array.from({ length: total }, (_, index) => ({
    ...fileEntry(`file-${index}`),
    contentType: "image/png",
  }));
  const pages = new Map([
    [1, { items: items.slice(0, 100), page: 1, pageSize: 100, total, totalPages: 2 }],
    [2, { items: items.slice(100), page: 2, pageSize: 100, total, totalPages: 2 }],
  ]);
  const collected = await collectAllUserFiles(async (page) => pages.get(page));
  assert.equal(collected.length, total);

  const customLayer = { style: {} };
  const root = {
    dataset: {},
    style: {},
    querySelector: (selector) => selector === ".console-background--custom" ? customLayer : null,
  };
  const previousCreate = URL.createObjectURL;
  URL.createObjectURL = () => "blob:page2-background";
  try {
    const controller = createBackgroundController(root, null, async (file) => new Blob([file.fileId]));
    await controller.hydrate({
      fileIds: collected.map((file) => file.fileId),
      activeFileId: "file-120",
      mode: "default",
      kuliantnt: false,
    }, collected);
    assert.equal(controller.selection().activeFileId, "file-120");
    assert.equal(customLayer.style.backgroundImage, 'url("blob:page2-background")');
    assert.equal(controller.lastError(), null);
  } finally {
    URL.createObjectURL = previousCreate;
  }
});
