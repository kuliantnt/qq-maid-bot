import assert from "node:assert/strict";
import test from "node:test";

import { agentToolOptions, selectedAgentToolNames } from "../dist/agent-tools.js";

const registeredTools = [{ name: "web_search", description: "受控搜索" }];

function checkboxInputs(options) {
  return options.map((option) => ({ checked: option.checked, value: option.name }));
}

test("未注册工具可取消并从保存值删除", () => {
  const options = agentToolOptions(registeredTools, ["web_search", "legacy_tool"], true);
  const legacy = options.find((tool) => tool.name === "legacy_tool");

  assert.deepEqual(legacy, {
    name: "legacy_tool",
    description: "已写入 agent.toml，但当前进程未注册此工具",
    registered: false,
    checked: true,
    disabled: false,
  });
  legacy.checked = false;
  assert.deepEqual(selectedAgentToolNames(checkboxInputs(options)), ["web_search"]);
});

test("未操作未注册工具时保存其他配置仍保留它", () => {
  const options = agentToolOptions(registeredTools, ["legacy_tool"], true);

  assert.deepEqual(selectedAgentToolNames(checkboxInputs(options)), ["legacy_tool"]);
  assert.deepEqual(options.map((tool) => tool.name), ["image_generation", "web_search", "legacy_tool"]);
  assert.deepEqual(agentToolOptions(registeredTools, [], true).map((tool) => tool.name), ["image_generation", "web_search"]);
});

test("图片生成作为场景独立开关显示且只保存当前选择", () => {
  const privateOptions = agentToolOptions(registeredTools, ["image_generation"], true);
  const groupOptions = agentToolOptions(registeredTools, [], true);

  assert.equal(privateOptions.find((tool) => tool.name === "image_generation")?.checked, true);
  assert.equal(groupOptions.find((tool) => tool.name === "image_generation")?.checked, false);
  assert.deepEqual(selectedAgentToolNames(checkboxInputs(privateOptions)), ["image_generation"]);
  assert.deepEqual(selectedAgentToolNames(checkboxInputs(groupOptions)), []);
});
