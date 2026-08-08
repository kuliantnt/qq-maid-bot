import assert from "node:assert/strict";
import test from "node:test";

import {
  CONSOLE_THEME_STORAGE_KEY,
  DEFAULT_CONSOLE_THEME,
  createThemeController,
  parseStoredTheme,
  serializeTheme,
  safeCustomColors,
} from "../dist/theme.js";
import { renderThemeSelector } from "../dist/views/configuration/theme-selector.js";
import { createFakeDom, installDomGlobals, clearDomGlobals } from "./helpers/fake-dom.mjs";

test("缺失、损坏和未知主题回退到默认值且不改变存储格式", () => {
  const fallback = { preset: DEFAULT_CONSOLE_THEME, version: 2 };

  assert.deepEqual(parseStoredTheme(null), fallback);
  assert.deepEqual(parseStoredTheme("not-json"), fallback);
  assert.deepEqual(parseStoredTheme('{"preset":"unknown","version":2}'), fallback);
  assert.deepEqual(parseStoredTheme('{"preset":"night-shift","version":2}'), fallback);
});

test("version 1 旧主题迁移到语义主题且不保留已移除预设", () => {
  assert.deepEqual(parseStoredTheme('{"preset":"night-shift","version":1}'), { preset: "night-green", version: 2 });
  assert.deepEqual(parseStoredTheme('{"preset":"ember-grid","version":1}'), { preset: DEFAULT_CONSOLE_THEME, version: 2 });
  assert.deepEqual(parseStoredTheme('{"preset":"tide-signal","version":1}'), { preset: DEFAULT_CONSOLE_THEME, version: 2 });
});

test("有效主题按 version 2 的本地存储契约往返解析", () => {
  const preference = { preset: "light", version: 2 };

  assert.equal(serializeTheme(preference), '{"preset":"light","version":2}');
  assert.deepEqual(parseStoredTheme(serializeTheme(preference)), preference);
  assert.equal(CONSOLE_THEME_STORAGE_KEY, "console-theme");
});

test("主题控制器应用语义 token 并持久化选择，刷新后保持", () => {
  const values = new Map();
  const properties = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
  };
  const root = { dataset: {}, style: { setProperty: (name, value) => properties.set(name, value) } };
  const controller = createThemeController(storage, root);

  assert.equal(root.dataset.theme, DEFAULT_CONSOLE_THEME);
  assert.equal(properties.get("--console-background"), "#0D1117");
  assert.equal(properties.get("--console-card"), "#21262D");
  assert.equal(properties.get("--console-accent"), "#3FB950");

  controller.select("night-green");
  assert.equal(root.dataset.theme, "night-green");
  assert.equal(properties.get("--console-background"), "#101714");
  assert.equal(values.get(CONSOLE_THEME_STORAGE_KEY), '{"preset":"night-green","version":2}');

  const refreshedRoot = { dataset: {}, style: { setProperty: () => {} } };
  const refreshed = createThemeController(storage, refreshedRoot);
  assert.equal(refreshed.current().preset, "night-green");
  assert.equal(refreshedRoot.dataset.theme, "night-green");

  controller.reset();
  assert.equal(root.dataset.theme, DEFAULT_CONSOLE_THEME);
  assert.equal(values.has(CONSOLE_THEME_STORAGE_KEY), false);
});

test("localStorage 失败时主题控制器继续使用内存状态", () => {
  const storage = {
    getItem: () => { throw "storage blocked"; },
    setItem: () => { throw "storage blocked"; },
    removeItem: () => { throw "storage blocked"; },
  };
  const root = { dataset: {} };
  const controller = createThemeController(storage, root);

  assert.equal(controller.current().preset, DEFAULT_CONSOLE_THEME);
  controller.select("light");
  assert.equal(root.dataset.theme, "light");
  controller.reset();
  assert.equal(root.dataset.theme, DEFAULT_CONSOLE_THEME);
});

test("自定义颜色只接受三个安全六位 hex 值", () => {
  assert.deepEqual(safeCustomColors(["#abc", "red", "#112233", "#445566", "#778899"]), ["#112233", "#445566", "#778899"]);
  assert.deepEqual(safeCustomColors(["#112233", "#445566"]), ["#112233", "#445566"]);
});

function selectorUserData(updatePreferences, files = []) {
  return {
    preferences: {
      customColors: [],
      backgroundFileIds: files.map((file) => file.fileId),
      activeBackgroundFileId: null,
      backgroundMode: "default",
      kuliantnt: false,
    },
    files,
    updatePreferences,
  };
}

function selectorBackgroundController() {
  let mode = "default";
  return {
    current: () => mode,
    isUnlocked: () => false,
    lastError: () => null,
    select: (next) => { mode = next; },
    deleteFile: () => undefined,
  };
}

test("主题保存失败恢复原 radio，并在保存期间锁定整个选择器", async () => {
  const fake = createFakeDom();
  installDomGlobals(fake);
  try {
    const target = document.createElement("div");
    const root = { dataset: {}, style: { setProperty: () => {}, removeProperty: () => {} } };
    const controller = createThemeController(null, root);
    let rejectSave;
    const userData = selectorUserData(() => new Promise((_, reject) => { rejectSave = reject; }));
    renderThemeSelector(target, controller, selectorBackgroundController(), userData);

    const fieldset = target.children[0];
    const choices = target.querySelectorAll('input[type="radio"]');
    const current = choices.find((input) => input.name === "console-theme" && input.checked);
    const next = choices.find((input) => input.name === "console-theme" && !input.checked);
    assert.ok(current);
    assert.ok(next);
    next.checked = true;
    next.listeners.get("change")[0]();
    assert.equal(fieldset.disabled, true);
    rejectSave(new Error("服务端拒绝"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(current.checked, true);
    assert.equal(next.checked, false);
    assert.equal(fieldset.disabled, false);
    assert.equal(target.querySelectorAll("p").some((paragraph) => paragraph.textContent.includes("主题保存失败：服务端拒绝")), true);
  } finally {
    clearDomGlobals();
  }
});

test("自定义背景上传和删除失败都有界面错误并恢复控件", async () => {
  const fake = createFakeDom();
  installDomGlobals(fake);
  try {
    const file = { fileId: "background-1", filename: "old.png", url: "/old.png" };
    const target = document.createElement("div");
    const root = { dataset: {}, style: { setProperty: () => {}, removeProperty: () => {} } };
    const controller = createThemeController(null, root);
    const background = selectorBackgroundController();
    const userData = selectorUserData(
      async () => undefined,
      [file],
    );
    userData.uploadFile = async () => { throw new Error("上传不可用"); };
    userData.deleteFile = async () => { throw new Error("删除不可用"); };
    renderThemeSelector(target, controller, background, userData);

    const fieldset = target.children[0];
    const input = target.querySelector('input[type="file"]');
    input.files = [new File(["image"], "new.png", { type: "image/png" })];
    input.listeners.get("change")[0]();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(target.querySelectorAll("p").some((paragraph) => paragraph.textContent.includes("背景文件上传失败：上传不可用")), true);
    assert.equal(input.value, "");
    assert.equal(fieldset.disabled, false);

    const remove = target.querySelectorAll("button").find((button) => button.textContent === "删除");
    remove.listeners.get("click")[0]();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(target.querySelectorAll("p").some((paragraph) => paragraph.textContent.includes("背景文件删除失败：删除不可用")), true);
    assert.equal(fieldset.disabled, false);
  } finally {
    clearDomGlobals();
  }
});
