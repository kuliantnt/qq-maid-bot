import assert from "node:assert/strict";
import test from "node:test";

import {
  addCandidate,
  isMalformedCandidate,
  moveCandidate,
  normalizeCandidates,
  removeCandidate,
} from "../dist/views/configuration/model-route-editor.js";

test("添加候选追加到末尾且去重", () => {
  const initial = ["opencode_zen:a"];
  const first = addCandidate(initial, "opencode_go:b");
  assert.deepEqual(first.list, ["opencode_zen:a", "opencode_go:b"]);
  assert.equal(first.error, null);
  const second = addCandidate(first.list, "opencode_go:b");
  assert.equal(second.error, "该模型已在路线中");
  assert.deepEqual(second.list, first.list);
});

test("空值与非法格式被拒绝", () => {
  assert.equal(addCandidate([], "  ").error, "模型不能为空");
  assert.equal(addCandidate([], "no-colon").error, "格式应为 provider:model");
  assert.equal(addCandidate([], ":missing-provider").error, "格式应为 provider:model");
  assert.equal(addCandidate([], "provider:").error, "格式应为 provider:model");
  assert.equal(isMalformedCandidate(""), true);
  assert.equal(isMalformedCandidate("opencode_zen:deepseek-v4-flash"), false);
});

test("删除只移除指定候选", () => {
  const list = ["a:1", "b:2", "c:3"];
  assert.deepEqual(removeCandidate(list, 1), ["a:1", "c:3"]);
  assert.deepEqual(removeCandidate(list, -1), list);
  assert.deepEqual(removeCandidate(list, 99), list);
});

test("移动候选保持其余顺序且越界收敛", () => {
  const list = ["a:1", "b:2", "c:3", "d:4"];
  assert.deepEqual(moveCandidate(list, 0, 2), ["b:2", "c:3", "a:1", "d:4"]);
  assert.deepEqual(moveCandidate(list, 3, 0), ["d:4", "a:1", "b:2", "c:3"]);
  assert.deepEqual(moveCandidate(list, 0, -5), list);
  assert.deepEqual(moveCandidate(list, 1, 99), ["a:1", "c:3", "d:4", "b:2"]);
  assert.deepEqual(moveCandidate(list, 0, 0), list);
});

test("归一化去除空白与重复并保持顺序", () => {
  assert.deepEqual(
    normalizeCandidates([" a:1 ", "", "b:2", "a:1", "  c:3  ", "b:2"]),
    ["a:1", "b:2", "c:3"],
  );
});
