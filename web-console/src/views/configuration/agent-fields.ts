import type { ConfigurationSnapshot } from "../../types.js";
import { updateAgentConfiguration } from "../../api.js";
import { agentToolOptions, selectedAgentToolNames, type AgentToolOption } from "../../agent-tools.js";
import { array, button, checkboxField, configurationGroup, element, fieldGroup, inputId, numberField, record, selectField, sourceLabel, statusField, string, textField } from "./fields.js";
import { current, runSave } from "./state.js";
import { errorMessage, showResult } from "./ui.js";
import { isWebSearchBackend, readAgentWebSearchConfig, updateTavilyCredentialStatus, webSearchBackendLabel, webSearchConfigChange, webSearchFormConfig, webSearchRouteChanges } from "./web-search.js";
import { openCodeProviderChange, readOpenCodeProviders, renderOpenCodeProviders, renderOpenCodeRouteHints } from "./opencode-providers.js";
import { renderModelRouteEditor } from "./model-route-editor.js";
import { shouldAutosaveOnBlur } from "./autosave.js";

export const AGENT_ROUTE_LABELS: Record<string, string> = {
  private_main: "私聊主路线",
  group_main: "群聊主路线",
  aux: "辅助任务路线",
  private_search: "私聊搜索路线",
  group_search: "群聊搜索路线",
};

export function renderAgent(snapshot: ConfigurationSnapshot): void {
  const target = element("agent-config-fields");
  target.replaceChildren();
  const agent = snapshot.agent;
  if (!agent || !agent.fileExists) {
    target.textContent = "Agent 策略文件尚不可用；请检查默认 config/agent.toml 是否可写。";
    element("save-agent-config", HTMLButtonElement).disabled = true;
    return;
  }
  const documentValue = record(agent.savedValue);
  const knowledge = record(documentValue.knowledge);
  const embedding = record(knowledge.embedding);
  const runningKnowledge = record(record(agent.runningValue).knowledge);
  const runningEmbedding = record(runningKnowledge.embedding);
  target.append(configurationGroup("memory-knowledge", "agent", fieldGroup("知识检索", [
    selectField("知识检索模式", "agent-knowledge-mode", string(knowledge.mode) || "preflight", [
      ["preflight", "preflight（高相关时条件注入）"],
      ["tool", "tool（完全由 Agent 检索）"],
      ["auto", "auto（紧急回退）"],
    ], !agent.editable, "agent"),
    checkboxField("本地语义召回", "agent-knowledge-embedding-enabled", embedding.enabled === true, !agent.editable, "agent"),
    statusField(
      `当前生效：${string(runningKnowledge.mode) || "preflight"} · 本地语义召回：${runningEmbedding.enabled === true ? "开启" : "关闭"}`,
      `来源：${sourceLabel(agent.source)}${agent.pendingRestart ? " · 已保存变更等待重启" : ""}`,
    ),
    statusField(
      "本地模型资源",
      "首次开启会下载 BAAI/bge-small-zh-v1.5，并增加 CPU、内存占用；低配置服务器建议关闭。",
    ),
  ])));
  const savedWebSearch = readAgentWebSearchConfig(documentValue);
  const runningWebSearch = readAgentWebSearchConfig(agent.runningValue);
  const tavilyKeyConfigured = snapshot.fields.some(
    (field) => field.key === "tools.web_search.tavily.api_key" && field.configured,
  );
  const backendPendingRestart = savedWebSearch.backend !== runningWebSearch.backend;
  const credentialStatus = statusField("Tavily 凭据", "");
  credentialStatus.id = "agent-web-search-credential-status";
  target.append(configurationGroup("online-tools", "agent", fieldGroup("联网搜索", [
    statusField(
      `当前生效后端：${webSearchBackendLabel(runningWebSearch.backend)}`,
      `已保存后端：${webSearchBackendLabel(savedWebSearch.backend)} · ${backendPendingRestart ? "等待重启" : "当前已生效"}`,
    ),
    selectField("搜索后端", "agent-web-search-backend", savedWebSearch.backend, [
      ["provider_native", "Provider 原生搜索"],
      ["tavily", "Tavily"],
      ["disabled", "关闭联网搜索"],
    ], !agent.editable, "agent"),
    numberField("Tavily 结果数", "agent-web-search-max-results", savedWebSearch.maxResults, 1, 10, !agent.editable, "agent"),
    selectField("Tavily 搜索深度", "agent-web-search-depth", savedWebSearch.searchDepth, [
      ["basic", "basic"],
      ["advanced", "advanced"],
    ], !agent.editable, "agent"),
    selectField("Tavily 主题", "agent-web-search-topic", savedWebSearch.topic, [
      ["general", "通用"],
      ["news", "新闻"],
      ["finance", "金融"],
    ], !agent.editable, "agent"),
    selectField("Tavily 时间范围", "agent-web-search-time-range", savedWebSearch.timeRange ?? "", [
      ["", "不限"],
      ["day", "最近一天"],
      ["week", "最近一周"],
      ["month", "最近一月"],
      ["year", "最近一年"],
    ], !agent.editable, "agent"),
    numberField("连接超时（秒）", "agent-web-search-connect-timeout", savedWebSearch.connectTimeoutSeconds, 1, 3600, !agent.editable, "agent"),
    numberField("首响应超时（秒）", "agent-web-search-first-response-timeout", savedWebSearch.firstResponseTimeoutSeconds, 1, 3600, !agent.editable, "agent"),
    numberField("总超时（秒）", "agent-web-search-total-timeout", savedWebSearch.totalTimeoutSeconds, 1, 3600, !agent.editable, "agent"),
    credentialStatus,
  ])));
  const backendSelect = element("agent-web-search-backend", HTMLSelectElement);
  const refreshCredentialStatus = (): void => {
    updateTavilyCredentialStatus(
      isWebSearchBackend(backendSelect.value) ? backendSelect.value : "provider_native",
      tavilyKeyConfigured,
    );
  };
  backendSelect.addEventListener("change", refreshCredentialStatus);
  refreshCredentialStatus();
  target.append(configurationGroup("models-providers", "agent", renderOpenCodeProviders(
    snapshot,
    async (form) => {
      let change: Record<string, unknown>;
      try {
        change = openCodeProviderChange(form);
      } catch (cause) {
        showResult(errorMessage(cause), true);
        return;
      }
      await runSave(async () => updateAgentConfiguration(current!.agent!.revision, [change]));
    },
    async (id) => {
      await runSave(async () => updateAgentConfiguration(current!.agent!.revision, [{
        action: "remove_provider",
        id,
      }]));
    },
  )));
  const openCodeKeyConfigured = snapshot.fields.some(
    (field) => field.key === "provider.opencode.api_key" && field.configured,
  );
  const modelRoutes = record(documentValue.model_routes);
  const routes = document.createElement("div");
  const conversationRoutes = fieldGroup("聊天与辅助路线", [
    ...["private_main", "group_main", "aux"].map((routeName) => {
      const route = record(modelRoutes[routeName]);
      return renderModelRouteEditor({
        label: AGENT_ROUTE_LABELS[routeName] ?? routeName,
        inputId: `agent-route-${routeName}`,
        candidates: array(route.candidates).map(string).filter(Boolean),
        disabled: !agent.editable,
        routeName,
      });
    }),
  ]);
  conversationRoutes.classList.add("model-route-field-group");
  const searchRoutes = fieldGroup("搜索路线", [
    ...["private_search", "group_search"].map((routeName) => textField(
      AGENT_ROUTE_LABELS[routeName] ?? routeName,
      `agent-search-${routeName}`,
      savedWebSearch.routes[routeName] ?? "",
      !agent.editable,
      "agent",
    )),
  ]);
  searchRoutes.classList.add("model-route-field-group");
  // 聊天候选链与搜索模型的编辑方式不同，分组后避免奇数项跨行混排造成的视觉错位。
  routes.append(conversationRoutes, searchRoutes);
  routes.append(renderOpenCodeRouteHints(
    !agent.editable,
    readOpenCodeProviders(documentValue).filter((provider) => provider.enabled).map((provider) => provider.id),
    openCodeKeyConfigured,
  ));
  target.append(configurationGroup("model-routing", "agent", routes));
  const scenes = record(documentValue.scenes);
  for (const sceneName of ["private", "group"]) {
    const scene = record(scenes[sceneName]);
    const row = document.createElement("div");
    row.className = "config-row compact-row";
    const label = document.createElement("label");
    label.htmlFor = `agent-tool-${sceneName}`;
    label.textContent = `${sceneName === "private" ? "私聊" : "群聊"} Tool Calling`;
    const input = document.createElement("input");
    input.id = `agent-tool-${sceneName}`;
    input.type = "checkbox";
    input.checked = scene.tool_calling_enabled === true;
    input.disabled = !agent.editable;
    input.dataset.autosaveScope = "agent-scene";
    input.dataset.autosaveScene = sceneName;
    row.append(label, input);
    const sceneGroup = document.createElement("div");
    sceneGroup.append(row);

    const tools = document.createElement("fieldset");
    tools.className = "tool-whitelist";
    const legend = document.createElement("legend");
    legend.textContent = `${sceneName === "private" ? "私聊" : "群聊"}工具白名单`;
    tools.append(legend);
    const savedNames = array(scene.enabled_tools).filter((value): value is string => typeof value === "string");
    const visibleTools = agentToolOptions(snapshot.registeredTools, savedNames, agent.editable);
    if (visibleTools.length === 0) {
      const hint = document.createElement("p");
      hint.className = "hint";
      hint.textContent = "当前没有可用的已注册工具。";
      tools.append(hint);
    } else {
      const grid = document.createElement("div");
      grid.className = "tool-whitelist-grid";
      for (const tool of visibleTools) {
        grid.append(toolCheckbox(tool, sceneName));
      }
      tools.append(grid);
    }
    const saveScene = document.createElement("button");
    saveScene.type = "button";
    saveScene.className = "secondary tool-whitelist-save";
    saveScene.textContent = `保存${sceneName === "private" ? "私聊" : "群聊"}配置`;
    saveScene.disabled = !agent.editable;
    saveScene.onclick = () => void saveAgentScene(sceneName);
    tools.append(saveScene);
    sceneGroup.append(tools);
    target.append(configurationGroup("online-tools", "agent", sceneGroup));
  }
  const save = element("save-agent-config", HTMLButtonElement);
  save.disabled = !agent.editable;
  save.onclick = () => void saveAgent();
}

export async function saveAgent(): Promise<void> {
  if (!current?.agent) return;
  let webSearchChange: Record<string, unknown>;
  try {
    webSearchChange = webSearchConfigChange(webSearchFormConfig());
  } catch (cause) {
    showResult(errorMessage(cause), true);
    return;
  }
  const documentValue = record(current.agent.savedValue);
  const scenes = record(documentValue.scenes);
  const embedding = record(record(documentValue.knowledge).embedding);
  const changes: unknown[] = [{
    action: "set_knowledge",
    mode: element("agent-knowledge-mode", HTMLSelectElement).value,
    embedding: {
      enabled: element("agent-knowledge-embedding-enabled", HTMLInputElement).checked,
      cache_dir: string(embedding.cache_dir) || "cache/knowledge-embedding",
    },
  }, webSearchChange];
  for (const routeName of ["private_main", "group_main", "aux"]) {
    const candidates = element(`agent-route-${routeName}`, HTMLInputElement).value
      .split(",").map((value) => value.trim()).filter(Boolean);
    changes.push({ action: "set_model_route", name: routeName, candidates });
  }
  const savedRoutes = readAgentWebSearchConfig(documentValue).routes;
  changes.push(...webSearchRouteChanges(savedRoutes, {
    private_search: element("agent-search-private_search", HTMLInputElement).value,
    group_search: element("agent-search-group_search", HTMLInputElement).value,
  }));
  for (const sceneName of ["private", "group"]) {
    changes.push({ action: "set_scene", scene: sceneName, config: agentSceneConfig(sceneName, scenes) });
  }
  await runSave(async () => updateAgentConfiguration(current!.agent!.revision, changes));
}

export async function saveAgentScene(sceneName: string): Promise<void> {
  if (!current?.agent) return;
  const scenes = record(record(current.agent.savedValue).scenes);
  await runSave(async () => updateAgentConfiguration(current!.agent!.revision, [{
    action: "set_scene",
    scene: sceneName,
    config: agentSceneConfig(sceneName, scenes),
  }]));
}

export async function saveOpenCodeProvider(id: string): Promise<void> {
  if (!current?.agent) return;
  const baseUrl = element(`${id}-base-url`, HTMLInputElement);
  const timeout = element(`${id}-timeout`, HTMLInputElement);
  const saved = readOpenCodeProviders(current.agent.savedValue).find((provider) => provider.id === id);
  // 未添加的预设只能通过“添加 Provider”显式启用，浏览默认字段不能改变配置。
  if (!saved?.enabled) return;
  const form = {
    ...saved,
    baseUrl: baseUrl.value,
    requestTimeoutSeconds: timeout.value.trim() ? Number(timeout.value) : null,
    enabled: true,
  };
  if (!shouldAutosaveOnBlur({ scope: "agent", value: form, baseline: saved })) return;
  let change: Record<string, unknown>;
  try {
    change = openCodeProviderChange(form);
  } catch (cause) {
    showResult(errorMessage(cause), true);
    return;
  }
  await runSave(async () => updateAgentConfiguration(current?.agent?.revision ?? "missing", [change]));
}

export function agentSceneConfig(sceneName: string, scenes: Record<string, unknown>): Record<string, unknown> {
  const toolInputs = document.querySelectorAll<HTMLInputElement>(`input[data-agent-tool="${sceneName}"]`);
  return {
    ...record(scenes[sceneName]),
    tool_calling_enabled: element(`agent-tool-${sceneName}`, HTMLInputElement).checked,
    enabled_tools: selectedAgentToolNames(toolInputs),
  };
}

export function toolCheckbox(tool: AgentToolOption, sceneName: string): HTMLElement {
  const label = document.createElement("label");
  label.className = "tool-checkbox";
  label.title = tool.description;
  const input = document.createElement("input");
  input.type = "checkbox";
  input.value = tool.name;
  input.checked = tool.checked;
  input.disabled = tool.disabled;
  input.dataset.agentTool = sceneName;
  input.dataset.autosaveScene = sceneName;
  const name = document.createElement("span");
  name.textContent = tool.name === "image_generation" ? "图片生成" : tool.name;
  label.append(input, name);
  if (!tool.registered) {
    const state = document.createElement("span");
    state.className = "tool-registration-state";
    state.textContent = "当前进程未注册";
    label.append(state);
  }
  return label;
}
