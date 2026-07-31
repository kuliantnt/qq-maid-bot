import assert from "node:assert/strict";
import test from "node:test";

import { createBackgroundController } from "../dist/background.js";
import { activateBackgroundFile } from "../dist/views/theme-selector.js";

function fakeRoot(customLayer) {
  return {
    dataset: {},
    style: {},
    querySelector: (selector) => selector === ".console-background--custom" ? customLayer : null,
  };
}

function makeUserData({ failPersist = false, failRollback = false } = {}) {
  const userData = {
    preferences: {
      customColors: [],
      backgroundFileIds: [],
      activeBackgroundFileId: null,
      backgroundMode: "default",
      kuliantnt: false,
    },
    updateCalls: [],
    updatePreferences: async (patch) => {
      userData.updateCalls.push(patch);
      if (patch.activeBackgroundFileId === null && failRollback) throw new Error("rollback failed");
      if (patch.activeBackgroundFileId !== null && failPersist) throw new Error("persist failed");
      userData.preferences = { ...userData.preferences, ...patch };
      return userData.preferences;
    },
  };
  return userData;
}

test("激活成功：先读 Blob，再写服务端，最后应用本地背景", async () => {
  const customLayer = { style: {} };
  const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
  const userData = makeUserData();
  const statuses = [];
  const previousCreate = URL.createObjectURL;
  URL.createObjectURL = () => "blob:activated";
  try {
    await activateBackgroundFile({
      userData,
      controller,
      setStatus: (text) => statuses.push(text),
    }, { fileId: "a", filename: "a.png", url: "/a" }, true);

    assert.deepEqual(userData.updateCalls, [{
      backgroundFileIds: ["a"],
      activeBackgroundFileId: "a",
    }]);
    assert.equal(controller.selection().activeFileId, "a");
    assert.equal(customLayer.style.backgroundImage, 'url("blob:activated")');
    assert.equal(statuses.at(-1), "背景已保存。");
  } finally {
    URL.createObjectURL = previousCreate;
  }
});

test("新上传背景读取失败时不留下活动背景 ID，且不提交服务端", async () => {
  const customLayer = { style: {} };
  const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => {
    if (file.fileId === "bad") throw new Error("read failed");
    return new Blob([file.fileId]);
  });
  const userData = makeUserData();
  const statuses = [];
  const previousCreate = URL.createObjectURL;
  URL.createObjectURL = () => "blob:kept";
  try {
    await controller.selectFile({ fileId: "existing", filename: "existing.png", url: "/existing" });
    const kept = customLayer.style.backgroundImage;

    await activateBackgroundFile({
      userData,
      controller,
      setStatus: (text) => statuses.push(text),
    }, { fileId: "bad", filename: "bad.png", url: "/bad" }, true);

    assert.equal(statuses.at(-1), "背景读取失败，已保留原背景：read failed");
    assert.equal(userData.updateCalls.length, 0, "读取失败时不能提交 active_background_file_id");
    assert.equal(controller.selection().activeFileId, "existing");
    assert.equal(customLayer.style.backgroundImage, kept, "必须保留原浏览器背景");
  } finally {
    URL.createObjectURL = previousCreate;
  }
});

test("已有背景与新上传背景读取失败行为一致（同一激活函数）", async () => {
  const run = async (fileId) => {
    const customLayer = { style: {} };
    const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => {
      throw new Error("read failed");
    });
    const userData = makeUserData();
    const statuses = [];
    await activateBackgroundFile({
      userData,
      controller,
      setStatus: (text) => statuses.push(text),
    }, { fileId, filename: `${fileId}.png`, url: `/${fileId}` }, true);
    return { status: statuses.at(-1), updates: userData.updateCalls.length, active: controller.selection().activeFileId };
  };

  const existing = await run("existing-file");
  const uploaded = await run("uploaded-file");
  assert.deepEqual(existing, uploaded);
  assert.equal(existing.updates, 0);
  assert.equal(existing.active, null);
});

test("服务端写入失败时不激活本地，原 object URL 保留", async () => {
  const customLayer = { style: {} };
  const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
  const userData = makeUserData({ failPersist: true });
  const statuses = [];
  const previousCreate = URL.createObjectURL;
  URL.createObjectURL = () => "blob:kept";
  try {
    await controller.selectFile({ fileId: "old", filename: "old.png", url: "/old" });
    const kept = customLayer.style.backgroundImage;

    await activateBackgroundFile({
      userData,
      controller,
      setStatus: (text) => statuses.push(text),
    }, { fileId: "new", filename: "new.png", url: "/new" }, true);

    assert.match(statuses.at(-1), /背景保存失败，已保留原背景：persist failed/);
    assert.equal(controller.selection().activeFileId, "old");
    assert.equal(customLayer.style.backgroundImage, kept);
  } finally {
    URL.createObjectURL = previousCreate;
  }
});

test("本地应用失败时回滚服务端活动背景；回滚失败也如实显示且不产生未处理 rejection", async () => {
  const customLayer = { style: {} };
  const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
  const userData = makeUserData({ failRollback: true });
  const statuses = [];
  const previousCreate = URL.createObjectURL;
  URL.createObjectURL = () => {
    throw new Error("object url failed");
  };
  try {
    // activateBackgroundFile 内部捕获所有错误，promise 必须正常 resolve，不能产生未处理 rejection。
    await activateBackgroundFile({
      userData,
      controller,
      setStatus: (text) => statuses.push(text),
    }, { fileId: "a", filename: "a.png", url: "/a" }, true);

    const finalStatus = statuses.at(-1);
    assert.match(finalStatus, /背景应用失败（object url failed）/);
    assert.match(finalStatus, /回滚活动背景也失败：rollback failed/);
    // 服务端先提交了活动背景，随后回滚请求失败；本地没有激活任何文件。
    assert.deepEqual(userData.updateCalls, [
      { backgroundFileIds: ["a"], activeBackgroundFileId: "a" },
      { activeBackgroundFileId: null },
    ]);
    assert.equal(controller.selection().activeFileId, null);
  } finally {
    URL.createObjectURL = previousCreate;
  }
});

test("本地应用失败但回滚成功时恢复服务端活动背景并显示明确信息", async () => {
  const customLayer = { style: {} };
  const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
  const userData = makeUserData();
  const statuses = [];
  const previousCreate = URL.createObjectURL;
  URL.createObjectURL = () => {
    throw new Error("object url failed");
  };
  try {
    await activateBackgroundFile({
      userData,
      controller,
      setStatus: (text) => statuses.push(text),
    }, { fileId: "a", filename: "a.png", url: "/a" }, true);

    assert.equal(statuses.at(-1), "背景应用失败，已回滚活动背景：object url failed");
    assert.equal(controller.selection().activeFileId, null);
  } finally {
    URL.createObjectURL = previousCreate;
  }
});
