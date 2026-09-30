import { describe, expect, it } from "vitest";
import {
  addCandidate,
  isMalformedCandidate,
  moveCandidate,
  normalizeCandidates,
  removeCandidate,
} from "../src/features/configuration/model-route-editor.js";

describe("模型路线候选编辑", () => {
  it("添加候选追加到末尾且去重", () => {
    const first = addCandidate(["opencode_zen:a"], "opencode_go:b");
    expect(first.list).toEqual(["opencode_zen:a", "opencode_go:b"]);
    expect(first.error).toBeNull();
    const second = addCandidate(first.list, "opencode_go:b");
    expect(second.error).toBe("该模型已在路线中");
    expect(second.list).toEqual(first.list);
  });

  it("空值与非法格式被拒绝", () => {
    expect(addCandidate([], "  ").error).toBe("模型不能为空");
    expect(addCandidate([], "no-colon").error).toBe("格式应为 provider:model");
    expect(addCandidate([], ":missing-provider").error).toBe("格式应为 provider:model");
    expect(addCandidate([], "provider:").error).toBe("格式应为 provider:model");
    expect(isMalformedCandidate("")).toBe(true);
    expect(isMalformedCandidate("opencode_zen:deepseek-v4-flash")).toBe(false);
  });

  it("删除只移除指定候选", () => {
    const list = ["a:1", "b:2", "c:3"];
    expect(removeCandidate(list, 1)).toEqual(["a:1", "c:3"]);
    expect(removeCandidate(list, -1)).toEqual(list);
    expect(removeCandidate(list, 99)).toEqual(list);
  });

  it("移动候选保持其余顺序且越界收敛", () => {
    const list = ["a:1", "b:2", "c:3", "d:4"];
    expect(moveCandidate(list, 0, 2)).toEqual(["b:2", "c:3", "a:1", "d:4"]);
    expect(moveCandidate(list, 3, 0)).toEqual(["d:4", "a:1", "b:2", "c:3"]);
    expect(moveCandidate(list, 0, -5)).toEqual(list);
    expect(moveCandidate(list, 1, 99)).toEqual(["a:1", "c:3", "d:4", "b:2"]);
    expect(moveCandidate(list, 0, 0)).toEqual(list);
  });

  it("归一化去除空白与重复并保持顺序", () => {
    expect(normalizeCandidates([" a:1 ", "", "b:2", "a:1", "  c:3  ", "b:2"])).toEqual(["a:1", "b:2", "c:3"]);
  });
});
