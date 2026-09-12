import { describe, expect, it } from "vitest";
import { configInputValue, configurationSummary, isEmptyInputValue, parseConfigInputValue } from "../src/features/configuration/configuration-values.js";
import { businessGroupOf, groupFieldsBySection } from "../src/features/configuration/configuration-navigation.js";
import type { ConfigFieldSnapshot, ConfigurationSnapshot } from "../src/types.js";

function field(overrides: Partial<ConfigFieldSnapshot>): ConfigFieldSnapshot {
  return {
    key: "command.prefix",
    module: "command",
    valueType: "string",
    source: "managed_toml",
    overridden: false,
    editable: true,
    configured: true,
    valid: true,
    revision: null,
    sensitivity: "public",
    applyMode: "immediate",
    savedValue: "!",
    effectiveValue: "!",
    runningValue: "!",
    pendingRestart: false,
    ...overrides,
  };
}

describe("配置输入投影", () => {
  it("baseline 为空时输入显示为空串；空输入仍视为未修改", () => {
    expect(configInputValue(field({ savedValue: null, effectiveValue: null }))).toBe("");
    expect(isEmptyInputValue("")).toBe(true);
  });

  it("string_list 显示为逗号分隔并按分隔符解析回数组", () => {
    const listField = field({ key: "console.allowed_origins", valueType: "string_list", savedValue: ["a", "b"] });
    expect(configInputValue(listField)).toBe("a, b");
    expect(parseConfigInputValue(listField, "a, b, c")).toEqual(["a", "b", "c"]);
  });

  it("integer 非整数值抛错", () => {
    const intField = field({ valueType: "integer" });
    expect(parseConfigInputValue(intField, "42")).toBe(42);
    expect(() => parseConfigInputValue(intField, "4.5")).toThrow();
  });
});

describe("配置分组", () => {
  it("按前缀归入业务域，未知字段进入高级兼容区", () => {
    expect(businessGroupOf("provider.openai.base_url")).toBe("models-providers");
    expect(businessGroupOf("platform.onebot11.bind_port")).toBe("platforms");
    expect(businessGroupOf("unknown.key")).toBe("advanced");
  });

  it("分节渲染保留 FIELD_SECTIONS 语义", () => {
    const grouped = groupFieldsBySection([
      field({ key: "platform.qq_official.app_id" }),
      field({ key: "platform.onebot11.bind_port" }),
    ]);
    expect(grouped).toHaveLength(2);
    expect(grouped[0]?.label).toBe("QQ 官方入口");
    expect(grouped[1]?.label).toBe("OneBot 11 入口");
  });
});

describe("配置摘要", () => {
  it("统计无效字段与待重启数量（含 agent）", () => {
    const snapshot = {
      fields: [
        field({ pendingRestart: true }),
        field({ valid: false }),
        field({}),
      ],
      agent: { pendingRestart: true },
    } as unknown as ConfigurationSnapshot;
    const summary = configurationSummary(snapshot);
    expect(summary.invalidCount).toBe(1);
    expect(summary.pendingCount).toBe(2);
  });
});
