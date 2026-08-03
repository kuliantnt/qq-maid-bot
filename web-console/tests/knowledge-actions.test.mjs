import test from "node:test";
import assert from "node:assert/strict";

import { ConsoleApiError } from "../dist/api.js";
import { createKnowledgeActionHandlers } from "../dist/views/knowledge/knowledge-actions.js";
import { createFakeDom, installDomGlobals } from "./helpers/fake-dom.mjs";

function item(overrides = {}) {
  return { file_id: "file-1", filename: "guide.md", source: "managed", status: "ready", downloadable: true, ...overrides };
}

function setup() {
  installDomGlobals(createFakeDom());
  const statuses = [];
  const refreshes = [];
  const handlers = createKnowledgeActionHandlers({
    setStatus: (text) => statuses.push(text),
    download: async () => ({ blob: new Blob(["guide"]), filename: "guide.md" }),
    triggerDownload: () => undefined,
    deleteFile: async () => undefined,
    retryFile: async () => item({ status: "pending" }),
    refresh: (reason) => refreshes.push(reason),
    getItems: () => [],
  });
  return { handlers, statuses, refreshes };
}

function button(fileId, label) {
  const value = document.createElement("button");
  value.dataset.fileId = fileId;
  value.textContent = label;
  return value;
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

test("download guards unavailable rows and disables its own button while running", async () => {
  const { handlers } = setup();
  let resolveDownload;
  let downloads = 0;
  const controlled = createKnowledgeActionHandlers({
    setStatus: () => undefined,
    download: () => new Promise((resolve) => { downloads += 1; resolveDownload = resolve; }),
    triggerDownload: () => undefined,
    deleteFile: async () => undefined,
    retryFile: async () => item(),
    refresh: () => undefined,
    getItems: () => [],
  });
  const downloadButton = button("file-1", "下载");
  controlled.onDownload(item());
  controlled.onDownload(item());
  assert.equal(downloadButton.disabled, true);
  assert.equal(downloads, 1);
  resolveDownload({ blob: new Blob(["x"]), filename: "guide.md" });
  await flush();
  assert.equal(downloadButton.disabled, false);
  handlers.onDownload(item({ source: "directory" }));
  handlers.onDownload(item({ downloadable: false }));
});

test("retry only submits failed managed files and refreshes after success", async () => {
  const { handlers, refreshes } = setup();
  button("file-1", "重新处理");
  handlers.onRetry(item({ status: "failed" }));
  await flush();
  assert.deepEqual(refreshes, ["retry"]);
  handlers.onRetry(item());
  await flush();
  assert.deepEqual(refreshes, ["retry"]);
});

test("delete dialog has safe focus, confirmation guard, and conflict feedback", async () => {
  const { handlers, statuses, refreshes } = setup();
  const opener = button("file-1", "删除");
  let resolveDelete;
  const controlled = createKnowledgeActionHandlers({
    setStatus: (text) => statuses.push(text),
    download: async () => ({ blob: new Blob(), filename: "guide.md" }),
    triggerDownload: () => undefined,
    deleteFile: () => new Promise((resolve) => { resolveDelete = resolve; }),
    retryFile: async () => item(),
    refresh: (reason) => refreshes.push(reason),
    getItems: () => [],
  });
  controlled.onDelete(item());
  const dialog = document.querySelector('[role="alertdialog"]');
  assert.equal(dialog.getAttribute("aria-modal"), "true");
  assert.match(dialog.children[0].textContent, /guide\.md/);
  assert.match(dialog.children[1].textContent, /无法继续被检索/);
  const cancel = dialog.children[2];
  const confirm = dialog.children[3];
  assert.equal(cancel.focused, true);
  dialog.listeners.get("click")[0]({ target: dialog });
  assert.equal(dialog.getAttribute("open"), "");
  confirm.onclick();
  confirm.onclick();
  assert.equal(confirm.disabled, true);
  resolveDelete();
  await flush();
  assert.deepEqual(refreshes, ["delete"]);
  assert.equal(statuses.at(-1), "文件已删除");
  assert.equal(opener.focused, true);

  const conflict = setup();
  const conflictOpener = button("file-1", "删除");
  const conflictHandlers = createKnowledgeActionHandlers({
    setStatus: (text) => conflict.statuses.push(text),
    download: async () => ({ blob: new Blob(), filename: "guide.md" }), triggerDownload: () => undefined,
    deleteFile: async () => { throw new ConsoleApiError("冲突", "conflict", 409); }, retryFile: async () => item(),
    refresh: () => undefined, getItems: () => [],
  });
  conflictHandlers.onDelete(item());
  document.querySelector('[role="alertdialog"]').children[3].onclick();
  await flush();
  assert.equal(conflict.statuses.at(-1), "文件正在处理中，暂不能删除");
  assert.equal(conflictOpener.focused, true);
  conflictHandlers.onDelete(item({ status: "processing" }));
});
