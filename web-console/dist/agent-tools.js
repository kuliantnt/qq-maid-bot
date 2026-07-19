/** 未注册项只能来自已保存白名单，避免通过普通工具列表构造任意名称。 */
export function agentToolOptions(registeredTools, savedNames, editable) {
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
export function selectedAgentToolNames(inputs) {
    return [...inputs].filter((input) => input.checked).map((input) => input.value);
}
