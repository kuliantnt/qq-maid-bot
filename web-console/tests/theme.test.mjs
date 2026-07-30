import assert from "node:assert/strict";
import test from "node:test";

import {
  CONSOLE_THEME_STORAGE_KEY,
  DEFAULT_CONSOLE_THEME,
  createThemeController,
  parseStoredTheme,
  serializeTheme,
} from "../dist/theme.js";

test("缺失、损坏和未知主题回退到默认值且不改变存储格式", () => {
  const fallback = { preset: DEFAULT_CONSOLE_THEME, version: 1 };

  assert.deepEqual(parseStoredTheme(null), fallback);
  assert.deepEqual(parseStoredTheme("not-json"), fallback);
  assert.deepEqual(parseStoredTheme('{"preset":"unknown","version":1}'), fallback);
  assert.deepEqual(parseStoredTheme('{"preset":"night-shift","version":2}'), fallback);
});

test("有效主题按 version 1 的本地存储契约往返解析", () => {
  const preference = { preset: "ember-grid", version: 1 };

  assert.equal(serializeTheme(preference), '{"preset":"ember-grid","version":1}');
  assert.deepEqual(parseStoredTheme(serializeTheme(preference)), preference);
  assert.equal(CONSOLE_THEME_STORAGE_KEY, "console-theme");
});

test("主题控制器只更新 root 和 localStorage，恢复默认会删除 key", () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
  };
  const root = { dataset: {} };
  const controller = createThemeController(storage, root);

  controller.select("tide-signal");
  assert.equal(root.dataset.theme, "tide-signal");
  assert.equal(values.get(CONSOLE_THEME_STORAGE_KEY), '{"preset":"tide-signal","version":1}');

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
  controller.select("ember-grid");
  assert.equal(root.dataset.theme, "ember-grid");
  controller.reset();
  assert.equal(root.dataset.theme, DEFAULT_CONSOLE_THEME);
});
