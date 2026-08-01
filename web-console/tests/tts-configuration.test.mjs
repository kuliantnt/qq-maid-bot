import test from "node:test";
import assert from "node:assert/strict";

import {
  configFieldGroupLabel,
  configFieldLabel,
  parseTtsNumberValue,
  publicConfigurationChanges,
  secretConfigurationChanges,
  ttsNumberRange,
  ttsProviderOptions,
} from "../dist/views/configuration/configuration.js";

const TTS_KEYS = [
  "delivery.tts.provider",
  "delivery.tts.qwen_api_key",
  "delivery.tts.qwen_base_url",
  "delivery.tts.qwen_model",
  "delivery.tts.qwen_voice",
  "delivery.tts.request_timeout_seconds",
  "delivery.tts.max_text_chars",
];

function field(key, savedValue, sensitivity = "public") {
  return {
    key,
    module: "gateway.tts",
    valueType: typeof savedValue === "number" ? "integer" : "string",
    source: sensitivity === "secret" ? "encrypted_secret" : "managed_toml",
    overridden: false,
    editable: true,
    configured: savedValue !== null,
    valid: true,
    revision: "revision-1",
    sensitivity,
    applyMode: "restart",
    savedValue: sensitivity === "secret" ? null : savedValue,
    effectiveValue: sensitivity === "secret" ? null : savedValue,
    runningValue: sensitivity === "secret" ? null : savedValue,
    pendingRestart: false,
  };
}

test("全部 TTS 字段进入回复与语音业务分组并使用专用标签", () => {
  for (const key of TTS_KEYS) {
    assert.equal(configFieldGroupLabel(key), "回复与语音");
    assert.notEqual(configFieldLabel(key), key);
  }
});

test("TTS Provider 下拉框映射关闭与千问", () => {
  assert.deepEqual(ttsProviderOptions("qwen"), [
    ["disabled", "关闭"],
    ["qwen", "千问"],
  ]);
});

test("TTS Provider 下拉框保留未知历史值", () => {
  const options = ttsProviderOptions("legacy-provider");
  assert.deepEqual(options.at(-1), ["legacy-provider", "legacy-provider（当前自定义值）"]);
});

test("TTS 数字字段使用后端约束对应的浏览器范围", () => {
  assert.deepEqual(ttsNumberRange("delivery.tts.request_timeout_seconds"), [1, 120]);
  assert.deepEqual(ttsNumberRange("delivery.tts.max_text_chars"), [1, 600]);
  assert.equal(ttsNumberRange("delivery.tts.qwen_model"), null);
});

test("TTS 数字字段拒绝空值、越界值和小数", () => {
  for (const [key, invalidValues] of [
    ["delivery.tts.request_timeout_seconds", ["", "0", "121", "1.5"]],
    ["delivery.tts.max_text_chars", ["", "0", "601", "1.5"]],
  ]) {
    for (const value of invalidValues) {
      assert.throws(
        () => parseTtsNumberValue(key, value),
        /必须是 \d+ 到 \d+ 之间的整数/,
      );
    }
  }
});

test("TTS 数字字段允许合法边界值", () => {
  assert.equal(parseTtsNumberValue("delivery.tts.request_timeout_seconds", "1"), 1);
  assert.equal(parseTtsNumberValue("delivery.tts.request_timeout_seconds", "120"), 120);
  assert.equal(parseTtsNumberValue("delivery.tts.max_text_chars", "1"), 1);
  assert.equal(parseTtsNumberValue("delivery.tts.max_text_chars", "600"), 600);
});

test("切换 disabled 只保存 Provider，不清除或改写 Qwen 配置", () => {
  const fields = [
    field("delivery.tts.provider", "qwen"),
    field("delivery.tts.qwen_base_url", "https://example.test/tts"),
    field("delivery.tts.qwen_model", "qwen3-tts-flash"),
    field("delivery.tts.qwen_voice", "Cherry"),
    field("delivery.tts.request_timeout_seconds", 30),
    field("delivery.tts.max_text_chars", 600),
  ];
  const values = new Map(fields.map((item) => [item.key, item.savedValue]));
  values.set("delivery.tts.provider", "disabled");

  assert.deepEqual(publicConfigurationChanges(fields, values), [{
    action: "set",
    key: "delivery.tts.provider",
    value: "disabled",
  }]);
});

test("千问 API Key 继续使用 secret replace/clear，且留空不修改", () => {
  const secret = field("delivery.tts.qwen_api_key", null, "secret");
  const dirty = new Set([secret.key]);

  assert.deepEqual(secretConfigurationChanges([secret], new Map(), new Set(), new Set()), []);
  assert.deepEqual(
    secretConfigurationChanges([secret], new Map([[secret.key, "new-key"]]), new Set(), dirty),
    [{ action: "replace", key: secret.key, value: "new-key", expected_revision: "revision-1" }],
  );
  assert.deepEqual(
    secretConfigurationChanges([secret], new Map([[secret.key, "must-not-win"]]), new Set([secret.key]), dirty),
    [{ action: "clear", key: secret.key, expected_revision: "revision-1" }],
  );
  assert.deepEqual(
    publicConfigurationChanges([secret], new Map([[secret.key, "must-not-enter-runtime.toml"]])),
    [],
  );
});
