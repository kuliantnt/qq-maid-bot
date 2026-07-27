import test from "node:test";
import assert from "node:assert/strict";

import {
  openCodeProviderChange,
  openCodeProviderPresets,
  openCodeProviderWarning,
  readOpenCodeProviders,
} from "../dist/opencode-providers.js";

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
  assert.equal(presets[0].chatFallback, false);
});

test("页面从 agent.toml 已保存值恢复 Provider 表单", () => {
  const providers = readOpenCodeProviders({
    providers: {
      opencode_zen: {
        kind: "openai_responses",
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
  assert.equal(providers[0].baseUrl, "https://proxy.example/v1");
  assert.equal(providers[0].authHeader, "X-API-Key");
  assert.equal(providers[0].authScheme, "");
  assert.equal(providers[0].requestTimeoutSeconds, 12);
  assert.equal(providers[1].enabled, false);
});

test("Responses 保存操作显式关闭 Chat fallback 且不携带 Key 明文", () => {
  const form = { ...openCodeProviderPresets()[0], enabled: true };
  const change = openCodeProviderChange(form);
  assert.equal(change.action, "set_provider");
  assert.equal(change.id, "opencode_zen");
  assert.equal(change.provider.chat_fallback, false);
  assert.equal(change.provider.api_key_env, "OPENCODE_API_KEY");
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
