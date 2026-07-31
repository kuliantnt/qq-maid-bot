import assert from "node:assert/strict";
import test from "node:test";

import {
  BACKGROUND_MODE_COOKIE,
  BACKGROUND_UNLOCK_COOKIE,
  BACKGROUND_TRANSITION_INDEX_COOKIE,
  createBackgroundController,
  installBackgroundConsoleUnlock,
} from "../dist/background.js";

function cookieDocument(initial = "") {
  let value = initial;
  return {
    get cookie() {
      return value;
    },
    set cookie(next) {
      const parts = next.split(";").map((part) => part.trim());
      const [pair] = parts;
      const maxAge = parts.find((part) => part.startsWith("Max-Age="));
      if (pair && maxAge && Number(maxAge.slice("Max-Age=".length)) <= 0) {
        const [name] = pair.split("=");
        value = value.split(";").map((part) => part.trim())
          .filter((part) => !part.startsWith(`${name}=`))
          .join("; ");
      } else if (pair) {
        value = value ? `${value}; ${pair}` : pair;
      }
    },
    read() {
      return value;
    },
  };
}

test("背景默认模式不读取未解锁的特殊模式", () => {
  const cookies = cookieDocument(`${BACKGROUND_MODE_COOKIE}=special`);
  const root = { dataset: {} };
  const controller = createBackgroundController(root, cookies);

  assert.equal(controller.current(), "default");
  assert.equal(controller.isUnlocked(), false);
  assert.equal(root.dataset.background, "default");
});

test("控制台解锁切换特殊背景并触发持久化 hook，不再写 cookie", () => {
  const cookies = cookieDocument();
  const root = { dataset: {} };
  const calls = [];
  const controller = createBackgroundController(root, cookies, undefined, () => calls.push(true));
  const target = {};
  installBackgroundConsoleUnlock(target, controller);

  assert.equal(target.kuliantnt, "特殊背景已解锁");
  assert.equal(controller.current(), "special");
  assert.equal(controller.isUnlocked(), true);
  assert.equal(root.dataset.background, "special");
  assert.deepEqual(calls, [true]);
  assert.equal(cookies.read(), "");
});

test("解锁后可以切回默认（无背景）；服务端偏好 hydrate 恢复解锁状态", async () => {
  const controller = createBackgroundController({ dataset: {} }, cookieDocument());

  controller.unlock();
  controller.select("default");
  assert.equal(controller.current(), "default");
  assert.equal(controller.select("special"), "special");

  const refreshed = createBackgroundController({ dataset: {} }, cookieDocument());
  await refreshed.hydrate({ fileIds: [], activeFileId: null, mode: "default", kuliantnt: true }, []);
  assert.equal(refreshed.isUnlocked(), true);
  assert.equal(refreshed.current(), "default");
  assert.equal(refreshed.select("special"), "special");
});

test("默认（无背景）模式不提供过渡中心图，特殊背景按拼图切片循环", () => {
  const cookies = cookieDocument(`${BACKGROUND_UNLOCK_COOKIE}=1; ${BACKGROUND_MODE_COOKIE}=special`);
  const root = { dataset: {} };
  const controller = createBackgroundController(root, cookies);

  assert.deepEqual(controller.nextTransitionImage(), {
    url: "/console/background/special.webp",
    position: "0% 0%",
  });
  assert.deepEqual(controller.nextTransitionImage(), {
    url: "/console/background/special.webp",
    position: "50% 0%",
  });
  controller.select("default");
  assert.equal(controller.nextTransitionImage(), null);
});

test("自定义背景替换 object URL 时撤销旧 URL，读取失败不改变当前 URL", async () => {
  const cookies = cookieDocument();
  const customLayer = { style: {} };
  const root = {
    dataset: {},
    style: {},
    querySelector: (selector) => selector === ".console-background--custom" ? customLayer : null,
  };
  const created = [];
  const revoked = [];
  const previousCreate = URL.createObjectURL;
  const previousRevoke = URL.revokeObjectURL;
  URL.createObjectURL = () => { created.push(true); return `blob:${created.length}`; };
  URL.revokeObjectURL = (url) => revoked.push(url);
  try {
    const controller = createBackgroundController(root, cookies, async (file) => {
      if (file.fileId === "bad") throw new Error("read failed");
      return new Blob([file.fileId]);
    });
    await controller.selectFile({ fileId: "first", filename: "first.png", url: "/first" });
    await controller.selectFile({ fileId: "second", filename: "second.png", url: "/second" });
    assert.deepEqual(revoked, ["blob:1"]);
    await assert.rejects(() => controller.selectFile({ fileId: "bad", filename: "bad.png", url: "/bad" }));
    assert.equal(customLayer.style.backgroundImage, 'url("blob:2")');
    assert.equal(root.dataset.background, "custom");
    controller.dispose();
    assert.deepEqual(revoked, ["blob:1", "blob:2"]);
    assert.equal(customLayer.style.backgroundImage, "");
  } finally {
    URL.createObjectURL = previousCreate;
    URL.revokeObjectURL = previousRevoke;
  }
});

test("特殊背景解锁调用 typed persistence hook", () => {
  const root = { dataset: {}, style: {} };
  const calls = [];
  const controller = createBackgroundController(root, cookieDocument(), undefined, () => calls.push(true));
  const target = {};
  installBackgroundConsoleUnlock(target, controller);
  assert.equal(target.kuliantnt, "特殊背景已解锁");
  assert.deepEqual(calls, [true]);
});

test("一次性迁移把旧解锁 cookie 写入服务端偏好并清理全部旧 cookie", async () => {
  const cookies = cookieDocument(
    `${BACKGROUND_UNLOCK_COOKIE}=1; ${BACKGROUND_MODE_COOKIE}=special; ${BACKGROUND_TRANSITION_INDEX_COOKIE}=3`,
  );
  const root = { dataset: {} };
  const controller = createBackgroundController(root, cookies);
  let persisted = false;

  await controller.migrateFromLegacy({ kuliantnt: false, backgroundMode: "default" }, async () => { persisted = true; });

  assert.equal(persisted, true);
  assert.equal(controller.isUnlocked(), true);
  assert.equal(cookies.read(), "");
});

test("服务端已解锁时迁移不再持久化，仅清理旧 cookie", async () => {
  const cookies = cookieDocument(`${BACKGROUND_UNLOCK_COOKIE}=1; ${BACKGROUND_MODE_COOKIE}=special`);
  const root = { dataset: {} };
  const controller = createBackgroundController(root, cookies);
  let persisted = false;

  await controller.migrateFromLegacy({ kuliantnt: true, backgroundMode: "special" }, async () => { persisted = true; });

  assert.equal(persisted, false);
  assert.equal(controller.isUnlocked(), true);
  assert.equal(cookies.read(), "");
});

test("迁移持久化失败时向外抛出且保留旧 cookie", async () => {
  const cookies = cookieDocument(`${BACKGROUND_UNLOCK_COOKIE}=1; ${BACKGROUND_MODE_COOKIE}=special`);
  const root = { dataset: {} };
  const controller = createBackgroundController(root, cookies);

  await assert.rejects(
    () => controller.migrateFromLegacy({ kuliantnt: false, backgroundMode: "default" }, async () => { throw new Error("persist failed"); }),
    /persist failed/,
  );
  assert.match(cookies.read(), new RegExp(`${BACKGROUND_UNLOCK_COOKIE}=1`));
  assert.match(cookies.read(), new RegExp(`${BACKGROUND_MODE_COOKIE}=special`));
});

test("认证后选择、解锁、过渡与自定义背景不再写入任何 cookie", async () => {
  const cookies = cookieDocument(`${BACKGROUND_UNLOCK_COOKIE}=1; ${BACKGROUND_MODE_COOKIE}=special`);
  const root = { dataset: {}, style: {} };
  const created = [];
  const previousCreate = URL.createObjectURL;
  const previousRevoke = URL.revokeObjectURL;
  URL.createObjectURL = () => { created.push(true); return `blob:${created.length}`; };
  URL.revokeObjectURL = () => undefined;
  try {
    const controller = createBackgroundController(root, cookies, async (file) => new Blob([file.fileId]));
    await controller.hydrate({ fileIds: [], activeFileId: null, mode: "default", kuliantnt: true }, []);
    await controller.migrateFromLegacy({ kuliantnt: true, backgroundMode: "default" }, async () => {});
    assert.equal(cookies.read(), "");

    controller.unlock();
    assert.deepEqual(controller.nextTransitionImage(), {
      url: "/console/background/special.webp",
      position: "0% 0%",
    });
    controller.select("default");
    assert.equal(controller.nextTransitionImage(), null);
    await controller.selectFile({ fileId: "custom", filename: "custom.png", url: "/custom" });
    assert.equal(cookies.read(), "");
  } finally {
    URL.createObjectURL = previousCreate;
    URL.revokeObjectURL = previousRevoke;
  }
});

test("hydrate 读取背景内容失败时回退默认（无背景）状态、撤销 object URL 且不清理 cookie", async () => {
  const cookies = cookieDocument(`${BACKGROUND_UNLOCK_COOKIE}=1`);
  const customLayer = { style: {} };
  const root = {
    dataset: {},
    style: {},
    querySelector: (selector) => selector === ".console-background--custom" ? customLayer : null,
  };
  const revoked = [];
  const previousCreate = URL.createObjectURL;
  const previousRevoke = URL.revokeObjectURL;
  URL.createObjectURL = () => "blob:created";
  URL.revokeObjectURL = (url) => revoked.push(url);
  try {
    const controller = createBackgroundController(root, cookies, async (file) => {
      if (file.fileId === "bad") throw new Error("read failed");
      return new Blob([file.fileId]);
    });
    await controller.hydrate({
      fileIds: ["a"],
      activeFileId: "a",
      mode: "default",
      kuliantnt: false,
    }, [{ fileId: "a", filename: "a.png", url: "/a" }]);
    assert.equal(customLayer.style.backgroundImage, 'url("blob:created")');

    await controller.hydrate({
      fileIds: ["a", "bad"],
      activeFileId: "bad",
      mode: "default",
      kuliantnt: false,
    }, [
      { fileId: "a", filename: "a.png", url: "/a" },
      { fileId: "bad", filename: "bad.png", url: "/bad" },
    ]);

    assert.equal(controller.current(), "default");
    assert.equal(controller.selection().activeFileId, null);
    assert.equal(root.dataset.background, "default");
    assert.equal(customLayer.style.backgroundImage, "");
    assert.deepEqual(revoked, ["blob:created"]);
    assert.match(cookies.read(), new RegExp(`${BACKGROUND_UNLOCK_COOKIE}=1`));
  } finally {
    URL.createObjectURL = previousCreate;
    URL.revokeObjectURL = previousRevoke;
  }
});

test("selectFile 把 forceRefresh 透传给读取器，hydrate 调用时不传", async () => {
  const cookies = cookieDocument();
  const customLayer = { style: {} };
  const root = {
    dataset: {},
    style: {},
    querySelector: (selector) => selector === ".console-background--custom" ? customLayer : null,
  };
  const readCalls = [];
  const previousCreate = URL.createObjectURL;
  const previousRevoke = URL.revokeObjectURL;
  URL.createObjectURL = () => "blob:created";
  URL.revokeObjectURL = () => undefined;
  try {
    const controller = createBackgroundController(root, cookies, async (file, forceRefresh) => {
      readCalls.push(forceRefresh);
      return new Blob([file.fileId]);
    });
    await controller.selectFile({ fileId: "a", filename: "a.png", url: "/a" }, true);
    assert.deepEqual(readCalls, [true]);
    await controller.selectFile({ fileId: "b", filename: "b.png", url: "/b" });
    assert.deepEqual(readCalls, [true, undefined]);

    await controller.hydrate({
      fileIds: ["a", "b"],
      activeFileId: "a",
      mode: "default",
      kuliantnt: false,
    }, [
      { fileId: "a", filename: "a.png", url: "/a" },
      { fileId: "b", filename: "b.png", url: "/b" },
    ]);
    assert.deepEqual(readCalls, [true, undefined, undefined]);
    assert.equal(customLayer.style.backgroundImage, 'url("blob:created")');
  } finally {
    URL.createObjectURL = previousCreate;
    URL.revokeObjectURL = previousRevoke;
  }
});

test("删除非激活文件保持当前背景，删除激活文件后重置为默认并撤销 object URL", async () => {
  const cookies = cookieDocument();
  const customLayer = { style: {} };
  const root = {
    dataset: {},
    style: {},
    querySelector: (selector) => selector === ".console-background--custom" ? customLayer : null,
  };
  const revoked = [];
  const previousCreate = URL.createObjectURL;
  const previousRevoke = URL.revokeObjectURL;
  URL.createObjectURL = () => "blob:active";
  URL.revokeObjectURL = (url) => revoked.push(url);
  try {
    const controller = createBackgroundController(root, cookies, async (file) => new Blob([file.fileId]));
    await controller.hydrate({
      fileIds: ["a", "b"],
      activeFileId: "a",
      mode: "default",
      kuliantnt: false,
    }, [
      { fileId: "a", filename: "a.png", url: "/a" },
      { fileId: "b", filename: "b.png", url: "/b" },
    ]);
    assert.equal(customLayer.style.backgroundImage, 'url("blob:active")');

    controller.deleteFile("b");
    assert.equal(controller.selection().activeFileId, "a");
    assert.deepEqual(revoked, []);

    controller.deleteFile("a");
    assert.equal(controller.selection().activeFileId, null);
    assert.equal(controller.current(), "default");
    assert.equal(customLayer.style.backgroundImage, "");
    assert.deepEqual(revoked, ["blob:active"]);
  } finally {
    URL.createObjectURL = previousCreate;
    URL.revokeObjectURL = previousRevoke;
  }
});

test("服务端 special 模式 hydrate 后刷新仍为 special（而不是 default）", async () => {
  const controller = createBackgroundController({ dataset: {} }, cookieDocument());
  await controller.hydrate({
    fileIds: [],
    activeFileId: null,
    mode: "special",
    kuliantnt: true,
  }, []);

  assert.equal(controller.current(), "special");
  assert.equal(controller.selection().activeFileId, null);

  // 新会话再次用服务端偏好 hydrate（模拟刷新）后仍然一致。
  const refreshed = createBackgroundController({ dataset: {} }, cookieDocument());
  await refreshed.hydrate({
    fileIds: [],
    activeFileId: null,
    mode: "special",
    kuliantnt: true,
  }, []);
  assert.equal(refreshed.current(), "special");
  assert.equal(refreshed.lastError(), null);
});

test("选择无背景后服务端 default 模式 hydrate 刷新仍为 default", async () => {
  const controller = createBackgroundController({ dataset: {} }, cookieDocument());
  controller.unlock();
  controller.select("special");
  assert.equal(controller.current(), "special");

  controller.select("default");
  assert.equal(controller.current(), "default");

  const refreshed = createBackgroundController({ dataset: {} }, cookieDocument());
  await refreshed.hydrate({
    fileIds: [],
    activeFileId: null,
    mode: "default",
    kuliantnt: true,
  }, []);
  assert.equal(refreshed.current(), "default");
  assert.equal(refreshed.selection().activeFileId, null);
});

test("旧 special Cookie 一次性迁移：解锁状态与背景模式一起写入服务端成功后清理 Cookie", async () => {
  const cookies = cookieDocument(
    `${BACKGROUND_UNLOCK_COOKIE}=1; ${BACKGROUND_MODE_COOKIE}=special`,
  );
  const controller = createBackgroundController({ dataset: {} }, cookies);
  const persistedPatches = [];

  await controller.migrateFromLegacy({
    kuliantnt: false,
    backgroundMode: "default",
  }, async (patch) => {
    persistedPatches.push(patch);
  });

  assert.deepEqual(persistedPatches, [{ kuliantnt: true, backgroundMode: "special" }]);
  assert.equal(controller.isUnlocked(), true);
  assert.equal(cookies.read(), "");
});

test("服务端已是 special 模式时迁移不再写入模式字段，仅清理 Cookie", async () => {
  const cookies = cookieDocument(
    `${BACKGROUND_UNLOCK_COOKIE}=1; ${BACKGROUND_MODE_COOKIE}=special`,
  );
  const controller = createBackgroundController({ dataset: {} }, cookies);
  const persistedPatches = [];

  await controller.migrateFromLegacy({
    kuliantnt: true,
    backgroundMode: "special",
  }, async (patch) => {
    persistedPatches.push(patch);
  });

  assert.deepEqual(persistedPatches, []);
  assert.equal(cookies.read(), "");
});

test("旧特殊模式迁移写入失败时保留 Cookie 并向外抛出", async () => {
  const cookies = cookieDocument(`${BACKGROUND_MODE_COOKIE}=special`);
  const controller = createBackgroundController({ dataset: {} }, cookies);

  await assert.rejects(
    () => controller.migrateFromLegacy({
      kuliantnt: false,
      backgroundMode: "default",
    }, async () => {
      throw new Error("mode persist failed");
    }),
    /mode persist failed/,
  );
  assert.match(cookies.read(), new RegExp(`${BACKGROUND_MODE_COOKIE}=special`));
});

test("自定义背景模式由 active 文件表达：hydrate 后模式字段恒为 default 并激活文件", async () => {
  const customLayer = { style: {} };
  const root = {
    dataset: {},
    style: {},
    querySelector: (selector) => selector === ".console-background--custom" ? customLayer : null,
  };
  const previousCreate = URL.createObjectURL;
  URL.createObjectURL = () => "blob:custom";
  try {
    const controller = createBackgroundController(root, cookieDocument(), async (file) => new Blob([file.fileId]));
    await controller.hydrate({
      fileIds: ["a"],
      activeFileId: "a",
      mode: "default",
      kuliantnt: false,
    }, [{ fileId: "a", filename: "a.png", url: "/a" }]);
    assert.equal(controller.selection().activeFileId, "a");
    assert.equal(controller.current(), "default");
    assert.equal(customLayer.style.backgroundImage, 'url("blob:custom")');
  } finally {
    URL.createObjectURL = previousCreate;
  }
});
