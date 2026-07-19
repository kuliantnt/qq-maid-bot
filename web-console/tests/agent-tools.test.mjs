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
  assert.deepEqual(options.map((tool) => tool.name), ["web_search", "legacy_tool"]);
  assert.deepEqual(agentToolOptions(registeredTools, [], true).map((tool) => tool.name), ["web_search"]);
});
