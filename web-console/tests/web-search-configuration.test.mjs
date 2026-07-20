import test from "node:test";
import assert from "node:assert/strict";

import {
  readAgentWebSearchConfig,
  tavilyCredentialNotice,
  webSearchConfigChange,
  webSearchRouteChanges,
} from "../dist/views/configuration.js";

const baseConfig = {
  backend: "tavily",
  maxResults: 8,
  searchDepth: "advanced",
  topic: "news",
  timeRange: "week",
  connectTimeoutSeconds: 5,
  firstResponseTimeoutSeconds: 15,
  totalTimeoutSeconds: 45,
  routes: {
    private_search: "openai:gpt-search",
    group_search: "gemini:gemini-2.5-flash",
  },
};

test("WebUI 只读取 tools.web_search 及其 routes", () => {
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

  assert.deepEqual(parsed, baseConfig);
  assert.ok(!Object.values(parsed.routes).includes("legacy-must-be-ignored"));
});

test("WebUI 为三种后端生成结构化保存操作且不携带 route 或 secret", () => {
  for (const backend of ["provider_native", "tavily", "disabled"]) {
    const change = webSearchConfigChange({ ...baseConfig, backend });
    assert.equal(change.action, "set_web_search");
    assert.equal(change.backend, backend);
    assert.equal(change.max_results, 8);
    assert.equal(change.time_range, "week");
    assert.ok(!("routes" in change));
    assert.ok(!("api_key" in change));
  }
});

test("切换后端时未修改或留空的搜索路线不会生成删除操作", () => {
  assert.deepEqual(webSearchRouteChanges(baseConfig.routes, {
    private_search: baseConfig.routes.private_search,
    group_search: "",
  }), []);
  assert.deepEqual(webSearchRouteChanges(baseConfig.routes, {
    private_search: "openai:gpt-search-new",
    group_search: baseConfig.routes.group_search,
  }), [{
    action: "set_search_route",
    name: "private_search",
    model: "openai:gpt-search-new",
  }]);
});

test("WebUI 在提交前校验结果数和超时顺序", () => {
  assert.throws(
    () => webSearchConfigChange({ ...baseConfig, maxResults: 0 }),
    /1 到 10/,
  );
  assert.throws(
    () => webSearchConfigChange({ ...baseConfig, maxResults: 1.5 }),
    /1 到 10/,
  );
  assert.throws(
    () => webSearchConfigChange({ ...baseConfig, connectTimeoutSeconds: 20, firstResponseTimeoutSeconds: 10 }),
    /连接超时不能大于首响应超时/,
  );
  assert.throws(
    () => webSearchConfigChange({ ...baseConfig, firstResponseTimeoutSeconds: 50, totalTimeoutSeconds: 40 }),
    /首响应超时不能大于总超时/,
  );
});

test("选择 Tavily 且未配置 Key 时显示明确提示", () => {
  assert.match(tavilyCredentialNotice("tavily", false), /Tavily API Key 尚未配置/);
  assert.equal(tavilyCredentialNotice("tavily", true), "");
  assert.equal(tavilyCredentialNotice("provider_native", false), "");
  assert.equal(tavilyCredentialNotice("disabled", false), "");
});
