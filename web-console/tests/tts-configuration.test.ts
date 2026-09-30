import { describe, expect, it } from "vitest";
import { parseTtsNumberValue, ttsNumberRange, ttsProviderOptions } from "../src/features/configuration/tts-config.js";
import { parseConfigInputValue } from "../src/features/configuration/configuration-values.js";
import type { ConfigFieldSnapshot } from "../src/types.js";

function ttsField(key: string, savedValue: unknown): ConfigFieldSnapshot {
  return {
    key,
    module: "gateway.tts",
    valueType: typeof savedValue === "number" ? "integer" : "string",
    source: "managed_toml",
    overridden: false,
    editable: true,
    configured: savedValue !== null,
    valid: true,
    revision: "revision-1",
    sensitivity: "public",
    applyMode: "restart",
    savedValue,
    effectiveValue: savedValue,
    runningValue: savedValue,
    pendingRestart: false,
  };
}

describe("TTS Provider 下拉", () => {
  it("映射关闭与千问", () => {
    expect(ttsProviderOptions("qwen")).toEqual([
      ["disabled", "关闭"],
      ["qwen", "千问"],
    ]);
  });

  it("保留未知历史值，避免保存时静默改写", () => {
    const options = ttsProviderOptions("legacy-provider");
    expect(options.at(-1)).toEqual(["legacy-provider", "legacy-provider（当前自定义值）"]);
  });
});

describe("TTS 数字范围", () => {
  it("数字字段使用后端约束对应的浏览器范围", () => {
    expect(ttsNumberRange("delivery.tts.request_timeout_seconds")).toEqual([1, 120]);
    expect(ttsNumberRange("delivery.tts.max_text_chars")).toEqual([1, 600]);
    expect(ttsNumberRange("delivery.tts.qwen_model")).toBeNull();
  });

  it("拒绝空值、越界值和小数", () => {
    for (const [key, invalidValues] of [
      ["delivery.tts.request_timeout_seconds", ["", "0", "121", "1.5"]],
      ["delivery.tts.max_text_chars", ["", "0", "601", "1.5"]],
    ] as const) {
      for (const value of invalidValues) {
        expect(() => parseTtsNumberValue(key, value)).toThrow(/必须是 \d+ 到 \d+ 之间的整数/);
      }
    }
  });

  it("允许合法边界值", () => {
    expect(parseTtsNumberValue("delivery.tts.request_timeout_seconds", "1")).toBe(1);
    expect(parseTtsNumberValue("delivery.tts.request_timeout_seconds", "120")).toBe(120);
    expect(parseTtsNumberValue("delivery.tts.max_text_chars", "1")).toBe(1);
    expect(parseTtsNumberValue("delivery.tts.max_text_chars", "600")).toBe(600);
  });

  it("通用配置保存走同一范围校验", () => {
    const field = ttsField("delivery.tts.max_text_chars", 600);
    expect(parseConfigInputValue(field, "300")).toBe(300);
    expect(() => parseConfigInputValue(field, "601")).toThrow(/600/);
  });
});
