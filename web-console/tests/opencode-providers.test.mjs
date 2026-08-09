import test from "node:test";
import assert from "node:assert/strict";

import {
  OPEN_CODE_ROUTE_TEMPLATE_NOTICE,
  openCodeProviderChange,
  openCodeProviderPresets,
  openCodeProviderWarning,
  readOpenCodeProviders,
  readPreservedCustomProviders,
} from "../dist/views/configuration/opencode-providers.js";

test("OpenCode 三个预设使用固定 ID、协议、官方 Base URL 和共享 Key", () => {
  const presets = openCodeProviderPresets();
  assert.deepEqual(presets.map((provider) => provider.id), [
    "opencode_zen",
    "opencode_zen_chat",
    "opencode_go",
  ]);
  assert.deepEqual(presets.map((provider) => provider.kind), [
    "openai_responses",
    "openai_compatible",
    "openai_compatible",
  ]);
  assert.deepEqual(presets.map((provider) => provider.baseUrl), [
    "https://opencode.ai/zen/v1",
    "https://opencode.ai/zen/v1",
    "https://opencode.ai/zen/go/v1",
  ]);
  assert.ok(presets.every((provider) => provider.apiKeyEnv === "OPENCODE_API_KEY"));
  assert.ok(presets.every((provider) => provider.authHeader === "Authorization"));
  assert.ok(presets.every((provider) => provider.authScheme === "Bearer"));
});

test("页面从 agent.toml 已保存值恢复 Provider 表单", () => {
  const providers = readOpenCodeProviders({
    providers: {
      opencode_zen: {
        kind: "openai_compatible",
        base_url: "https://proxy.example/v1",
        api_key_env: "OPENCODE_API_KEY",
        auth_header: "X-API-Key",
        auth_scheme: null,
        request_timeout_seconds: 12,
        chat_fallback: false,
      },
    },
  });
  assert.equal(providers[0].enabled, true);
  assert.equal(providers[0].kind, "openai_responses");
  assert.equal(providers[0].baseUrl, "https://proxy.example/v1");
  assert.equal(providers[0].apiKeyEnv, "OPENCODE_API_KEY");
  assert.equal(providers[0].authHeader, "Authorization");
  assert.equal(providers[0].authScheme, "Bearer");
  assert.equal(providers[0].requestTimeoutSeconds, 12);
  assert.equal(providers[1].enabled, false);
});

test("非预设自定义 Provider 按原始 ID、类型和地址展示且不被预设表单替换", () => {
  const documentValue = {
    providers: {
      custom_future: {
        kind: "future_protocol",
        base_url: "https://future.example/v2",
        extension_field: "keep-me",
      },
      opencode_go: {
        kind: "openai_compatible",
        base_url: "https://opencode.ai/zen/go/v1",
      },
    },
  };
  assert.deepEqual(readPreservedCustomProviders(documentValue), [{
    id: "custom_future",
    kind: "future_protocol",
    baseUrl: "https://future.example/v2",
  }]);
  assert.equal(documentValue.providers.custom_future.extension_field, "keep-me");
});

test("Responses 保存操作显式关闭 Chat fallback 且不携带 Key 明文", () => {
  const form = {
    ...openCodeProviderPresets()[0],
    enabled: true,
    apiKeyEnv: "ATTACKER_KEY",
    authHeader: "X-API-Key",
    authScheme: "Basic",
  };
  const change = openCodeProviderChange(form);
  assert.equal(change.action, "set_provider");
  assert.equal(change.id, "opencode_zen");
  assert.equal(change.provider.chat_fallback, false);
  assert.equal(change.provider.api_key_env, "OPENCODE_API_KEY");
  assert.equal(change.provider.auth_header, "Authorization");
  assert.equal(change.provider.auth_scheme, "Bearer");
  assert.ok(!JSON.stringify(change).includes("api_key\""));
});

test("Chat 保存操作不发送 Responses 专属字段", () => {
  for (const form of openCodeProviderPresets().slice(1)) {
    const change = openCodeProviderChange({ ...form, enabled: true });
    assert.equal(change.provider.kind, "openai_compatible");
    assert.ok(!("chat_fallback" in change.provider));
  }
});

test("无效 Base URL 与超时在发请求前被拒绝", () => {
  const form = openCodeProviderPresets()[0];
  assert.throws(() => openCodeProviderChange({ ...form, baseUrl: "not-a-url" }), /Base URL/);
  assert.throws(() => openCodeProviderChange({ ...form, requestTimeoutSeconds: 0 }), /请求超时/);
});

test("Provider 与 Key 缺失仅显示预警，不阻止路线预编辑", () => {
  assert.match(openCodeProviderWarning(false, false), /仍可先编辑模型路线/);
  assert.match(openCodeProviderWarning(true, false), /API Key 尚未配置/);
  assert.equal(openCodeProviderWarning(true, true), "");
});

test("路线按钮明确插入模板且提示替换占位模型名", () => {
  assert.match(OPEN_CODE_ROUTE_TEMPLATE_NOTICE, /路线模板/);
  assert.match(OPEN_CODE_ROUTE_TEMPLATE_NOTICE, /必须.*占位模型名.*替换/);
  assert.doesNotMatch(OPEN_CODE_ROUTE_TEMPLATE_NOTICE, /可直接使用/);
});
