import type { AgentConfigSnapshot } from "../../types.js";
import {
  readAgentWebSearchConfig,
  webSearchConfigChange,
  webSearchRouteChanges,
  type AgentWebSearchConfig,
} from "./agent-web-search.js";

/**
 * Agent 策略草稿：页面编辑状态与 agent.toml 保存契约的双向投影。
 *
 * 从旧版 agent-fields.ts 的 saveAgent 契约平移：一次保存提交
 * set_knowledge + set_web_search + set_model_route×3 + set_search_route×n + set_scene×2，
 * 搜索路线只在真实修改时生成操作，空输入保留 agent.toml 当前内容。
 */

export const CONVERSATION_ROUTE_NAMES = ["private_main", "group_main", "aux"] as const;
export const SEARCH_ROUTE_NAMES = ["private_search", "group_search"] as const;
export const SCENE_NAMES = ["private", "group"] as const;

export type ConversationRouteName = (typeof CONVERSATION_ROUTE_NAMES)[number];
export type SearchRouteName = (typeof SEARCH_ROUTE_NAMES)[number];
export type AgentSceneName = (typeof SCENE_NAMES)[number];

export const AGENT_ROUTE_LABELS: Record<string, string> = {
  private_main: "私聊主路线",
  group_main: "群聊主路线",
  aux: "辅助任务路线",
};

export interface AgentSceneDraft {
  toolCallingEnabled: boolean;
  enabledTools: string[];
}

export interface AgentDraft {
  knowledgeMode: string;
  knowledgeEmbeddingEnabled: boolean;
  webSearch: AgentWebSearchConfig;
  searchRoutes: Record<SearchRouteName, string>;
  routeCandidates: Record<ConversationRouteName, string[]>;
  scenes: Record<AgentSceneName, AgentSceneDraft>;
}

export function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null;
}

export function record(value: unknown): Record<string, unknown> {
  return isRecord(value) ? { ...value } : {};
}

export function string(value: unknown): string {
  return typeof value === "string" ? value : "";
}

export function array(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function stringArray(value: unknown): string[] {
  return array(value).filter((item): item is string => typeof item === "string");
}

/** 从 agent.savedValue 构造页面草稿；缺失字段回落到与旧版一致的默认值。 */
export function createAgentDraft(agent: AgentConfigSnapshot): AgentDraft {
  const documentValue = record(agent.savedValue);
  const knowledge = record(documentValue.knowledge);
  const embedding = record(knowledge.embedding);
  const modelRoutes = record(documentValue.model_routes);
  const scenes = record(documentValue.scenes);
  const savedWebSearch = readAgentWebSearchConfig(documentValue);
  return {
    knowledgeMode: string(knowledge.mode) || "preflight",
    knowledgeEmbeddingEnabled: embedding.enabled === true,
    webSearch: savedWebSearch,
    searchRoutes: {
      private_search: savedWebSearch.routes.private_search ?? "",
      group_search: savedWebSearch.routes.group_search ?? "",
    },
    routeCandidates: {
      private_main: stringArray(record(modelRoutes.private_main).candidates),
      group_main: stringArray(record(modelRoutes.group_main).candidates),
      aux: stringArray(record(modelRoutes.aux).candidates),
    },
    scenes: {
      private: sceneDraft(scenes.private),
      group: sceneDraft(scenes.group),
    },
  };
}

function sceneDraft(value: unknown): AgentSceneDraft {
  const scene = record(value);
  return {
    toolCallingEnabled: scene.tool_calling_enabled === true,
    enabledTools: stringArray(scene.enabled_tools),
  };
}

/** 组装完整保存操作序列；联网搜索参数不合法时抛错，由调用方阻塞提交。 */
export function buildAgentChanges(draft: AgentDraft, agent: AgentConfigSnapshot): unknown[] {
  const documentValue = record(agent.savedValue);
  const embedding = record(record(documentValue.knowledge).embedding);
  const savedWebSearch = readAgentWebSearchConfig(documentValue);
  const changes: unknown[] = [
    {
      action: "set_knowledge",
      mode: draft.knowledgeMode,
      embedding: {
        enabled: draft.knowledgeEmbeddingEnabled,
        // cache_dir 不在页面暴露：沿用 agent.toml 已保存值，缺失时回落默认路径。
        cache_dir: string(embedding.cache_dir) || "cache/knowledge-embedding",
      },
    },
    webSearchConfigChange(draft.webSearch),
  ];
  for (const name of CONVERSATION_ROUTE_NAMES) {
    changes.push({ action: "set_model_route", name, candidates: draft.routeCandidates[name] ?? [] });
  }
  changes.push(...webSearchRouteChanges(savedWebSearch.routes, draft.searchRoutes));
  for (const sceneName of SCENE_NAMES) {
    changes.push(buildSceneChange(draft, agent, sceneName));
  }
  return changes;
}

/** 单场景保存：保留 agent.toml 中该场景的其余字段，只覆盖工具开关与白名单。 */
export function buildSceneChange(draft: AgentDraft, agent: AgentConfigSnapshot, sceneName: AgentSceneName): Record<string, unknown> {
  const scenes = record(record(agent.savedValue).scenes);
  const sceneDraftState = draft.scenes[sceneName];
  return {
    action: "set_scene",
    scene: sceneName,
    config: {
      ...record(scenes[sceneName]),
      tool_calling_enabled: sceneDraftState.toolCallingEnabled,
      enabled_tools: [...sceneDraftState.enabledTools],
    },
  };
}
