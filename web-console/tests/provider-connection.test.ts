import { describe, expect, it } from "vitest";
import {
  buildProviderPayload,
  hasCanonicalProvider,
  removeProviderChange,
  savedProvidersOf,
  setProviderChange,
  validateConnectionId,
} from "../src/features/configuration/provider-connection.js";
import type { AgentConfigSnapshot } from "../src/types.js";

describe("Connection ID 规则与 set_provider 变更", () => {
  const values = {
    display_name: "自定义",
    enabled: true,
    kind: "openai_responses",
    base_url: "https://custom.example/v1",
    auth_header: "Authorization",
    auth_scheme: "Bearer",
    request_timeout_seconds: "",
  };

  it("创建 Provider 生成正确的 set_provider change，且不提交凭证 Namespace", () => {
    const change = setProviderChange("custom_router", values, false);
    expect(change).toEqual({
      action: "set_provider",
      id: "custom_router",
      provider: {
        display_name: "自定义",
        enabled: true,
        kind: "openai_responses",
        base_url: "https://custom.example/v1",
        auth_header: "Authorization",
        auth_scheme: "Bearer",
        request_timeout_seconds: null,
        chat_fallback: false,
      },
    });
    expect("api_key_env" in (change.provider as Record<string, unknown>)).toBe(false);
  });

  it("历史非 canonical ID 按原始 key 精确保存；新建必须 canonical 小写", () => {
    expect(setProviderChange("MyProxy", values, true).id).toBe("MyProxy");
    expect(validateConnectionId("MyProxy", true)).toBe("MyProxy");
    // 新建拒绝大写与非法字符；已有连接仍要求合法 key 语法
    expect(() => validateConnectionId("MyProxy", false)).toThrow("小写");
    expect(() => validateConnectionId("unsafe/path", false)).toThrow();
    expect(() => validateConnectionId("", false)).toThrow();
    expect(() => validateConnectionId("a".repeat(65), false)).toThrow();
    expect(() => validateConnectionId("unsafe/path", true)).toThrow();
    expect(() => validateConnectionId("", true)).toThrow();
    expect(() => validateConnectionId("a".repeat(65), true)).toThrow();
    expect(validateConnectionId("  spaced_ok  ", false)).toBe("spaced_ok");
  });

  it("hasCanonicalProvider 按大小写不敏感检测别名冲突", () => {
    const saved = { MyProxy: {}, custom: {} };
    expect(hasCanonicalProvider(saved, "myproxy")).toBe(true);
    expect(hasCanonicalProvider(saved, "MYPROXY")).toBe(true);
    expect(hasCanonicalProvider(saved, "other")).toBe(false);
  });

  it("buildProviderPayload：空值语义与 Responses fallback 显式关闭", () => {
    const payload = buildProviderPayload({
      display_name: "  ",
      enabled: false,
      kind: "openai_compatible",
      base_url: " https://p.example/v1 ",
      auth_header: "Authorization",
      auth_scheme: "",
      request_timeout_seconds: "30",
    });
    expect(payload).toEqual({
      display_name: null,
      enabled: false,
      kind: "openai_compatible",
      base_url: "https://p.example/v1",
      auth_header: "Authorization",
      auth_scheme: null,
      request_timeout_seconds: 30,
    });
    expect("chat_fallback" in payload).toBe(false);
  });

  it("remove_provider change 只携带原始 ID", () => {
    expect(removeProviderChange("MyProxy")).toEqual({ action: "remove_provider", id: "MyProxy" });
  });

  it("savedProvidersOf 读取 agent.toml providers 表并容忍缺失", () => {
    const agent = {
      savedValue: { providers: { MyProxy: { kind: "openai_compatible" }, broken: null } },
    } as unknown as AgentConfigSnapshot;
    const saved = savedProvidersOf(agent);
    expect(saved.MyProxy).toEqual({ kind: "openai_compatible" });
    expect(saved.broken).toEqual({});
    expect(savedProvidersOf(null)).toEqual({});
    expect(savedProvidersOf({ savedValue: null } as unknown as AgentConfigSnapshot)).toEqual({});
  });
});
