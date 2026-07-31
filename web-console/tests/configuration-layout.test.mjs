import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  configurationBusinessGroup,
  configurationBusinessGroups,
  publicConfigurationChanges,
} from "../dist/views/configuration.js";

test("构建产物不再包含 Provider 连接测试面板或请求入口", async () => {
  const [html, api, configuration] = await Promise.all([
    readFile(new URL("../dist/index.html", import.meta.url), "utf8"),
    readFile(new URL("../dist/api.js", import.meta.url), "utf8"),
    readFile(new URL("../dist/views/configuration.js", import.meta.url), "utf8"),
  ]);
  for (const content of [html, api, configuration]) {
    assert.doesNotMatch(content, /Provider 连接测试|READ-ONLY TEST|test-provider-connection|configuration\/test-connection/);
  }
});

test("受管字段按稳定业务映射归类，未来字段进入高级兼容区", () => {
  assert.equal(configurationBusinessGroup("provider.openai.base_url"), "models-providers");
  assert.equal(configurationBusinessGroup("tools.web_search.tavily.api_key"), "online-tools");
  assert.equal(configurationBusinessGroup("features.memory.dream_enabled"), "memory-knowledge");
  assert.equal(configurationBusinessGroup("delivery.tts.provider"), "replies-voice");
  assert.equal(configurationBusinessGroup("platform.onebot11.enabled"), "platforms");
  assert.equal(configurationBusinessGroup("features.todo.daily_reminder_time"), "tasks-notifications");
  assert.equal(configurationBusinessGroup("console.allowed_origins"), "system-security");
  assert.equal(configurationBusinessGroup("future.history.option"), "advanced");

  assert.deepEqual(configurationBusinessGroups([
    "console.enabled",
    "provider.openai.base_url",
    "features.todo.daily_reminder_enabled",
    "future.history.option",
  ]), ["models-providers", "tasks-notifications", "system-security", "advanced"]);
});

test("普通字段保存只提交当前受管字段的实际变化，不携带未展示历史字段", () => {
  const fields = [{
    key: "features.rss.enabled",
    module: "core.rss",
    valueType: "boolean",
    source: "managed_toml",
    overridden: false,
    editable: true,
    configured: true,
    valid: true,
    revision: null,
    sensitivity: "public",
    applyMode: "restart",
    savedValue: true,
    effectiveValue: true,
    runningValue: true,
    pendingRestart: false,
  }];
  const changes = publicConfigurationChanges(fields, new Map([
    ["features.rss.enabled", false],
    ["history.unrendered.value", "must-not-enter-payload"],
  ]));
  assert.deepEqual(changes, [{ action: "set", key: "features.rss.enabled", value: false }]);
});
