import { describe, expect, it } from "vitest";
import { discoveryLabel, filterModels, mergeModels } from "../src/features/configuration/provider-model-data.js";

describe("模型 discovery 与本地 metadata 合并", () => {
  const metadata = {
    revision: "models-r1",
    provider: "openai",
    models: [
      {
        id: "known",
        display_name: "本地显示名称",
        enabled: false,
        status: "beta",
        provenance: { display_name: "local_override", context_window: "catalog" },
      },
      { id: "catalog-only", display_name: "参考条目", enabled: true, status: "deprecated", provenance: { display_name: "catalog" } },
    ],
    overrides: [{ provider: "openai", id: "known", display_name: "本地显示名称", enabled: false }],
  };

  it("发现模型优先，未知私有模型保留，只有精确 ID 补充目录和字段来源", () => {
    const rows = mergeModels({ state: "success", models: [{ id: "private/unknown" }, { id: "known" }] }, metadata);
    expect(rows.map((row) => row.id)).toEqual(["known", "private/unknown", "catalog-only"]);
    expect(rows[0]?.sources).toEqual(["connection discovery", "catalog", "local override"]);
    expect(rows[1]?.metadata).toEqual({});
  });

  it("unsupported、failed、unknown 与 success 空列表不混淆", () => {
    const states = ["unsupported", "failed", "unknown", "success"];
    const labels = states.map((state) => discoveryLabel({ state, models: [], category: "test" }));
    expect(new Set(labels).size).toBe(4);
    expect(labels[3]).toMatch(/获取成功.*空列表/);
    for (const label of labels.slice(0, 3)) expect(label).not.toMatch(/暂无模型|空列表/);
    expect(discoveryLabel(null)).toMatch(/尚未获取/);
    // failed 的 models 可能是历史残留，不得并入展示
    expect(mergeModels({ state: "failed", models: [{ id: "stale" }] }, {})).toEqual([]);
  });

  it("filterModels 支持搜索与状态过滤", () => {
    const rows = mergeModels({ state: "success", models: [{ id: "private/unknown" }, { id: "known" }] }, metadata);
    expect(filterModels(rows, "本地显示", "disabled")[0]?.id).toBe("known");
    expect(filterModels(rows, "PRIVATE/", "enabled")[0]?.id).toBe("private/unknown");
    expect(filterModels(rows, "", "deprecated")[0]?.id).toBe("catalog-only");
    expect(filterModels(rows, "", "beta")[0]?.id).toBe("known");
    expect(filterModels(rows, "不存在", "all")).toEqual([]);
  });
});
