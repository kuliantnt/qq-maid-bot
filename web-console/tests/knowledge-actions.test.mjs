import test from "node:test";
import assert from "node:assert/strict";

import { ConsoleApiError } from "../dist/api.js";
import { createKnowledgeActionHandlers, triggerBrowserDownload } from "../dist/views/knowledge/knowledge-actions.js";
import { createFakeDom, installDomGlobals } from "./helpers/fake-dom.mjs";

function item(overrides = {}) {
  return { file_id: "file-1", filename: "guide.md", source: "managed", status: "ready", downloadable: true, ...overrides };
}

function setup() {
  installDomGlobals(createFakeDom());
  document.body = document.createElement("div");
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
  document.body?.append(value);
  return value;
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

test("browser download attaches the anchor and defers blob URL revocation", () => {
  const previousDocument = globalThis.document;
  const previousUrl = globalThis.URL;
  const previousWindow = globalThis.window;
  const href = "blob:test-download";
  const revoked = [];
  const timers = [];
  const body = {
    children: [],
    append(anchor) {
      this.children.push(anchor);
      anchor.parentNode = this;
    },
    removeChild(anchor) {
      this.children.splice(this.children.indexOf(anchor), 1);
      anchor.parentNode = null;
    },
  };
  const anchor = {
    style: {},
    href: "",
    download: "",
    parentNode: null,
    click() {
      assert.equal(this.parentNode, body);
      assert.equal(revoked.length, 0);
    },
    remove() {
      this.parentNode.removeChild(this);
    },
  };
  globalThis.document = { body, createElement: () => anchor };
  globalThis.URL = {
    createObjectURL: (blob) => {
      assert.ok(blob instanceof Blob);
      return href;
    },
    revokeObjectURL: (value) => revoked.push(value),
  };
  globalThis.window = { setTimeout: (callback, delay) => timers.push({ callback, delay }) };

  try {
    triggerBrowserDownload(new Blob(["guide"]), "guide.md");
    assert.equal(anchor.download, "guide.md");
    assert.equal(anchor.href, href);
    assert.equal(anchor.style.display, "none");
    assert.equal(body.children.length, 0);
    assert.deepEqual(revoked, []);
    assert.equal(timers.length, 1);
    assert.equal(timers[0].delay, 60_000);
    timers[0].callback();
    assert.deepEqual(revoked, [href]);
  } finally {
    globalThis.document = previousDocument;
    globalThis.URL = previousUrl;
    globalThis.window = previousWindow;
  }
});

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

test("delete dialog is a singleton and Escape restores the opener focus", () => {
  const { handlers } = setup();
  const first = button("file-a", "删除");
  handlers.onDelete(item({ file_id: "file-a", filename: "a.md" }));
  const dialog = document.querySelector('[role="alertdialog"]');
  handlers.onDelete(item({ file_id: "file-a", filename: "b.md" }));
  assert.equal(document.querySelectorAll("#knowledge-delete-title").length, 1);
  assert.equal(document.querySelectorAll("#knowledge-delete-message").length, 1);
  assert.equal(document.querySelectorAll('[role="alertdialog"]').length, 1);
  dialog.listeners.get("cancel")[0]({});
  assert.equal(dialog.getAttribute("open"), null);
  assert.equal(first.focused, true);
});
