import test from "node:test";
import assert from "node:assert/strict";
import { renderProviders, providerChange } from "../dist/views/configuration/providers.js";
import {
  captureConfigurationInputState,
  resetConfigurationStateForTests,
  setCurrent,
  setCurrentBackgroundController,
  setCurrentThemeController,
} from "../dist/views/configuration/state.js";
import { createFakeDom, installDomGlobals, clearDomGlobals, jsonResponse } from "./helpers/fake-dom.mjs";

test("通用预设来自服务端，非预设连接可编辑且 ID 不可修改", () => {
  installDomGlobals(createFakeDom());
  try {
    const provider = { enabled: true, kind: "openai_compatible", base_url: "https://local.example/v1", api_key_env: "LEGACY_KEY", auth_header: "X-Key", auth_scheme: null };
    const snapshot = {
      fields: [], agent: { editable: true, revision: "a", savedValue: { providers: { custom: provider } }, runningValue: { providers: {} } },
      providers: {
        adapters: ["openai_compatible", "openai_responses"],
        presets: [{ id: "trusted_template", name: "新增服务端模板", kind: "openai_compatible", base_url: "https://preset.example/v1", auth_header: "Authorization", auth_scheme: "Bearer" }],
        credentials: { custom: { configured: true, editable: true, revision: "secret-a", pending_restart: true } },
      },
    };
    const section = renderProviders(snapshot);
    assert.ok(section.querySelectorAll("option").some(option => option.textContent === "新增服务端模板"));
    const cards = section.querySelectorAll("article");
    const custom = cards.find(card => card.querySelector("h4")?.textContent === "custom");
    assert.ok(custom);
    assert.equal(custom.querySelectorAll("input").find(input => input.value === "custom").disabled, true);
    assert.equal(custom.querySelectorAll("input").find(input => input.value === provider.base_url).disabled, false);
    assert.ok(custom.querySelectorAll("button").some(button => button.textContent === "删除供应商"));
    const dialog = section.querySelector("dialog");
    assert.ok(dialog);
    let opened = false;
    dialog.showModal = () => { opened = true; };
    section.querySelectorAll("button").find(button => button.textContent === "+ 新建供应商").onclick();
    assert.equal(opened, true);
    const key = custom.querySelector('input[type="password"]');
    assert.equal(key.value, "");
    key.value = "test-only-sensitive-value";
    key.id = "test-transient-key";
    section.id = "configuration";
    assert.equal(captureConfigurationInputState().has("id:test-transient-key"), false);
  } finally { clearDomGlobals(); }
});

test("创建表单沿用 Provider 协议，不猜测模型品牌或提交凭证 Namespace", () => {
  const value = { enabled: true, kind: "openai_responses", base_url: "https://custom.example/v1", auth_header: "Authorization", auth_scheme: "Bearer", chat_fallback: false };
  assert.deepEqual(providerChange("custom_router", value), { action: "set_provider", id: "custom_router", provider: value });
  assert.deepEqual(providerChange("MyProxy", value, true), { action: "set_provider", id: "MyProxy", provider: value });
  for (const id of ["BadID", "unsafe/path", "", "a".repeat(65)]) assert.throws(() => providerChange(id, value));
  for (const id of ["unsafe/path", "", "a".repeat(65)]) assert.throws(() => providerChange(id, value, true));
  assert.equal("api_key_env" in value, false);
});

test("历史 MyProxy 卡片按原始 ID 保存且 canonical 冲突被拒绝", async () => {
  installDomGlobals(createFakeDom());
  resetConfigurationStateForTests();
  const provider = { display_name: null, enabled: true, kind: "openai_responses", base_url: "https://legacy.example/v1", api_key_env: "TEST_PROXY_KEY", auth_header: "Authorization", auth_scheme: "Bearer" };
  const snapshot = {
    revision: "runtime",
    fields: [],
    agent: { editable: true, revision: "agent-a", savedValue: { providers: { MyProxy: provider } }, runningValue: { providers: { MyProxy: provider } } },
    providers: {
      adapters: ["openai_compatible", "openai_responses"],
      presets: [],
      credentials: { MyProxy: { configured: true, editable: false, revision: "missing", pending_restart: false } },
    },
  };
  setCurrent(snapshot);
  setCurrentThemeController({});
  setCurrentBackgroundController({});
  document.registerStaticId("configuration-result");
  document.registerStaticId("console-toast");
  document.registerStaticId("configuration");
  document.registerStaticId("save-public-config", "button");
  document.registerStaticId("save-secret-config", "button");
  document.registerStaticId("save-agent-config", "button");
  document.registerStaticId("validate-config", "button");
  let request = null;
  globalThis.fetch = async (_input, init) => {
    request = JSON.parse(init.body);
    return jsonResponse({ data: snapshot });
  };
  try {
    const section = renderProviders(snapshot);
    section.id = "configuration";
    const card = section.querySelectorAll("article").find(card => card.querySelector("h4")?.textContent === "MyProxy");
    assert.ok(card);
    assert.equal(card.querySelector("input").value, "MyProxy");
    const save = card.querySelectorAll("button").find(button => button.textContent === "保存连接");
    assert.ok(save);
    await save.onclick();
    assert.deepEqual(request?.changes, [{
      action: "set_provider",
      id: "MyProxy",
      provider: {
        display_name: "MyProxy",
        enabled: true,
        kind: "openai_responses",
        base_url: "https://legacy.example/v1",
        chat_fallback: false,
        auth_header: "Authorization",
        auth_scheme: "Bearer",
        request_timeout_seconds: null,
      },
    }]);
  } finally {
    clearDomGlobals();
  }
});
