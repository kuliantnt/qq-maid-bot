import { describe, expect, it } from "vitest";
import {
  buildModelOverride,
  draftFromOverride,
  emptyModelOverrideDraft,
} from "../src/features/configuration/model-override-form.js";

describe("本地模型 override 表单", () => {
  it("编辑字段完整回填并读回，保留启停和声明", () => {
    const draft = draftFromOverride("vendor/model:free", {
      display_name: "显示名",
      enabled: false,
      context_window: 128000,
      max_output_tokens: 4096,
      status: "beta",
      price: { input_per_million_usd: 0, output_per_million_usd: 2 },
      modalities: { input: ["text", "image"], output: ["text"] },
      capabilities: { reasoning: { advertised: true }, tool_calling: { advertised: false } },
    });
    expect(buildModelOverride(draft)).toEqual({
      id: "vendor/model:free",
      display_name: "显示名",
      enabled: false,
      context_window: 128000,
      max_output_tokens: 4096,
      status: "beta",
      price: { input_per_million_usd: 0, output_per_million_usd: 2 },
      modalities: { input: ["text", "image"], output: ["text"] },
      capabilities: { reasoning: { advertised: true }, tool_calling: { advertised: false } },
    });
  });

  it("继承不复制目录默认值：空草稿只产出模型 ID", () => {
    expect(buildModelOverride(draftFromOverride("new-model", {}))).toEqual({ id: "new-model" });
    expect(() => buildModelOverride(emptyModelOverrideDraft())).toThrow("请填写模型 ID");
  });

  it("token 数必须是正整数，价格必须非负", () => {
    const draft = draftFromOverride("model", {});
    expect(() => buildModelOverride({ ...draft, context_window: "-1" })).toThrow("正整数");
    expect(() => buildModelOverride({ ...draft, context_window: "1.5" })).toThrow("正整数");
    expect(() => buildModelOverride({ ...draft, max_output_tokens: "0" })).toThrow("正整数");
    expect(() => buildModelOverride({ ...draft, inputPrice: "-2" })).toThrow("非负");
    expect(buildModelOverride({ ...draft, inputPrice: "0" }).price).toEqual({
      input_per_million_usd: 0,
      output_per_million_usd: null,
    });
  });

  it("模态取消继承时才产出 modalities；声明能力只包含显式选择", () => {
    const built = buildModelOverride({
      ...draftFromOverride("model", {}),
      outputPrice: "3",
      modalitiesInherit: false,
      inputModalities: ["text", "audio"],
      claims: { reasoning: "true", tool_calling: "", vision: "false" },
    });
    expect(built.modalities).toEqual({ input: ["text", "audio"], output: [] });
    expect(built.capabilities).toEqual({ reasoning: { advertised: true }, vision: { advertised: false } });
    expect(built.price).toEqual({ input_per_million_usd: null, output_per_million_usd: 3 });
    expect("modalities" in buildModelOverride(draftFromOverride("model", {}))).toBe(false);
  });
});
