import { describe, expect, it } from "vitest";
import {
  buildAgentChanges,
  buildSceneChange,
  createAgentDraft,
} from "../src/features/configuration/agent-draft.js";
import { agentToolOptions } from "../src/features/configuration/agent-tool-options.js";
import type { AgentConfigSnapshot, RegisteredTool } from "../src/types.js";

const SAVED_VALUE = {
  knowledge: {
    mode: "tool",
    embedding: { enabled: true, cache_dir: "cache/custom-embedding" },
  },
  model_routes: {
    private_main: { candidates: ["openai:gpt-5", "deepseek:deepseek-chat"] },
    group_main: { candidates: ["openai:gpt-5"] },
    aux: { candidates: [] },
  },
  tools: {
    web_search: {
      backend: "tavily",
      max_results: 8,
      search_depth: "advanced",
      topic: "news",
      time_range: "week",
      connect_timeout_seconds: 5,
      first_response_timeout_seconds: 15,
      total_timeout_seconds: 45,
      routes: {
        private_search: { model: "openai:gpt-search" },
        group_search: { model: "" },
      },
    },
  },
  scenes: {
    private: { tool_calling_enabled: true, enabled_tools: ["todo_write", "legacy_tool"] },
    group: { tool_calling_enabled: false, enabled_tools: [] },
  },
};

function agentSnapshot(overrides: Partial<AgentConfigSnapshot> = {}): AgentConfigSnapshot {
  return {
    revision: "rev-1",
    fileExists: true,
    source: "agent_toml",
    editable: true,
    readOnly: false,
    pendingRestart: false,
    savedValue: SAVED_VALUE,
    runningValue: {},
    ...overrides,
  };
}

const registeredTools: RegisteredTool[] = [
  { name: "todo_write", description: "写 Todo" },
  { name: "memory_save", description: "写记忆" },
  { name: "image_generation", description: "内置图片生成" },
];

describe("Agent 草稿构造", () => {
  it("从 savedValue 投影全部编辑区", () => {
    const draft = createAgentDraft(agentSnapshot());
    expect(draft.knowledgeMode).toBe("tool");
    expect(draft.knowledgeEmbeddingEnabled).toBe(true);
    expect(draft.webSearch.backend).toBe("tavily");
    expect(draft.webSearch.maxResults).toBe(8);
    expect(draft.routeCandidates.private_main).toEqual(["openai:gpt-5", "deepseek:deepseek-chat"]);
    expect(draft.routeCandidates.aux).toEqual([]);
    expect(draft.searchRoutes.private_search).toBe("openai:gpt-search");
    // 后端保存值里空 model 的路线在草稿中表示为空输入。
    expect(draft.searchRoutes.group_search).toBe("");
    expect(draft.scenes.private).toEqual({ toolCallingEnabled: true, enabledTools: ["todo_write", "legacy_tool"] });
    expect(draft.scenes.group.toolCallingEnabled).toBe(false);
  });

  it("缺失字段回落默认值", () => {
    const draft = createAgentDraft(agentSnapshot({ savedValue: {} }));
    expect(draft.knowledgeMode).toBe("preflight");
    expect(draft.knowledgeEmbeddingEnabled).toBe(false);
    expect(draft.webSearch.backend).toBe("provider_native");
    expect(draft.routeCandidates.private_main).toEqual([]);
  });
});

describe("Agent 保存契约", () => {
  it("完整保存按旧版顺序提交全部操作", () => {
    const draft = createAgentDraft(agentSnapshot());
    const changes = buildAgentChanges(draft, agentSnapshot());
    const actions = changes.map((change) => (change as { action: string }).action);
    expect(actions).toEqual([
      "set_knowledge",
      "set_web_search",
      "set_model_route",
      "set_model_route",
      "set_model_route",
      "set_scene",
      "set_scene",
    ]);
    expect(changes[0]).toEqual({
      action: "set_knowledge",
      mode: "tool",
      embedding: { enabled: true, cache_dir: "cache/custom-embedding" },
    });
    expect(changes[1]).toMatchObject({ action: "set_web_search", backend: "tavily", max_results: 8 });
    expect(changes[2]).toEqual({ action: "set_model_route", name: "private_main", candidates: ["openai:gpt-5", "deepseek:deepseek-chat"] });
    // 未修改且已保存的搜索路线不生成操作；空 model 的留空输入同样保留原内容。
    expect(changes.filter((change) => (change as { action: string }).action === "set_search_route")).toEqual([]);
    const scenes = changes.filter((change) => (change as { action: string }).action === "set_scene") as Array<{
      scene: string;
      config: Record<string, unknown>;
    }>;
    expect(scenes.map((change) => change.scene)).toEqual(["private", "group"]);
    expect(scenes[0]?.config.enabled_tools).toEqual(["todo_write", "legacy_tool"]);
  });

  it("cache_dir 缺失时回落默认路径", () => {
    const agent = agentSnapshot({
      savedValue: { knowledge: { mode: "auto", embedding: { enabled: false } } },
    });
    const draft = createAgentDraft(agent);
    const changes = buildAgentChanges(draft, agent);
    expect(changes[0]).toMatchObject({
      action: "set_knowledge",
      mode: "auto",
      embedding: { enabled: false, cache_dir: "cache/knowledge-embedding" },
    });
  });

  it("搜索路线只在真实修改时生成 set_search_route", () => {
    const agent = agentSnapshot();
    const draft = createAgentDraft(agent);
    draft.searchRoutes.private_search = "deepseek:deepseek-search";
    draft.searchRoutes.group_search = "openai:gpt-search-new";
    const changes = buildAgentChanges(draft, agent);
    expect(changes.filter((change) => (change as { action: string }).action === "set_search_route")).toEqual([
      { action: "set_search_route", name: "private_search", model: "deepseek:deepseek-search" },
      { action: "set_search_route", name: "group_search", model: "openai:gpt-search-new" },
    ]);
  });

  it("联网搜索参数不合法时完整保存被阻塞", () => {
    const agent = agentSnapshot();
    const draft = createAgentDraft(agent);
    draft.webSearch.maxResults = 0;
    expect(() => buildAgentChanges(draft, agent)).toThrow(/1 到 10/);
  });

  it("单场景保存保留该场景其余字段", () => {
    const agent = agentSnapshot();
    const draft = createAgentDraft(agent);
    draft.scenes.private.enabledTools = ["todo_write"];
    const change = buildSceneChange(draft, agent, "private") as {
      action: string;
      scene: string;
      config: Record<string, unknown>;
    };
    expect(change.action).toBe("set_scene");
    expect(change.scene).toBe("private");
    expect(change.config).toMatchObject({ tool_calling_enabled: true, enabled_tools: ["todo_write"] });
    // 群聊场景未修改，不受私聊草稿影响。
    const groupChange = buildSceneChange(draft, agent, "group") as { config: Record<string, unknown> };
    expect(groupChange.config.tool_calling_enabled).toBe(false);
  });
});

describe("工具白名单选项", () => {
  it("注册工具与已保存未注册工具合并，image_generation 不重复", () => {
    const options = agentToolOptions(registeredTools, ["todo_write", "legacy_tool"], true);
    expect(options.map((option) => option.name)).toEqual([
      "image_generation",
      "todo_write",
      "memory_save",
      "legacy_tool",
    ]);
    expect(options.find((option) => option.name === "legacy_tool")?.registered).toBe(false);
    expect(options.find((option) => option.name === "legacy_tool")?.checked).toBe(true);
    expect(options.find((option) => option.name === "memory_save")?.checked).toBe(false);
  });

  it("不可编辑时全部选项禁用", () => {
    const options = agentToolOptions(registeredTools, [], false);
    expect(options.every((option) => option.disabled)).toBe(true);
  });
});
