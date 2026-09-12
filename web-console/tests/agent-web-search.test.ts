import { describe, expect, it } from "vitest";
import {
  readAgentWebSearchConfig,
  tavilyCredentialNotice,
  webSearchBackendLabel,
  webSearchConfigChange,
  webSearchRouteChanges,
} from "../src/features/configuration/agent-web-search.js";

const baseConfig = {
  backend: "tavily" as const,
  maxResults: 8,
  searchDepth: "advanced" as const,
  topic: "news" as const,
  timeRange: "week" as const,
  connectTimeoutSeconds: 5,
  firstResponseTimeoutSeconds: 15,
  totalTimeoutSeconds: 45,
  routes: {
    private_search: "openai:gpt-search",
    group_search: "gemini:gemini-2.5-flash",
  },
};

describe("联网搜索配置读取", () => {
  it("只读取 tools.web_search 及其 routes，旧顶层 search_routes 被忽略", () => {
    const parsed = readAgentWebSearchConfig({
      search_routes: { private_search: { model: "legacy-must-be-ignored" } },
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
            group_search: { model: "gemini:gemini-2.5-flash" },
          },
        },
      },
    });

    expect(parsed).toEqual(baseConfig);
    expect(Object.values(parsed.routes)).not.toContain("legacy-must-be-ignored");
  });

  it("缺失或非法字段回落默认值", () => {
    const parsed = readAgentWebSearchConfig({});
    expect(parsed.backend).toBe("provider_native");
    expect(parsed.maxResults).toBe(5);
    expect(parsed.searchDepth).toBe("basic");
    expect(parsed.topic).toBe("general");
    expect(parsed.timeRange).toBeNull();
    expect(parsed.routes).toEqual({});
  });
});

describe("联网搜索保存契约", () => {
  it("为三种后端生成 set_web_search 且不携带 route 或 secret", () => {
    for (const backend of ["provider_native", "tavily", "disabled"] as const) {
      const change = webSearchConfigChange({ ...baseConfig, backend });
      expect(change.action).toBe("set_web_search");
      expect(change.backend).toBe(backend);
      expect(change.max_results).toBe(8);
      expect(change.time_range).toBe("week");
      expect(change).not.toHaveProperty("routes");
      expect(change).not.toHaveProperty("api_key");
    }
  });

  it("切换后端时未修改或留空的搜索路线不会生成删除操作", () => {
    expect(webSearchRouteChanges(baseConfig.routes, {
      private_search: baseConfig.routes.private_search,
      group_search: "",
    })).toEqual([]);
    expect(webSearchRouteChanges(baseConfig.routes, {
      private_search: "openai:gpt-search-new",
      group_search: baseConfig.routes.group_search,
    })).toEqual([{
      action: "set_search_route",
      name: "private_search",
      model: "openai:gpt-search-new",
    }]);
  });

  it("提交前校验结果数和超时顺序", () => {
    expect(() => webSearchConfigChange({ ...baseConfig, maxResults: 0 })).toThrow(/1 到 10/);
    expect(() => webSearchConfigChange({ ...baseConfig, maxResults: 1.5 })).toThrow(/1 到 10/);
    expect(() => webSearchConfigChange({ ...baseConfig, connectTimeoutSeconds: 20, firstResponseTimeoutSeconds: 10 }))
      .toThrow(/连接超时不能大于首响应超时/);
    expect(() => webSearchConfigChange({ ...baseConfig, firstResponseTimeoutSeconds: 50, totalTimeoutSeconds: 40 }))
      .toThrow(/首响应超时不能大于总超时/);
    expect(() => webSearchConfigChange({ ...baseConfig, connectTimeoutSeconds: 1.5 })).toThrow(/整数秒数/);
  });
});

describe("Tavily 凭据提示", () => {
  it("选择 Tavily 且未配置 Key 时显示明确提示", () => {
    expect(tavilyCredentialNotice("tavily", false)).toMatch(/Tavily API Key 尚未配置/);
    expect(tavilyCredentialNotice("tavily", true)).toBe("");
    expect(tavilyCredentialNotice("provider_native", false)).toBe("");
    expect(tavilyCredentialNotice("disabled", false)).toBe("");
  });

  it("后端中文标签保持稳定", () => {
    expect(webSearchBackendLabel("provider_native")).toBe("Provider 原生搜索");
    expect(webSearchBackendLabel("tavily")).toBe("Tavily");
    expect(webSearchBackendLabel("disabled")).toBe("已关闭");
  });
});
