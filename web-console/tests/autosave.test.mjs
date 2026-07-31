import assert from "node:assert/strict";
import test from "node:test";

import {
  clearDomGlobals,
  createFakeDom,
  flushMicrotasks,
  installDomGlobals,
  jsonResponse,
  waitFor,
} from "./helpers/fake-dom.mjs";
import { createThemeController } from "../dist/theme.js";
import { createBackgroundController } from "../dist/background.js";
import { initializeConfiguration, resetConfigurationStateForTests } from "../dist/views/configuration.js";

function makeField({
  key,
  valueType = "string",
  sensitivity = "public",
  savedValue = null,
  revision = null,
}) {
  const configured = sensitivity === "secret" ? revision !== null : savedValue !== null;
  return {
    key,
    module: "runtime",
    value_type: valueType,
    source: sensitivity === "secret" ? "encrypted_secret" : "managed_toml",
    overridden: false,
    editable: true,
    configured,
    valid: true,
    revision,
    sensitivity,
    apply_mode: "restart",
    saved_value: sensitivity === "secret" ? null : savedValue,
    effective_value: sensitivity === "secret" ? null : savedValue,
    running_value: sensitivity === "secret" ? null : savedValue,
    pending_restart: false,
  };
}

function defaultFields() {
  return [
    makeField({ key: "delivery.tts.qwen_model", savedValue: "model-a" }),
    makeField({ key: "bootstrap.listen_host", savedValue: "0.0.0.0" }),
    makeField({
      key: "provider.openai.api_key",
      sensitivity: "secret",
      revision: "sec-1",
    }),
    makeField({
      key: "provider.deepseek.api_key",
      sensitivity: "secret",
      revision: "sec-1",
    }),
  ];
}

function setupEnvironment({ slowRuntime = false, slowSecrets = false } = {}) {
  const fake = createFakeDom();
  installDomGlobals(fake);
  resetConfigurationStateForTests();
  const { document } = fake;

  const configuration = document.registerStaticId("configuration", "section");
  const summary = document.registerStaticId("configuration-summary", "div");
  const panels = {
    runtime: document.registerStaticId("configuration-panel-runtime", "section"),
    secrets: document.registerStaticId("configuration-panel-secrets", "section"),
    agent: document.registerStaticId("configuration-panel-agent", "section"),
    interface: document.registerStaticId("configuration-panel-interface", "section"),
  };
  const publicFields = document.registerStaticId("public-config-fields", "div");
  const secretFields = document.registerStaticId("secret-config-fields", "div");
  const agentFields = document.registerStaticId("agent-config-fields", "div");
  const themeSelector = document.registerStaticId("console-theme-selector", "div");
  const primaryTabs = document.registerStaticId("configuration-primary-tabs", "div");
  const secondaryTabs = document.registerStaticId("configuration-secondary-tabs", "div");
  const result = document.registerStaticId("configuration-result", "p");
  const toast = document.registerStaticId("console-toast", "div");
  const savePublic = document.registerStaticId("save-public-config", "button");
  const saveSecret = document.registerStaticId("save-secret-config", "button");
  const saveAgent = document.registerStaticId("save-agent-config", "button");
  const restart = document.registerStaticId("restart-service", "button");
  const validate = document.registerStaticId("validate-config", "button");
  const testProvider = document.registerStaticId("test-provider-connection", "button");
  const connectionProvider = document.registerStaticId("connection-provider", "select");

  configuration.append(summary, panels.runtime, panels.secrets, panels.agent, panels.interface);
  configuration.append(result, toast, restart, validate, testProvider, connectionProvider);
  panels.runtime.append(publicFields, savePublic);
  panels.secrets.append(secretFields, saveSecret);
  panels.agent.append(agentFields, saveAgent);
  panels.interface.append(themeSelector);

  const state = {
    revision: "rev-1",
    fields: defaultFields(),
  };
  const requests = [];
  const savedCounter = { count: 0 };
  const slow = {
    promise: Promise.resolve(),
    resolve: () => undefined,
  };
  if (slowRuntime || slowSecrets) {
    let release;
    slow.promise = new Promise((resolve) => {
      release = resolve;
    });
    slow.resolve = release;
  }

  const configurationPayload = () => ({
    ok: true,
    persisted: true,
    configuration: {
      revision: state.revision,
      file_exists: true,
      agent: {},
      fields: state.fields,
    },
    registered_tools: [],
    restart: { available: false },
  });

  globalThis.fetch = async (input, init = {}) => {
    const url = String(input);
    const method = init.method ?? "GET";
    let body = null;
    if (init.body) {
      try {
        body = JSON.parse(init.body);
      } catch {
        body = null;
      }
    }
    requests.push({ url, method, body });
    if (url === "/api/v1/console/configuration" && method === "GET") {
      return jsonResponse(configurationPayload());
    }
    if (url.endsWith("/api/v1/console/configuration/runtime") && method === "PATCH") {
      await slow.promise;
      if (body.expected_revision !== state.revision) {
        return jsonResponse({
          ok: false,
          error: { code: "config_conflict", message: "revision conflict" },
        }, 409);
      }
      for (const change of body.changes) {
        const field = state.fields.find((entry) => entry.key === change.key);
        if (!field) continue;
        if (change.action === "remove") {
          field.saved_value = null;
          field.effective_value = null;
          field.configured = false;
        } else {
          field.saved_value = change.value;
          field.effective_value = change.value;
          field.configured = true;
        }
      }
      const next = Number(state.revision.split("-")[1]) + 1;
      state.revision = `rev-${next}`;
      savedCounter.count += 1;
      return jsonResponse(configurationPayload());
    }
    if (url.endsWith("/api/v1/console/configuration/secrets") && method === "PATCH") {
      await slow.promise;
      for (const change of body.changes) {
        const field = state.fields.find((entry) => entry.key === change.key);
        if (!field) continue;
        if (change.expected_revision !== field.revision) {
          return jsonResponse({
            ok: false,
            error: { code: "config_conflict", message: "secret revision conflict" },
          }, 409);
        }
        const next = Number(field.revision.split("-")[1]) + 1;
        field.revision = `sec-${next}`;
        field.configured = change.action !== "clear";
      }
      savedCounter.count += 1;
      return jsonResponse(configurationPayload());
    }
    throw new Error(`unexpected fetch: ${method} ${url}`);
  };

  const storage = {
    getItem: () => null,
    setItem: () => undefined,
    removeItem: () => undefined,
  };
  const themeRoot = {
    dataset: {},
    style: { setProperty: () => undefined, removeProperty: () => undefined },
  };
  const themeController = createThemeController(storage, themeRoot);
  const backgroundRoot = { dataset: {}, style: {}, querySelector: () => null };
  const backgroundController = createBackgroundController(backgroundRoot, null);
  const userData = {
    preferences: {
      customColors: [],
      backgroundFileIds: [],
      activeBackgroundFileId: null,
      backgroundMode: "default",
      kuliantnt: false,
    },
    files: [],
    updatePreferences: async (patch) => {
      userData.preferences = { ...userData.preferences, ...patch };
      return userData.preferences;
    },
  };

  return {
    document,
    state,
    requests,
    savedCounter,
    slow,
    themeController,
    backgroundController,
    userData,
    initialize: async () => {
      await initializeConfiguration(themeController, backgroundController, userData);
    },
    fireFocusOut: (target, relatedTarget = null) => {
      const handlers = fake.document.listeners.get("focusout") ?? [];
      for (const handler of handlers) {
        handler({ target, relatedTarget });
      }
    },
    dispose: () => clearDomGlobals(),
  };
}

function runtimePatches(requests) {
  return requests.filter((request) => request.method === "PATCH" && request.url.endsWith("/runtime"));
}

function secretPatches(requests) {
  return requests.filter((request) => request.method === "PATCH" && request.url.endsWith("/secrets"));
}

test("慢速保存字段 A 时，字段 B 的未保存输入在 A 保存完成后仍然存在并可继续保存", async () => {
  const env = setupEnvironment({ slowRuntime: true });
  try {
    await env.initialize();
    const inputA = env.document.getElementById("config-delivery-tts-qwen_model");
    const inputB = env.document.getElementById("config-bootstrap-listen_host");

    inputA.value = "new-model";
    env.fireFocusOut(inputA);
    await waitFor(() => runtimePatches(env.requests).length === 1, "A 的保存请求应已发出");

    // 保存未完成时修改字段 B
    inputB.value = "10.1.2.3";
    env.slow.resolve();
    await waitFor(() => env.savedCounter.count === 1, "A 的保存应完成");
    await flushMicrotasks();

    const rebuiltB = env.document.getElementById("config-bootstrap-listen_host");
    assert.equal(rebuiltB.value, "10.1.2.3", "B 的未保存输入不能被重建吞掉");

    // B 后续可以正常保存，且只提交 B 的变化
    rebuiltB.value = "10.1.2.4";
    env.fireFocusOut(rebuiltB);
    await waitFor(() => env.savedCounter.count === 2, "B 的保存应完成");
    await flushMicrotasks();
    const patches = runtimePatches(env.requests);
    assert.equal(patches.length, 2);
    assert.deepEqual(patches[1].body.changes, [
      { action: "set", key: "bootstrap.listen_host", value: "10.1.2.4" },
    ]);
  } finally {
    env.dispose();
  }
});

test("连续修改两个 Secret：不重复提交已保存的旧 revision，后一个 Secret 正常保存", async () => {
  const env = setupEnvironment();
  try {
    await env.initialize();
    let first = env.document.getElementById("config-provider-openai-api_key");
    let second = env.document.getElementById("config-provider-deepseek-api_key");

    first.value = "sk-first";
    env.fireFocusOut(first);
    await waitFor(() => env.savedCounter.count === 1, "第一个 Secret 保存应完成");
    await flushMicrotasks();
    const firstPatch = secretPatches(env.requests)[0];
    assert.deepEqual(firstPatch.body.changes, [{
      action: "replace",
      key: "provider.openai.api_key",
      value: "sk-first",
      expected_revision: "sec-1",
    }]);
    assert.equal(
      env.state.fields.find((field) => field.key === "provider.openai.api_key").revision,
      "sec-2",
    );

    first = env.document.getElementById("config-provider-openai-api_key");
    second = env.document.getElementById("config-provider-deepseek-api_key");
    second.value = "sk-second";
    env.fireFocusOut(second);
    await waitFor(() => env.savedCounter.count === 2, "第二个 Secret 保存应完成");
    await flushMicrotasks();
    const secondPatch = secretPatches(env.requests)[1];
    // 第二个保存只提交第二个 Secret，且使用其自己的 revision，不重复提交第一个。
    assert.deepEqual(secondPatch.body.changes, [{
      action: "replace",
      key: "provider.deepseek.api_key",
      value: "sk-second",
      expected_revision: "sec-1",
    }]);
    const firstSubmissions = secretPatches(env.requests)
      .filter((request) => request.body.changes.some((change) => change.key === "provider.openai.api_key"));
    assert.equal(firstSubmissions.length, 1, "第一个 Secret 不能被重复提交");
  } finally {
    env.dispose();
  }
});

test("Secret 并发保存：第一个请求未完成时填写并失焦第二个 Secret，两个请求都成功且第二个只提交自身", async () => {
  const env = setupEnvironment({ slowSecrets: true });
  try {
    await env.initialize();
    const first = env.document.getElementById("config-provider-openai-api_key");

    first.value = "sk-first";
    env.fireFocusOut(first);
    await waitFor(() => secretPatches(env.requests).length === 1, "第一个 Secret 请求应已发出");

    // 第一个请求完成前填写并失焦第二个 Secret：此时必须排队，不能立即用旧 revision 计算。
    const second = env.document.getElementById("config-provider-deepseek-api_key");
    second.value = "sk-second";
    env.fireFocusOut(second);

    env.slow.resolve();
    await waitFor(() => env.savedCounter.count === 2, "两个 Secret 保存都应完成");
    await flushMicrotasks();

    const patches = secretPatches(env.requests);
    assert.equal(patches.length, 2, "两个 Secret 各自只提交一次");
    assert.deepEqual(patches[0].body.changes, [{
      action: "replace",
      key: "provider.openai.api_key",
      value: "sk-first",
      expected_revision: "sec-1",
    }]);
    // 第二个请求只能包含第二个 Secret，且 expected_revision 来自第一个保存完成后的最新 snapshot。
    assert.deepEqual(patches[1].body.changes, [{
      action: "replace",
      key: "provider.deepseek.api_key",
      value: "sk-second",
      expected_revision: "sec-1",
    }]);
    // 两个 Secret 都成功保存（revision 前进），不出现 config_conflict。
    assert.equal(
      env.state.fields.find((field) => field.key === "provider.openai.api_key").revision,
      "sec-2",
    );
    assert.equal(
      env.state.fields.find((field) => field.key === "provider.deepseek.api_key").revision,
      "sec-2",
    );
    const firstSubmissions = secretPatches(env.requests)
      .filter((request) => request.body.changes.some((change) => change.key === "provider.openai.api_key"));
    assert.equal(firstSubmissions.length, 1, "第一个 Secret 不能被重复提交");
    const conflicts = secretPatches(env.requests)
      .filter((request) => request.body.changes.some((change) => change.key === "provider.deepseek.api_key"));
    assert.equal(conflicts.length, 1, "第二个 Secret 恰好提交一次且未被冲突拦截");
  } finally {
    env.dispose();
  }
});

test("重新初始化（重新登录/刷新）后清空跨会话 Secret 已保存状态，同值输入再次按新值保存", async () => {
  const env = setupEnvironment();
  try {
    await env.initialize();
    const first = env.document.getElementById("config-provider-openai-api_key");
    first.value = "sk-first";
    env.fireFocusOut(first);
    await waitFor(() => env.savedCounter.count === 1, "第一次 Secret 保存应完成");
    await flushMicrotasks();

    // 模拟重新登录/刷新：再次初始化会清空旧会话的 secretSavedStates。
    await env.initialize();
    const rebuilt = env.document.getElementById("config-provider-openai-api_key");
    rebuilt.value = "sk-first";
    env.fireFocusOut(rebuilt);
    await waitFor(() => env.savedCounter.count === 2, "重新初始化后同值输入应再次保存");
    await flushMicrotasks();

    assert.equal(secretPatches(env.requests).length, 2, "残留状态被清空后同值输入不能再被误判为未变更");
    assert.deepEqual(secretPatches(env.requests)[1].body.changes, [{
      action: "replace",
      key: "provider.openai.api_key",
      value: "sk-first",
      expected_revision: "sec-2",
    }]);
  } finally {
    env.dispose();
  }
});

test("blur 跳到保存按钮时延后自动保存，显式按钮只提交一次", async () => {
  const env = setupEnvironment();
  try {
    await env.initialize();
    const inputA = env.document.getElementById("config-delivery-tts-qwen_model");
    const savePublic = env.document.getElementById("save-public-config");

    inputA.value = "model-v2";
    env.fireFocusOut(inputA, savePublic);
    await flushMicrotasks();
    assert.equal(runtimePatches(env.requests).length, 0, "blur 到保存按钮不应触发自动保存");

    savePublic.onclick();
    await waitFor(() => env.savedCounter.count === 1, "显式保存应完成");
    await flushMicrotasks();
    assert.equal(runtimePatches(env.requests).length, 1);

    // 保存完成后按钮被重建；再次点击没有新变更，不重复提交。
    const rebuiltSave = env.document.getElementById("save-public-config");
    rebuiltSave.onclick();
    await flushMicrotasks();
    assert.equal(runtimePatches(env.requests).length, 1, "无变更时显式按钮不能重复提交");
  } finally {
    env.dispose();
  }
});

test("自动保存队列按 revision 顺序串行执行，慢保存后的新修改使用新 revision", async () => {
  const env = setupEnvironment({ slowRuntime: true });
  try {
    await env.initialize();
    let inputA = env.document.getElementById("config-delivery-tts-qwen_model");
    inputA.value = "v1";
    env.fireFocusOut(inputA);
    await waitFor(() => runtimePatches(env.requests).length === 1, "第一次保存应已发出");

    // 第一次保存尚未完成时再次修改同一字段并触发保存。
    inputA = env.document.getElementById("config-delivery-tts-qwen_model");
    inputA.value = "v2";
    env.fireFocusOut(inputA);
    env.slow.resolve();
    await waitFor(() => env.savedCounter.count === 2, "两次保存都应完成");
    await flushMicrotasks();

    const patches = runtimePatches(env.requests);
    assert.equal(patches.length, 2);
    assert.equal(patches[0].body.expected_revision, "rev-1");
    assert.equal(patches[1].body.expected_revision, "rev-2");
    assert.deepEqual(patches[1].body.changes, [
      { action: "set", key: "delivery.tts.qwen_model", value: "v2" },
    ]);
  } finally {
    env.dispose();
  }
});
