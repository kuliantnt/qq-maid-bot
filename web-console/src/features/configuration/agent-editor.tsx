import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { updateAgentConfiguration } from "../../api.js";
import { Button } from "../../components/ui/button.js";
import { Field, Input } from "../../components/ui/field.js";
import { showToast } from "../../stores/toast.js";
import type { ConfigurationSnapshot } from "../../types.js";
import { agentToolDisplayName, agentToolOptions } from "./agent-tool-options.js";
import {
  AGENT_ROUTE_LABELS,
  CONVERSATION_ROUTE_NAMES,
  SCENE_NAMES,
  buildAgentChanges,
  buildSceneChange,
  createAgentDraft,
  record,
  string,
  type AgentDraft,
  type AgentSceneName,
} from "./agent-draft.js";
import {
  readAgentWebSearchConfig,
  tavilyCredentialNotice,
  webSearchBackendLabel,
  type AgentWebSearchBackend,
} from "./agent-web-search.js";
import { ModelRouteEditor } from "./model-route-editor.js";
import type { ConfigurationBusinessGroup } from "./configuration-navigation.js";

const KNOWLEDGE_MODE_OPTIONS: ReadonlyArray<readonly [string, string]> = [
  ["preflight", "preflight（高相关时条件注入）"],
  ["tool", "tool（完全由 Agent 检索）"],
  ["auto", "auto（紧急回退）"],
];

const WEB_SEARCH_BACKEND_OPTIONS: ReadonlyArray<readonly [AgentWebSearchBackend, string]> = [
  ["provider_native", "Provider 原生搜索"],
  ["tavily", "Tavily"],
  ["disabled", "关闭联网搜索"],
];

const SELECT_CLASS = "border border-line bg-input px-3 py-2 text-sm text-ink outline-none";

/**
 * Agent 策略结构化编辑器：知识检索、联网搜索、模型路线与场景白名单。
 *
 * 保存契约与旧版一致：完整保存提交 set_knowledge / set_web_search /
 * set_model_route / set_search_route / set_scene 操作序列；场景白名单另支持
 * 按场景单独保存。草稿跨业务域 Tab 共享，revision 变化（保存成功或外部修改）
 * 后以服务端返回为准重建。
 */
export function AgentEditor({ snapshot, group }: { snapshot: ConfigurationSnapshot; group: ConfigurationBusinessGroup }) {
  const queryClient = useQueryClient();
  const agent = snapshot.agent;
  const [draft, setDraft] = useState<AgentDraft | null>(() => (agent && agent.fileExists ? createAgentDraft(agent) : null));
  const [result, setResult] = useState<{ error: boolean; text: string } | null>(null);
  const [seenRevision, setSeenRevision] = useState(agent?.revision ?? "missing");

  // revision 变化说明保存成功或配置被外部修改：丢弃本地草稿，以服务端为准。
  const currentRevision = agent?.revision ?? "missing";
  if (draft !== null && seenRevision !== currentRevision) {
    setSeenRevision(currentRevision);
    setDraft(agent && agent.fileExists ? createAgentDraft(agent) : null);
  }

  const tavilyKeyConfigured = snapshot.fields.some(
    (field) => field.key === "tools.web_search.tavily.api_key" && field.configured,
  );

  const saveMutation = useMutation({
    mutationFn: ({ revision, changes }: { revision: string; changes: unknown[] }) => updateAgentConfiguration(revision, changes),
    onSuccess: (next) => {
      const pending = next.agent?.pendingRestart ? "重启后完全生效" : "保存成功";
      setResult({ error: false, text: `Agent 策略已真实持久化，${pending}` });
      showToast("info", "Agent 策略已保存");
      void queryClient.invalidateQueries({ queryKey: ["configuration"] });
    },
    onError: (cause) => setResult({ error: true, text: cause instanceof Error ? cause.message : "Agent 策略保存失败" }),
  });

  if (!agent || !agent.fileExists || draft === null) {
    return (
      <p role="alert" className="m-0 text-sm text-warning">
        Agent 策略文件尚不可用；请检查默认 config/agent.toml 是否可写。
      </p>
    );
  }

  const editable = agent.editable;
  const runningDocument = agent.runningValue;
  const runningKnowledge = record(record(runningDocument).knowledge);
  const runningEmbedding = record(runningKnowledge.embedding);
  const showKnowledge = group === "memory-knowledge";
  const showWebSearch = group === "online-tools";
  const showScenes = group === "online-tools";
  const showRoutes = group === "model-routing";

  // 联网搜索参数的页面侧校验在组装操作时执行；抛错时阻塞提交并保留草稿。
  const saveAll = (): void => {
    let changes: unknown[];
    try {
      changes = buildAgentChanges(draft, agent);
    } catch (cause) {
      setResult({ error: true, text: cause instanceof Error ? cause.message : "Agent 策略校验失败" });
      return;
    }
    saveMutation.mutate({ revision: agent.revision, changes });
  };

  const saveScene = (sceneName: AgentSceneName): void => {
    saveMutation.mutate({ revision: agent.revision, changes: [buildSceneChange(draft, agent, sceneName)] });
  };

  return (
    <div className="flex flex-col gap-6">
      {result ? (
        <p aria-live="polite" role={result.error ? "alert" : "status"} className={`m-0 text-sm font-semibold ${result.error ? "text-error" : "text-success"}`}>
          {result.text}
        </p>
      ) : null}

      {showKnowledge ? (
        <section aria-label="知识检索" className="flex flex-col gap-3">
          <h3 className="m-0 text-base font-bold">知识检索</h3>
          <Field
            label="知识检索模式"
            id="agent-knowledge-mode"
            hint={`当前生效：${string(runningKnowledge.mode) || "preflight"} · 本地语义召回：${runningEmbedding.enabled === true ? "开启" : "关闭"}`}
          >
            {(props) => (
              <select
                {...props}
                disabled={!editable}
                value={draft.knowledgeMode}
                onChange={(event) => setDraft({ ...draft, knowledgeMode: event.target.value })}
                className={SELECT_CLASS}
              >
                {KNOWLEDGE_MODE_OPTIONS.map(([value, label]) => (
                  <option key={value} value={value}>{label}</option>
                ))}
              </select>
            )}
          </Field>
          <label className="flex items-center gap-2 text-sm text-ink">
            <input
              type="checkbox"
              disabled={!editable}
              checked={draft.knowledgeEmbeddingEnabled}
              onChange={(event) => setDraft({ ...draft, knowledgeEmbeddingEnabled: event.target.checked })}
              className="size-3.5 accent-[var(--console-accent)]"
            />
            本地语义召回
          </label>
          <p className="m-0 text-xs leading-relaxed text-muted">
            首次开启会下载 BAAI/bge-small-zh-v1.5，并增加 CPU、内存占用；低配置服务器建议关闭。
          </p>
        </section>
      ) : null}

      {showWebSearch ? (
        <WebSearchSection draft={draft} onDraftChange={setDraft} editable={editable} tavilyKeyConfigured={tavilyKeyConfigured} runningValue={runningDocument} />
      ) : null}

      {showRoutes ? (
        <section aria-label="模型候选路线" className="flex flex-col gap-5">
          <h3 className="m-0 text-base font-bold">模型候选路线</h3>
          {CONVERSATION_ROUTE_NAMES.map((routeName) => (
            <ModelRouteEditor
              key={routeName}
              label={AGENT_ROUTE_LABELS[routeName] ?? routeName}
              candidates={draft.routeCandidates[routeName]}
              disabled={!editable}
              onChange={(candidates) =>
                setDraft({ ...draft, routeCandidates: { ...draft.routeCandidates, [routeName]: candidates } })}
            />
          ))}
          <div className="flex flex-col gap-3 border-t border-line-inner pt-4">
            <h4 className="m-0 text-sm font-bold">搜索路线</h4>
            {SCENE_SEARCH_ROUTE_FIELDS.map(([name, label]) => (
              <Field key={name} label={label} id={`agent-search-${name}`} hint="留空保留 agent.toml 当前搜索模型">
                {(props) => (
                  <Input
                    {...props}
                    disabled={!editable}
                    value={draft.searchRoutes[name]}
                    onChange={(event) =>
                      setDraft({ ...draft, searchRoutes: { ...draft.searchRoutes, [name]: event.target.value } })}
                    className="max-w-96"
                  />
                )}
              </Field>
            ))}
          </div>
        </section>
      ) : null}

      {showScenes ? <AgentScenes draft={draft} agent={snapshot} editable={editable} onDraftChange={setDraft} onSaveScene={saveScene} busy={saveMutation.isPending} /> : null}

      {editable ? (
        <div className="flex flex-wrap items-center gap-3 border-t border-line pt-4">
          <Button disabled={saveMutation.isPending} onClick={saveAll}>
            {saveMutation.isPending ? "保存中…" : "保存 Agent 策略"}
          </Button>
          <span className="text-xs text-muted">
            保存覆盖知识检索、联网搜索、模型路线与场景配置；已保存 revision：{agent.revision}
          </span>
        </div>
      ) : null}
    </div>
  );
}

const SCENE_SEARCH_ROUTE_FIELDS: ReadonlyArray<readonly ["private_search" | "group_search", string]> = [
  ["private_search", "私聊搜索路线"],
  ["group_search", "群聊搜索路线"],
];

/** 联网搜索表单：后端、Tavily 参数、超时链与凭据状态提示。 */
function WebSearchSection({ draft, onDraftChange, editable, tavilyKeyConfigured, runningValue }: {
  draft: AgentDraft;
  onDraftChange: (draft: AgentDraft) => void;
  editable: boolean;
  tavilyKeyConfigured: boolean;
  runningValue: unknown;
}) {
  const webSearch = draft.webSearch;
  const runningBackend = readAgentWebSearchConfig(runningValue).backend;
  const backendPendingRestart = runningBackend !== webSearch.backend;
  const notice = tavilyCredentialNotice(webSearch.backend, tavilyKeyConfigured);
  const patch = (changes: Partial<AgentDraft["webSearch"]>) =>
    onDraftChange({ ...draft, webSearch: { ...webSearch, ...changes } });

  return (
    <section aria-label="联网搜索" className="flex flex-col gap-3">
      <h3 className="m-0 text-base font-bold">联网搜索</h3>
      <p className="m-0 text-xs text-muted">
        当前生效后端：{webSearchBackendLabel(runningBackend)} · 已保存后端：{webSearchBackendLabel(webSearch.backend)}
        {backendPendingRestart ? " · 等待重启" : " · 当前已生效"}
      </p>
      <Field label="搜索后端" id="agent-web-search-backend">
        {(props) => (
          <select
            {...props}
            disabled={!editable}
            value={webSearch.backend}
            onChange={(event) => patch({ backend: event.target.value as AgentWebSearchBackend })}
            className={SELECT_CLASS}
          >
            {WEB_SEARCH_BACKEND_OPTIONS.map(([value, label]) => (
              <option key={value} value={value}>{label}</option>
            ))}
          </select>
        )}
      </Field>
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
        <Field label="Tavily 结果数" id="agent-web-search-max-results" hint="1 到 10 之间的整数">
          {(props) => (
            <Input
              {...props}
              type="number"
              min={1}
              max={10}
              disabled={!editable}
              value={String(webSearch.maxResults)}
              onChange={(event) => patch({ maxResults: Number(event.target.value) })}
            />
          )}
        </Field>
        <Field label="Tavily 搜索深度" id="agent-web-search-depth">
          {(props) => (
            <select
              {...props}
              disabled={!editable}
              value={webSearch.searchDepth}
              onChange={(event) => patch({ searchDepth: event.target.value === "advanced" ? "advanced" : "basic" })}
              className={SELECT_CLASS}
            >
              <option value="basic">basic</option>
              <option value="advanced">advanced</option>
            </select>
          )}
        </Field>
        <Field label="Tavily 主题" id="agent-web-search-topic">
          {(props) => (
            <select
              {...props}
              disabled={!editable}
              value={webSearch.topic}
              onChange={(event) => patch({ topic: event.target.value as AgentDraft["webSearch"]["topic"] })}
              className={SELECT_CLASS}
            >
              <option value="general">通用</option>
              <option value="news">新闻</option>
              <option value="finance">金融</option>
            </select>
          )}
        </Field>
        <Field label="Tavily 时间范围" id="agent-web-search-time-range">
          {(props) => (
            <select
              {...props}
              disabled={!editable}
              value={webSearch.timeRange ?? ""}
              onChange={(event) => patch({ timeRange: event.target.value === "" ? null : event.target.value as AgentDraft["webSearch"]["timeRange"] })}
              className={SELECT_CLASS}
            >
              <option value="">不限</option>
              <option value="day">最近一天</option>
              <option value="week">最近一周</option>
              <option value="month">最近一月</option>
              <option value="year">最近一年</option>
            </select>
          )}
        </Field>
        <Field label="连接超时（秒）" id="agent-web-search-connect-timeout">
          {(props) => (
            <Input
              {...props}
              type="number"
              min={1}
              disabled={!editable}
              value={String(webSearch.connectTimeoutSeconds)}
              onChange={(event) => patch({ connectTimeoutSeconds: Number(event.target.value) })}
            />
          )}
        </Field>
        <Field label="首响应超时（秒）" id="agent-web-search-first-response-timeout">
          {(props) => (
            <Input
              {...props}
              type="number"
              min={1}
              disabled={!editable}
              value={String(webSearch.firstResponseTimeoutSeconds)}
              onChange={(event) => patch({ firstResponseTimeoutSeconds: Number(event.target.value) })}
            />
          )}
        </Field>
        <Field label="总超时（秒）" id="agent-web-search-total-timeout">
          {(props) => (
            <Input
              {...props}
              type="number"
              min={1}
              disabled={!editable}
              value={String(webSearch.totalTimeoutSeconds)}
              onChange={(event) => patch({ totalTimeoutSeconds: Number(event.target.value) })}
            />
          )}
        </Field>
      </div>
      <p
        aria-live="polite"
        className={`m-0 text-xs leading-relaxed ${notice ? "font-semibold text-warning" : "text-muted"}`}
      >
        {notice
          ? notice
          : tavilyKeyConfigured
            ? "Tavily API Key：已配置。密钥保存在安全配置中心，不会写入 agent.toml 或回传浏览器。"
            : "Tavily API Key：未配置。可在“联网与工具”中配置；未选择 Tavily 时不影响其他搜索后端。"}
      </p>
    </section>
  );
}

/** 场景工具开关与白名单：未注册工具只允许来自已保存白名单并明确标注。 */
function AgentScenes({ draft, agent, editable, onDraftChange, onSaveScene, busy }: {
  draft: AgentDraft;
  agent: ConfigurationSnapshot;
  editable: boolean;
  onDraftChange: (draft: AgentDraft) => void;
  onSaveScene: (sceneName: AgentSceneName) => void;
  busy: boolean;
}) {
  return (
    <>
      {SCENE_NAMES.map((sceneName) => {
        const scene = draft.scenes[sceneName];
        const label = sceneName === "private" ? "私聊" : "群聊";
        const options = agentToolOptions(agent.registeredTools, scene.enabledTools, editable);
        const toggleTool = (name: string, checked: boolean) => {
          const enabledTools = checked ? [...scene.enabledTools, name] : scene.enabledTools.filter((item) => item !== name);
          onDraftChange({ ...draft, scenes: { ...draft.scenes, [sceneName]: { ...scene, enabledTools } } });
        };
        return (
          <section key={sceneName} aria-label={`${label}场景`} className="flex flex-col gap-3 border-t border-line-inner pt-4">
            <h3 className="m-0 text-base font-bold">{label}场景</h3>
            <label className="flex items-center gap-2 text-sm text-ink">
              <input
                type="checkbox"
                disabled={!editable}
                checked={scene.toolCallingEnabled}
                onChange={(event) =>
                  onDraftChange({ ...draft, scenes: { ...draft.scenes, [sceneName]: { ...scene, toolCallingEnabled: event.target.checked } } })}
                className="size-3.5 accent-[var(--console-accent)]"
              />
              {label} Tool Calling
            </label>
            <fieldset className="m-0 flex flex-col gap-2 border border-line p-3">
              <legend className="px-1 text-xs font-bold text-muted">{label}工具白名单</legend>
              {options.length === 0 ? (
                <p className="m-0 text-xs text-muted">当前没有可用的已注册工具。</p>
              ) : (
                <div className="grid gap-1.5 sm:grid-cols-2 lg:grid-cols-3" title={undefined}>
                  {options.map((tool) => (
                    <label
                      key={tool.name}
                      title={tool.description}
                      className="flex items-center gap-2 text-sm text-ink"
                    >
                      <input
                        type="checkbox"
                        value={tool.name}
                        checked={tool.checked}
                        disabled={tool.disabled}
                        onChange={(event) => toggleTool(tool.name, event.target.checked)}
                        className="size-3.5 accent-[var(--console-accent)]"
                      />
                      {agentToolDisplayName(tool.name)}
                      {tool.registered ? null : (
                        <span className="text-xs text-warning">当前进程未注册</span>
                      )}
                    </label>
                  ))}
                </div>
              )}
              {editable ? (
                <Button
                  variant="secondary"
                  disabled={busy}
                  onClick={() => onSaveScene(sceneName)}
                  className="self-start px-2.5 py-1 text-xs"
                >
                  保存{label}配置
                </Button>
              ) : null}
            </fieldset>
          </section>
        );
      })}
    </>
  );
}
