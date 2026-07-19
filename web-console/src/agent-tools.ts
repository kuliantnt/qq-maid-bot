import type { RegisteredTool } from "./types.js";

export interface AgentToolOption extends RegisteredTool {
  registered: boolean;
  checked: boolean;
  disabled: boolean;
}

/** 未注册项只能来自已保存白名单，避免通过普通工具列表构造任意名称。 */
export function agentToolOptions(
  registeredTools: RegisteredTool[],
  savedNames: string[],
  editable: boolean,
): AgentToolOption[] {
  const selected = new Set(savedNames);
  const registeredNames = new Set(registeredTools.map((tool) => tool.name));
  return [
    ...registeredTools.map((tool) => ({
      ...tool,
      registered: true,
      checked: selected.has(tool.name),
      disabled: !editable,
    })),
    ...savedNames
      .filter((name) => !registeredNames.has(name))
      .map((name) => ({
        name,
        description: "已写入 agent.toml，但当前进程未注册此工具",
        registered: false,
        checked: true,
        disabled: !editable,
      })),
  ];
}

export function selectedAgentToolNames(
  inputs: Iterable<Pick<HTMLInputElement, "checked" | "value">>,
): string[] {
  return [...inputs].filter((input) => input.checked).map((input) => input.value);
}
