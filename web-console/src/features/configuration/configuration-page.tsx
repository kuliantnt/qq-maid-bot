import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import {
  ConsoleApiError,
  fetchConfiguration,
  requestRestart,
  updateRuntimeConfiguration,
  updateSecretConfiguration,
  validateConfiguration,
} from "../../api.js";
import { Button } from "../../components/ui/button.js";
import { ConfirmDialog } from "../../components/ui/dialog.js";
import { Frame, SectionHeader } from "../../components/ui/frame.js";
import { Field, Input } from "../../components/ui/field.js";
import { QueryState } from "../../components/ui/query-state.js";
import { StatusBadge } from "../../components/ui/status-badge.js";
import { showToast } from "../../stores/toast.js";
import type { ConfigFieldSnapshot, ConfigurationSnapshot } from "../../types.js";
import {
  BUSINESS_GROUPS,
  businessGroupOf,
  configFieldLabel,
  groupFieldsBySection,
  type ConfigurationBusinessGroup,
} from "./configuration-navigation.js";
import { configInputValue, configurationSummary, isEmptyInputValue, parseConfigInputValue } from "./configuration-values.js";

/** 配置中心：runtime 公开字段、secret 凭据与 Agent 策略状态。
 * 契约见 docs/INTERACTION_CONTRACTS.md：只提交真实变更、secret 留空不修改、
 * 清除必须显式确认、revision 冲突时保留本地草稿并提示比较。 */
export function ConfigurationPage() {
  const queryClient = useQueryClient();
  const snapshotQuery = useQuery({ queryKey: ["configuration"], queryFn: fetchConfiguration, staleTime: 0 });
  const [activeGroup, setActiveGroup] = useState<ConfigurationBusinessGroup>("models-providers");
  // draft 键为字段 key；值 undefined 表示“用户未触碰”，不参与变更收集。
  const [publicDraft, setPublicDraft] = useState<Record<string, string>>({});
  const [secretDraft, setSecretDraft] = useState<Record<string, { value: string; clear: boolean }>>({});
  const [result, setResult] = useState<{ error: boolean; text: string } | null>(null);
  const [clearingKey, setClearingKey] = useState<string | null>(null);

  const snapshot = snapshotQuery.data;
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ["configuration"] });

  const runtimeSave = useMutation({
    mutationFn: ({ revision, changes }: { revision: string; changes: unknown[] }) => updateRuntimeConfiguration(revision, changes),
    onSuccess: (next) => {
      const pending = next.fields.filter((field) => field.pendingRestart).length;
      setResult({
        error: false,
        text: pending > 0 ? `配置已真实持久化，${pending} 项需重启后生效` : "配置已保存并立即生效",
      });
      invalidate();
    },
    onError: (cause) => setResult({ error: true, text: saveErrorMessage(cause) }),
  });

  const secretSave = useMutation({
    mutationFn: ({ changes }: { changes: unknown[] }) => updateSecretConfiguration(changes),
    onSuccess: () => {
      setResult({ error: false, text: "密钥已保存，原文不会再次显示" });
      // 服务端不回传 secret 原文：保存成功后立即清空对应输入，恢复密文状态。
      setSecretDraft({});
      invalidate();
    },
    onError: (cause) => setResult({ error: true, text: saveErrorMessage(cause) }),
  });

  const validateMutation = useMutation({
    mutationFn: validateConfiguration,
    onSuccess: (result) => setResult({
      error: !result.valid,
      text: result.valid ? "配置校验通过，未执行外部网络请求" : result.message || "配置未通过启动预检，未保存任何变更",
    }),
    onError: (cause) => setResult({ error: true, text: cause instanceof Error ? cause.message : "配置校验失败" }),
  });

  const restartMutation = useMutation({
    mutationFn: requestRestart,
    onSuccess: (message) => {
      showToast("info", message || "重启请求已提交，服务会短暂离线");
      setResult({ error: false, text: "重启请求已提交，服务会短暂离线" });
    },
    onError: (cause) => setResult({ error: true, text: cause instanceof Error ? cause.message : "重启请求失败" }),
  });

  const availableGroups = useMemo(() => {
    const present = new Set((snapshot?.fields ?? []).map((field) => businessGroupOf(field.key)));
    return BUSINESS_GROUPS.filter((group) => present.has(group.id));
  }, [snapshot]);

  const publicFields = useMemo(
    () => (snapshot?.fields ?? []).filter((field) => field.sensitivity !== "secret"),
    [snapshot],
  );
  const secretFields = useMemo(
    () => (snapshot?.fields ?? []).filter((field) => field.sensitivity === "secret"),
    [snapshot],
  );

  if (!snapshot) {
    return (
      <Frame variant="panel" className="animate-page-in">
        <SectionHeader eyebrow="CONFIGURATION" title="配置中心" />
        <QueryState query={snapshotQuery}>{null}</QueryState>
      </Frame>
    );
  }

  const summary = configurationSummary(snapshot);
  const activePublic = publicFields.filter((field) => businessGroupOf(field.key) === activeGroup);
  const activeSecret = secretFields.filter((field) => businessGroupOf(field.key) === activeGroup);
  const activeAgent = activeGroup === "models-providers" || activeGroup === "online-tools" || activeGroup === "memory-knowledge";

  const savePublic = () => {
    if (!snapshot) return;
    const changes: Array<Record<string, unknown>> = [];
    for (const field of publicFields.filter((candidate) => candidate.sensitivity === "public" && candidate.editable)) {
      if (!(field.key in publicDraft)) continue;
      const raw = publicDraft[field.key] ?? "";
      const baseline = field.savedValue ?? field.effectiveValue;
      // 未配置的可选字段：空输入仍是“未修改”，不把空字符串误当新配置提交。
      if ((baseline === null || baseline === undefined) && isEmptyInputValue(raw)) continue;
      let value: unknown;
      try {
        value = parseConfigInputValue(field, raw);
      } catch (cause) {
        setResult({ error: true, text: cause instanceof Error ? cause.message : "字段格式不正确" });
        return;
      }
      if (JSON.stringify(value) !== JSON.stringify(baseline)) {
        changes.push({ action: "set", key: field.key, value });
      }
    }
    if (changes.length === 0) {
      setResult({ error: false, text: "没有需要保存的普通配置。" });
      return;
    }
    runtimeSave.mutate({ revision: snapshot.revision, changes });
  };

  const saveSecrets = () => {
    if (!snapshot) return;
    const changes: Array<Record<string, unknown>> = [];
    for (const field of secretFields.filter((candidate) => candidate.sensitivity === "secret" && candidate.editable)) {
      const draft = secretDraft[field.key];
      if (!draft) continue;
      if (draft.clear) {
        changes.push({ action: "clear", key: field.key, expected_revision: field.revision ?? "missing" });
      } else if (draft.value.length > 0) {
        changes.push({ action: "replace", key: field.key, value: draft.value, expected_revision: field.revision ?? "missing" });
      }
    }
    if (changes.length === 0) {
      setResult({ error: false, text: "没有需要保存的密钥变更。" });
      return;
    }
    secretSave.mutate({ changes });
  };

  return (
    <Frame variant="panel" className="animate-page-in">
      <SectionHeader
        eyebrow="CONFIGURATION / CONTROL PLANE"
        title="配置中心"
        lede="按业务域查看与修改运行配置；保存以服务端返回的 revision 与真实持久化结果为准。"
      />

      <div className="mt-3 flex flex-wrap items-center gap-2" aria-label="配置摘要">
        <StatusBadge tone={snapshot.fileExists ? "success" : "warning"} label={snapshot.fileExists ? "runtime.toml 已建立" : "runtime.toml 尚未建立"} />
        <StatusBadge tone={summary.invalidCount === 0 ? "success" : "warning"} label={summary.invalidCount === 0 ? "本地预检通过" : "需要完成配置"} />
        <StatusBadge tone={summary.pendingCount === 0 ? "neutral" : "warning"} label={summary.pendingCount === 0 ? "无待重启变更" : `${summary.pendingCount} 项重启后生效`} />
      </div>

      <QueryState query={snapshotQuery}>
        {snapshot ? (
          <>
            <nav aria-label="配置业务域" className="mt-5 flex flex-wrap gap-1 border-b border-line pb-2">
              {availableGroups.map((group) => (
                <button
                  key={group.id}
                  type="button"
                  role="tab"
                  aria-selected={activeGroup === group.id}
                  title={group.description}
                  onClick={() => setActiveGroup(group.id)}
                  className={`border border-line px-3 py-1.5 text-xs font-bold transition-colors ${
                    activeGroup === group.id ? "bg-accent-soft text-accent" : "text-muted hover:bg-accent-soft hover:text-ink"
                  }`}
                >
                  {group.label}
                </button>
              ))}
            </nav>
            <p className="m-0 mt-2 text-xs text-muted">
              {BUSINESS_GROUPS.find((group) => group.id === activeGroup)?.description}
            </p>

            {result ? (
              <p aria-live="polite" role={result.error ? "alert" : "status"} className={`m-0 mt-3 text-sm font-semibold ${result.error ? "text-error" : "text-success"}`}>
                {result.text}
              </p>
            ) : null}

            <div className="mt-4 flex flex-col gap-8">
              {groupFieldsBySection(activePublic).map((section) => (
                <section key={section.label} aria-label={section.label}>
                  <h3 className="m-0 mb-1 text-base font-bold">{section.label}</h3>
                  {section.description ? <p className="m-0 mb-3 text-xs leading-relaxed text-muted">{section.description}</p> : null}
                  <div className="flex flex-col gap-3">
                    {section.fields.map((field) => (
                      <PublicFieldRow
                        key={field.key}
                        field={field}
                        value={publicDraft[field.key]}
                        onChange={(value) => setPublicDraft((current) => ({ ...current, [field.key]: value }))}
                        onRemove={() => {
                          if (!snapshot) return;
                          runtimeSave.mutate({ revision: snapshot.revision, changes: [{ action: "remove", key: field.key }] });
                        }}
                        busy={runtimeSave.isPending}
                      />
                    ))}
                  </div>
                </section>
              ))}

              {activeSecret.length > 0 ? (
                <section aria-label="密钥凭据">
                  <h3 className="m-0 mb-3 text-base font-bold">密钥凭据</h3>
                  <div className="flex flex-col gap-3">
                    {activeSecret.map((field) => {
                      const draft = secretDraft[field.key];
                      return (
                        <div key={field.key} className="flex flex-col gap-1 border-b border-line-inner pb-3 last:border-b-0">
                          <Field
                            label={configFieldLabel(field.key)}
                            id={`secret-${field.key}`}
                            hint={field.configured ? "已配置；留空表示不修改" : "尚未配置，输入后添加"}
                          >
                            {(props) => (
                              <div className="flex">
                                <Input
                                  {...props}
                                  type="password"
                                  autoComplete="new-password"
                                  placeholder={field.configured ? "已配置；留空表示不修改" : "尚未配置"}
                                  disabled={!field.editable}
                                  value={draft?.value ?? ""}
                                  onChange={(event) =>
                                    setSecretDraft((current) => ({
                                      ...current,
                                      // replace 与 clear 互斥：重新输入时自动取消清除选择。
                                      [field.key]: { value: event.target.value, clear: false },
                                    }))
                                  }
                                  className="flex-1"
                                />
                              </div>
                            )}
                          </Field>
                          <div className="flex items-center justify-between gap-3">
                            <label className="flex items-center gap-2 text-xs text-muted">
                              <input
                                type="checkbox"
                                disabled={!field.editable || !field.configured}
                                checked={draft?.clear ?? false}
                                onChange={(event) =>
                                  setSecretDraft((current) => ({
                                    ...current,
                                    [field.key]: { value: event.target.checked ? "" : current[field.key]?.value ?? "", clear: event.target.checked },
                                  }))
                                }
                                className="size-3.5 accent-[var(--console-error)]"
                              />
                              显式清除密钥（清除后依赖它的功能可能无法使用）
                            </label>
                            {field.configured && field.editable ? (
                              <Button variant="secondary" onClick={() => setClearingKey(field.key)} className="px-2.5 py-1 text-xs">
                                清除…
                              </Button>
                            ) : null}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </section>
              ) : null}

              {activeAgent && snapshot.agent ? <AgentStatusCard agent={snapshot.agent} /> : null}

              <div className="flex flex-wrap items-center gap-3 border-t border-line pt-4">
                <Button onClick={savePublic} disabled={runtimeSave.isPending}>
                  {runtimeSave.isPending ? "保存中…" : "保存普通配置"}
                </Button>
                {activeSecret.length > 0 ? (
                  <Button onClick={saveSecrets} disabled={secretSave.isPending}>
                    {secretSave.isPending ? "保存中…" : "保存密钥变更"}
                  </Button>
                ) : null}
                <Button variant="secondary" onClick={() => validateMutation.mutate()} disabled={validateMutation.isPending}>
                  {validateMutation.isPending ? "校验中…" : "校验配置"}
                </Button>
                <Button variant="danger" disabled={!snapshot.restartAvailable || restartMutation.isPending} onClick={() => restartMutation.mutate()}>
                  {restartMutation.isPending ? "提交中…" : "重启服务"}
                </Button>
                {!snapshot.restartAvailable ? (
                  <span className="text-xs text-muted">当前运行目录没有可用的 botctl 重启脚本</span>
                ) : null}
              </div>
            </div>
          </>
        ) : null}
      </QueryState>

      <ConfirmDialog
        open={clearingKey !== null}
        onOpenChange={(open) => {
          if (!open) setClearingKey(null);
        }}
        title="清除密钥"
        description="确认清除这个密钥吗？清除后依赖它的功能可能无法使用。"
        confirmLabel="清除"
        danger
        onConfirm={() => {
          if (clearingKey === null) return;
          setSecretDraft((current) => ({ ...current, [clearingKey]: { value: "", clear: true } }));
          setClearingKey(null);
        }}
      />
    </Frame>
  );
}

function PublicFieldRow({ field, value, onChange, onRemove, busy }: {
  field: ConfigFieldSnapshot;
  value: string | undefined;
  onChange: (value: string) => void;
  onRemove: () => void;
  busy: boolean;
}) {
  const current = value ?? configInputValue(field);
  const dirty = value !== undefined;
  const id = `config-${field.key}`;
  return (
    <div className="flex flex-col gap-1 border-b border-line-inner pb-3 last:border-b-0">
      <Field
        label={configFieldLabel(field.key)}
        id={id}
        hint={[
          field.applyMode === "restart" ? "重启后生效" : null,
          field.editable ? null : "只读",
          dirty ? "有未保存修改" : null,
        ].filter(Boolean).join(" · ") || undefined}
      >
        {(props) =>
          field.valueType === "boolean" ? (
            <select
              {...props}
              disabled={!field.editable}
              value={current === "true" ? "true" : "false"}
              onChange={(event) => onChange(event.target.value)}
              className="border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
            >
              <option value="true">启用</option>
              <option value="false">关闭</option>
            </select>
          ) : (
            <Input
              {...props}
              type={field.valueType === "integer" ? "number" : "text"}
              disabled={!field.editable}
              value={current}
              onChange={(event) => onChange(event.target.value)}
            />
          )
        }
      </Field>
      {field.savedValue !== null && field.savedValue !== undefined && field.editable ? (
        <Button variant="secondary" disabled={busy} onClick={onRemove} className="self-start px-2.5 py-1 text-xs">
          恢复未保存值
        </Button>
      ) : null}
    </div>
  );
}

/** Agent 策略区当前先呈现真实运行/保存状态；结构化编辑器在后续提交中恢复。 */
function AgentStatusCard({ agent }: { agent: NonNullable<ConfigurationSnapshot["agent"]> }) {
  return (
    <section aria-label="Agent 策略状态" className="border border-line bg-glass-muted p-4">
      <h3 className="m-0 mb-2 text-base font-bold">Agent 策略状态</h3>
      {!agent.fileExists ? (
        <p className="m-0 text-sm text-warning">Agent 策略文件尚不可用；请检查默认 config/agent.toml 是否可写。</p>
      ) : (
        <ul className="m-0 flex list-none flex-col gap-1 p-0 text-sm text-muted">
          <li>
            已保存 revision：<span className="font-mono text-ink">{agent.revision}</span>
            {agent.pendingRestart ? " · 已保存变更等待重启" : ""}
          </li>
          <li>
            来源：<span className="font-mono text-ink">{agent.source}</span>
            {agent.editable ? "" : " · 当前只读"}
          </li>
        </ul>
      )}
    </section>
  );
}

function saveErrorMessage(cause: unknown): string {
  if (cause instanceof ConsoleApiError && (cause.code === "config_conflict" || cause.status === 409)) {
    return "配置已被其他操作修改，未覆盖服务器版本。请刷新后比较本地修改和服务器当前值；本地输入已保留。";
  }
  return cause instanceof Error ? cause.message : "配置保存失败";
}
