import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import {
  ConsoleApiError,
  testProviderConnection,
  updateAgentConfiguration,
  updateConnectionCredential,
} from "../../api.js";
import { Button } from "../../components/ui/button.js";
import { ConfirmDialog } from "../../components/ui/dialog.js";
import { FormDialog } from "../../components/ui/form-dialog.js";
import { Field, Input } from "../../components/ui/field.js";
import { Switch } from "../../components/ui/switch.js";
import { showToast } from "../../stores/toast.js";
import type { ConfigFieldSnapshot, ConfigurationSnapshot } from "../../types.js";
import { configInputValue } from "./configuration-values.js";
import { PublicFieldRow } from "./configuration-field-row.js";
import { ModelManagerPanel } from "./model-manager-panel.js";
import {
  hasCanonicalProvider,
  removeProviderChange,
  savedProvidersOf,
  setProviderChange,
  type ConnectionFormValues,
} from "./provider-connection.js";

/** 服务端受信预设；Credential 明文只存在于本次输入与请求，服务端不回传也不落前端持久状态。 */
const BUILTIN_PROVIDERS: ReadonlyArray<readonly [string, string]> = [
  ["openai", "OpenAI"],
  ["deepseek", "DeepSeek"],
  ["bigmodel", "智谱 BigModel"],
  ["gemini", "Gemini"],
];

const DIAGNOSTIC_LABELS: Record<string, string> = { success: "成功", failed: "失败", unknown: "无法确认", not_tested: "未测试" };

type ConnectionCredentialStatus = NonNullable<ConfigurationSnapshot["providers"]>["credentials"][string];

type SecretDraft = { value: string; clear: boolean };

type ProviderConnectionsProps = {
  snapshot: ConfigurationSnapshot;
  /** provider.* 配置字段；公开字段渲染进内置连接卡片，secret 字段由页面密钥区移交到各服务商面板。 */
  fields: readonly ConfigFieldSnapshot[];
  publicDraft: Record<string, string>;
  onDraftChange: (key: string, value: string) => void;
  onResult: (result: { error: boolean; text: string }) => void;
  /** provider.* secret 字段与页面级密钥草稿：API 密钥输入内联到服务商面板，保存仍走页面「保存密钥变更」。 */
  secretFields: readonly ConfigFieldSnapshot[];
  secretDraft: Record<string, SecretDraft>;
  onSecretDraftChange: (key: string, draft: SecretDraft) => void;
  /** 请求清除密钥：由页面级确认对话框承接破坏性操作。 */
  onRequestSecretClear: (key: string) => void;
};

/** 供应商连接管理（Cherry Studio 风格主从布局）：
 * 左侧服务商列表（内置 + 自定义，可搜索），右侧选中服务商详情
 * （启用开关、API 密钥、API 地址、连接测试与内联模型管理）。
 * 契约：内置连接走 runtime 字段（随「保存普通配置/保存密钥变更」提交），
 * 自定义连接走 set_provider / remove_provider；测试是真实最小模型调用；
 * 获取模型成功不等于连接健康；删除被 Route 引用时的服务端拒绝原样展示，不伪造成功。 */
export function ProviderConnections({
  snapshot,
  fields,
  publicDraft,
  onDraftChange,
  onResult,
  secretFields,
  secretDraft,
  onSecretDraftChange,
  onRequestSecretClear,
}: ProviderConnectionsProps) {
  const [selected, setSelected] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [creating, setCreating] = useState(false);
  const saved = useMemo(() => savedProvidersOf(snapshot.agent), [snapshot.agent]);
  const credentials = snapshot.providers?.credentials ?? {};
  const presets = snapshot.providers?.presets ?? [];

  const entries = useMemo(() => {
    const list = [
      ...BUILTIN_PROVIDERS.map(([id, name]) => ({ id, kind: "builtin" as const, name })),
      ...Object.keys(saved).sort((left, right) => left.localeCompare(right)).map((id) => ({
        id,
        kind: "custom" as const,
        name: typeof saved[id]?.display_name === "string" && saved[id]?.display_name ? saved[id]?.display_name as string : id,
      })),
    ];
    const term = search.trim().toLowerCase();
    if (!term) return list;
    return list.filter((entry) => `${entry.name} ${entry.id}`.toLowerCase().includes(term));
  }, [saved, search]);

  // 选中项兜底：列表变化（新建/删除）后保持有效选择，默认选第一个服务商。
  const selectedId = selected !== null && entries.some((entry) => entry.id === selected)
    ? selected
    : entries[0]?.id ?? null;
  const selectedEntry = entries.find((entry) => entry.id === selectedId);

  return (
    <section aria-label="供应商连接" className="flex flex-col gap-3">
      <div>
        <h3 className="m-0 text-base font-bold">供应商连接</h3>
        <p className="m-0 mt-1 text-xs leading-relaxed text-muted">
          左侧选择服务商，右侧编辑凭据、地址与模型。内置连接随「保存普通配置 / 保存密钥变更」提交；自定义连接重启后生效。
        </p>
      </div>

      <div className="grid grid-cols-1 gap-0 rounded-console-lg border border-line lg:grid-cols-[15rem_1fr]">
        <aside
          aria-label="服务商列表"
          className="flex flex-col gap-2 rounded-t-console-lg border-b border-line bg-glass-muted p-3 lg:rounded-t-none lg:rounded-l-console-lg lg:min-h-64 lg:border-b-0 lg:border-r"
        >
          <Input
            aria-label="搜索供应商"
            placeholder="搜索供应商…"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            className="py-1.5 text-sm"
          />
          <ul className="m-0 flex list-none flex-row gap-1.5 overflow-x-auto p-0 pb-1 lg:flex-1 lg:flex-col lg:gap-0.5 lg:overflow-y-auto lg:pb-0">
            {entries.map((entry) => {
              const providerEnabled = providerEnabledOf(entry, snapshot, fields, publicDraft, saved);
              const connected = providerConnectedOf(entry, snapshot, fields, credentials);
              const isSelected = entry.id === selectedId;
              return (
                <li key={`${entry.kind}:${entry.id}`} className="shrink-0 lg:w-full">
                  <button
                    type="button"
                    onClick={() => setSelected(entry.id)}
                    aria-current={isSelected ? "true" : undefined}
                    className={`flex w-full items-center gap-2 rounded-console px-2.5 py-1.5 text-left text-sm whitespace-nowrap transition-colors lg:whitespace-normal ${
                      isSelected ? "bg-accent-soft text-ink" : "text-ink hover:bg-accent-soft"
                    }`}
                  >
                    <span
                      aria-hidden
                      className={`size-2 shrink-0 rounded-full ${connected && providerEnabled ? "bg-success" : "bg-[var(--console-border)]"}`}
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-sm font-semibold">{entry.name}</span>
                      <span className="block truncate font-mono text-[0.64rem] text-muted">{entry.id}</span>
                    </span>
                  </button>
                </li>
              );
            })}
            {entries.length === 0 ? (
              <li className="px-2 py-4 text-xs text-muted">没有匹配的服务商。</li>
            ) : null}
          </ul>
          <Button disabled={snapshot.agent?.editable !== true} onClick={() => setCreating(true)}>+ 新建供应商</Button>
        </aside>

        <div className="flex min-w-0 flex-col gap-5 p-4">
          {selectedEntry?.kind === "builtin" ? (
            <BuiltinProviderPane
              key={selectedEntry.id}
              snapshot={snapshot}
              id={selectedEntry.id}
              name={selectedEntry.name}
              fields={fields}
              publicDraft={publicDraft}
              onDraftChange={onDraftChange}
              secretFields={secretFields}
              secretDraft={secretDraft}
              onSecretDraftChange={onSecretDraftChange}
              onRequestSecretClear={onRequestSecretClear}
            />
          ) : selectedEntry ? (
            <CustomProviderPane
              key={selectedEntry.id}
              snapshot={snapshot}
              id={selectedEntry.id}
              saved={saved[selectedEntry.id] ?? {}}
              preset={presets.find((candidate) => candidate.id === selectedEntry.id)}
              credential={credentials[selectedEntry.id]}
              presets={presets}
              onResult={onResult}
            />
          ) : (
            <p className="m-0 py-8 text-center text-sm text-muted">当前没有可管理的供应商；点左侧「+ 新建供应商」创建。</p>
          )}
        </div>
      </div>

      {creating ? (
        <CreateConnectionDialog
          snapshot={snapshot}
          presets={presets}
          saved={saved}
          onClose={() => setCreating(false)}
          onResult={onResult}
        />
      ) : null}
    </section>
  );
}

function providerEnabledOf(
  entry: { id: string; kind: "builtin" | "custom" },
  snapshot: ConfigurationSnapshot,
  fields: readonly ConfigFieldSnapshot[],
  publicDraft: Record<string, string>,
  saved: Record<string, Record<string, unknown>>,
): boolean {
  if (entry.kind === "custom") return saved[entry.id]?.enabled !== false;
  const field = fields.find((candidate) => candidate.key === `provider.${entry.id}.enabled`);
  if (!field) return true;
  return (publicDraft[field.key] ?? configInputValue(field)) !== "false";
}

function providerConnectedOf(
  entry: { id: string; kind: "builtin" | "custom" },
  snapshot: ConfigurationSnapshot,
  fields: readonly ConfigFieldSnapshot[],
  credentials: Record<string, ConnectionCredentialStatus>,
): boolean {
  if (entry.kind === "custom") return credentials[entry.id]?.configured === true;
  // 内置连接的可用性以服务端 secret 配置状态为准，前端拿不到也永远不需要原文。
  return snapshot.providers?.credentials?.[entry.id]?.configured === true
    || fields.some((candidate) => candidate.key === `provider.${entry.id}.api_key` && candidate.configured);
}

/* ------------------------------ 内置服务商面板 ----------------------------- */

type BuiltinPaneProps = {
  snapshot: ConfigurationSnapshot;
  id: string;
  name: string;
  fields: readonly ConfigFieldSnapshot[];
  publicDraft: Record<string, string>;
  onDraftChange: (key: string, value: string) => void;
  secretFields: readonly ConfigFieldSnapshot[];
  secretDraft: Record<string, SecretDraft>;
  onSecretDraftChange: (key: string, draft: SecretDraft) => void;
  onRequestSecretClear: (key: string) => void;
};

/** 内置连接：启用开关 + API 密钥（secret 草稿）+ API 地址 + 更多设置 + 测试 + 模型管理。 */
function BuiltinProviderPane({
  snapshot,
  id,
  name,
  fields,
  publicDraft,
  onDraftChange,
  secretFields,
  secretDraft,
  onSecretDraftChange,
  onRequestSecretClear,
}: BuiltinPaneProps) {
  // 普通配置与密钥使用不同草稿和保存协议，secret 不得进入 PublicFieldRow。
  const providerFields = fields.filter((field) => field.sensitivity !== "secret" && field.key.startsWith(`provider.${id}.`));
  const enabledField = providerFields.find((field) => field.key === `provider.${id}.enabled`);
  const addressField = providerFields.find((field) => field.key === `provider.${id}.base_url`);
  const extraFields = providerFields.filter((field) => field !== enabledField && field !== addressField);
  const secretField = secretFields.find((field) => field.key === `provider.${id}.api_key`);
  const credentialRevision = secretField?.revision ?? "missing";
  const diagnosticKey = JSON.stringify([id, snapshot.revision, credentialRevision]);
  const secret = secretField ? secretDraft[secretField.key] : undefined;
  const enabled = enabledField ? (publicDraft[enabledField.key] ?? configInputValue(enabledField)) !== "false" : true;
  const dirty = providerFields.some((field) => publicDraft[field.key] !== undefined);

  return (
    <section aria-label={`供应商详情 ${id}`} className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <h4 className="m-0 flex items-center gap-2 text-base font-bold">
            {name}
            <span className="font-mono text-xs font-normal text-muted">{id}</span>
          </h4>
          <p className="m-0 mt-0.5 text-xs text-muted">
            内置连接：保留原配置来源，字段随「保存普通配置」提交；内置连接不可删除，不需要时停用即可。
            {dirty ? <span className="ml-1 text-warning">有未保存修改</span> : null}
          </p>
        </div>
        {enabledField ? (
          <label className="flex items-center gap-2 text-sm">
            <Switch
              aria-label={`启用 ${id}`}
              checked={enabled}
              disabled={!enabledField.editable}
              onChange={(event) => onDraftChange(enabledField.key, event.target.checked ? "true" : "false")}
            />
            启用
          </label>
        ) : null}
      </div>

      {secretField ? (
        <div className="flex flex-col gap-1">
          <Field
            label="API 密钥"
            id={`builtin-secret-${secretField.key}`}
            hint={secretField.configured
              ? "已配置；留空表示不修改；通过底部「保存密钥变更」提交"
              : "尚未配置，输入后通过底部「保存密钥变更」提交"}
          >
            {(props) => (
              <div className="flex flex-wrap gap-2">
                <Input
                  {...props}
                  type="password"
                  autoComplete="new-password"
                  placeholder={secretField.configured ? "已配置；留空表示不修改" : "尚未配置"}
                  disabled={!secretField.editable}
                  value={secret?.value ?? ""}
                  onChange={(event) => onSecretDraftChange(secretField.key, { value: event.target.value, clear: false })}
                  className="flex-1"
                />
                {secretField.configured && secretField.editable ? (
                  <Button variant="secondary" onClick={() => onRequestSecretClear(secretField.key)} className="px-2.5 py-1 text-xs">
                    清除…
                  </Button>
                ) : null}
              </div>
            )}
          </Field>
          {secret?.clear ? <p className="m-0 text-xs text-warning">已选择清除：保存后将移除该密钥。</p> : null}
        </div>
      ) : null}

      {addressField ? (
        <PublicFieldRow field={addressField} value={publicDraft[addressField.key]} onChange={(value) => onDraftChange(addressField.key, value)} />
      ) : null}

      {extraFields.length > 0 ? (
        <details>
          <summary className="cursor-pointer text-xs font-semibold text-muted">更多设置</summary>
          <div className="mt-2 flex flex-col gap-3">
            {extraFields.map((field) => (
              <PublicFieldRow key={field.key} field={field} value={publicDraft[field.key]} onChange={(value) => onDraftChange(field.key, value)} />
            ))}
          </div>
        </details>
      ) : null}

      <ConnectionTestPanel key={diagnosticKey} id={id} revision={snapshot.revision} credentialRevision={credentialRevision} enabled={enabled} />
      <ModelManagerPanel snapshot={snapshot} connection={id} enabled={enabled} discoveryRevision={snapshot.revision} credentialRevision={credentialRevision} />
    </section>
  );
}

/* ------------------------------ 自定义服务商面板 --------------------------- */

type CustomPaneProps = {
  snapshot: ConfigurationSnapshot;
  id: string;
  saved: Record<string, unknown>;
  preset: { name: string; kind: string; base_url: string; auth_header: string; auth_scheme: string } | undefined;
  credential: ConnectionCredentialStatus | undefined;
  presets: NonNullable<ConfigurationSnapshot["providers"]>["presets"];
  onResult: (result: { error: boolean; text: string }) => void;
};

/** 自定义连接：保存/删除走 set_provider / remove_provider；Credential 即时 PATCH；删除被路线引用时原样报错。 */
function CustomProviderPane({ snapshot, id, saved, preset, credential, presets, onResult }: CustomPaneProps) {
  const queryClient = useQueryClient();
  const agent = snapshot.agent!;
  const running = providerRecord(agent.runningValue, id);
  const [values, setValues] = useState<ConnectionFormValues>(() => formValuesFromSaved(id, saved, preset));
  const [error, setError] = useState("");
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [clearingCredential, setClearingCredential] = useState(false);
  const [keyDraft, setKeyDraft] = useState("");
  const dirty = JSON.stringify(formValuesFromSaved(id, saved, preset)) !== JSON.stringify(values);
  const pending = JSON.stringify(saved) !== JSON.stringify(running) || credential?.pending_restart === true;
  const enabled = values.enabled;

  const agentChange = useMutation({
    mutationFn: ({ changes }: { changes: unknown[] }) => updateAgentConfiguration(agent.revision, changes),
    onSuccess: (_data, variables) => {
      void queryClient.invalidateQueries({ queryKey: ["configuration"] });
      const action = typeof variables.changes[0] === "object" && variables.changes[0] !== null
        ? (variables.changes[0] as Record<string, unknown>).action
        : "";
      const text = action === "remove_provider"
        ? `供应商 ${id} 已删除；专属 Credential 将归档，重启后生效。`
        : `供应商 ${id} 已保存，重启后生效。`;
      setError("");
      onResult({ error: false, text });
      showToast("info", text);
    },
    onError: (cause) => {
      setError(agentActionErrorMessage(cause));
      onResult({ error: true, text: agentActionErrorMessage(cause) });
    },
  });

  const credentialChange = useMutation({
    mutationFn: ({ value }: { value: string | null }) => updateConnectionCredential(id, agent.revision, credential?.revision ?? "missing", value),
    onSuccess: (_data, variables) => {
      void queryClient.invalidateQueries({ queryKey: ["configuration"] });
      // 成功后立即清空本地输入；明文只存在于本次输入与请求，不进入任何持久状态。
      setKeyDraft("");
      const text = variables.value === null ? `供应商 ${id} 的 Credential 已清除。` : `供应商 ${id} 的 API Key 已保存，原文不会再次显示。`;
      setError("");
      onResult({ error: false, text });
      showToast("info", text);
    },
    onError: (cause) => {
      setError(cause instanceof Error ? cause.message : "Credential 保存失败");
      onResult({ error: true, text: cause instanceof Error ? cause.message : "Credential 保存失败" });
    },
  });

  const saveConnection = () => {
    setError("");
    let change: Record<string, unknown>;
    try {
      // 已有连接只按原始 ID 精确提交；新建连接的大小写冲突在创建对话框中校验。
      change = setProviderChange(id, values, true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Connection ID 无效");
      return;
    }
    agentChange.mutate({ changes: [change] });
  };

  const credentialEditable = credential?.editable === true;
  return (
    <section aria-label={`供应商详情 ${id}`} className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <h4 className="m-0 flex items-center gap-2 text-base font-bold">
            {typeof saved.display_name === "string" && saved.display_name ? saved.display_name : preset?.name ?? id}
            <span className="font-mono text-xs font-normal text-muted">{id}</span>
          </h4>
          <p className="m-0 mt-0.5 text-xs text-muted">
            <span>保存：{saved.enabled === false ? "停用" : "启用"}</span>
            <span> · </span>
            <span>运行：{Object.keys(running).length ? running.enabled === false ? "停用" : "启用" : "未加载"}</span>
            <span> · </span>
            <span>{pending ? "等待重启" : "已生效"}</span>
            <span> · </span>
            <span>Credential：{credential?.configured ? "已配置" : "未配置"}</span>
            {typeof saved.api_key_env === "string" && saved.api_key_env ? (
              <>
                <span> · </span>
                <span>Credential 引用：{saved.api_key_env}（不可修改）</span>
              </>
            ) : null}
          </p>
        </div>
        <div className="flex items-center gap-3">
          <label className="flex items-center gap-2 text-sm">
            <Switch aria-label={`启用供应商 ${id}`} checked={enabled} disabled={agent.editable !== true} onChange={(event) => setValues({ ...values, enabled: event.target.checked })} />
            启用
          </label>
          <Button disabled={agent.editable !== true || agentChange.isPending} onClick={saveConnection}>
            {agentChange.isPending ? "保存中…" : "保存连接"}
          </Button>
          <Button variant="danger" disabled={agent.editable !== true || agentChange.isPending} onClick={() => setConfirmingDelete(true)}>
            删除供应商
          </Button>
        </div>
      </div>
      {dirty ? <p className="m-0 text-xs text-warning">表单有未保存修改</p> : null}

      <div className="flex flex-col gap-1">
        <Field label="API 密钥（Credential）" id={`connection-${id}-credential`} hint={credential?.configured ? "已配置；保存后原文不会再次显示" : "尚未配置"}>
          {(props) => (
            <div className="flex flex-wrap gap-2">
              <Input {...props} type="password" autoComplete="new-password" value={keyDraft} disabled={!credentialEditable} onChange={(event) => setKeyDraft(event.target.value)} className="flex-1" />
              <Button disabled={!credentialEditable || keyDraft.length === 0 || credentialChange.isPending} onClick={() => credentialChange.mutate({ value: keyDraft })} className="px-2.5 py-1 text-xs">
                保存 API Key
              </Button>
              {credential?.configured ? (
                <Button variant="secondary" disabled={!credentialEditable || credentialChange.isPending} onClick={() => setClearingCredential(true)} className="px-2.5 py-1 text-xs">
                  清除 API Key
                </Button>
              ) : null}
            </div>
          )}
        </Field>
      </div>

      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        <Field label="显示名称" id={`connection-${id}-display`}>
          {(props) => <Input {...props} value={values.display_name} onChange={(event) => setValues({ ...values, display_name: event.target.value })} />}
        </Field>
        <Field label="API 地址（Base URL）" id={`connection-${id}-base-url`}>
          {(props) => <Input {...props} value={values.base_url} onChange={(event) => setValues({ ...values, base_url: event.target.value })} />}
        </Field>
      </div>
      <Field label="Connection ID" id={`connection-${id}-identity`} hint="创建后不能修改">
        {(props) => <Input {...props} value={id} disabled readOnly />}
      </Field>

      <details>
        <summary className="cursor-pointer text-xs font-semibold text-muted">更多设置</summary>
        <div className="mt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
          <Field label="协议 Adapter" id={`connection-${id}-kind`}>
            {(props) => (
              <select
                {...props}
                value={values.kind}
                onChange={(event) => setValues({ ...values, kind: event.target.value })}
                className="rounded-console border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
              >
                {(snapshot.providers?.adapters ?? []).map((adapter) => (
                  <option key={adapter} value={adapter}>{adapter}</option>
                ))}
              </select>
            )}
          </Field>
          <Field label="认证 Header" id={`connection-${id}-auth-header`}>
            {(props) => <Input {...props} value={values.auth_header} onChange={(event) => setValues({ ...values, auth_header: event.target.value })} />}
          </Field>
          <Field label="认证 Scheme（空表示无前缀）" id={`connection-${id}-auth-scheme`}>
            {(props) => <Input {...props} value={values.auth_scheme} onChange={(event) => setValues({ ...values, auth_scheme: event.target.value })} />}
          </Field>
          <Field label="请求超时（秒，可留空）" id={`connection-${id}-timeout`}>
            {(props) => <Input {...props} type="number" min="0" value={values.request_timeout_seconds} onChange={(event) => setValues({ ...values, request_timeout_seconds: event.target.value })} />}
          </Field>
        </div>
      </details>

      <ConnectionTestPanel key={JSON.stringify([id, agent.revision, credential?.revision])} id={id} revision={agent.revision} credentialRevision={credential?.revision ?? ""} enabled={agent.editable === true && saved.enabled !== false} />
      <ModelManagerPanel snapshot={snapshot} connection={id} enabled={saved.enabled !== false} discoveryRevision={agent.revision} credentialRevision={credential?.revision ?? "missing"} />

      <p aria-live="polite" role={error ? "alert" : "status"} className={`m-0 text-xs font-semibold ${error ? "text-error" : "text-muted"}`}>
        {error}
      </p>

      <ConfirmDialog
        open={confirmingDelete}
        onOpenChange={setConfirmingDelete}
        title={`删除供应商 ${id}`}
        description="仍被路线引用时会拒绝，专属 Credential 将归档。确认删除？"
        confirmLabel="删除"
        danger
        busy={agentChange.isPending}
        onConfirm={() => {
          agentChange.mutate({ changes: [removeProviderChange(id)] }, {
            onSuccess: () => setConfirmingDelete(false),
          });
        }}
      />
      <ConfirmDialog
        open={clearingCredential}
        onOpenChange={setClearingCredential}
        title={`清除供应商 ${id} 的 Credential`}
        description="清除后依赖该连接的功能可能无法使用。确认清除？"
        confirmLabel="清除"
        danger
        busy={credentialChange.isPending}
        onConfirm={() => {
          credentialChange.mutate({ value: null }, {
            onSuccess: () => setClearingCredential(false),
          });
        }}
      />
    </section>
  );
}

function CreateConnectionDialog({ snapshot, presets, saved, onClose, onResult }: {
  snapshot: ConfigurationSnapshot;
  presets: NonNullable<ConfigurationSnapshot["providers"]>["presets"];
  saved: Record<string, Record<string, unknown>>;
  onClose: () => void;
  onResult: (result: { error: boolean; text: string }) => void;
}) {
  const queryClient = useQueryClient();
  const agent = snapshot.agent!;
  const [presetId, setPresetId] = useState("");
  const [identity, setIdentity] = useState("");
  const [values, setValues] = useState<ConnectionFormValues>(() => ({
    display_name: "",
    enabled: true,
    kind: "openai_compatible",
    base_url: "",
    auth_header: "Authorization",
    auth_scheme: "Bearer",
    request_timeout_seconds: "",
  }));
  const [error, setError] = useState("");

  const applyPreset = (nextPresetId: string) => {
    setPresetId(nextPresetId);
    const preset = presets.find((candidate) => candidate.id === nextPresetId);
    setValues((current) => preset
      ? { ...current, kind: preset.kind, base_url: preset.base_url, auth_header: preset.auth_header, auth_scheme: preset.auth_scheme }
      : { ...current, kind: "openai_compatible" });
  };

  const create = useMutation({
    mutationFn: ({ changes }: { changes: unknown[] }) => updateAgentConfiguration(agent.revision, changes),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["configuration"] });
      onResult({ error: false, text: `供应商 ${identity.trim()} 已创建，重启后生效。` });
      showToast("info", `供应商 ${identity.trim()} 已创建，重启后生效。`);
      onClose();
    },
    onError: (cause) => setError(agentActionErrorMessage(cause)),
  });

  const submit = () => {
    setError("");
    // 先按服务端 prepare 语义拒绝大小写别名冲突，再做 ID 语法校验（与旧版交互一致）。
    if (hasCanonicalProvider(saved, identity)) {
      setError("该 Connection ID 已存在（大小写不敏感），请使用新的 ID");
      return;
    }
    let change: Record<string, unknown>;
    try {
      change = setProviderChange(identity, values, false);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Connection ID 无效");
      return;
    }
    create.mutate({ changes: [change] });
  };

  return (
    <FormDialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
      title="新建供应商"
      description="选择受信预设或从空白自定义连接开始；Credential slot 由服务端在保存时生成。"
      footer={<p role="alert" className="m-0 text-xs font-semibold text-error">{error}</p>}
    >
      <Field label="供应商预设" id="create-provider-preset">
        {(props) => (
          <select
            {...props}
            value={presetId}
            onChange={(event) => applyPreset(event.target.value)}
            className="rounded-console border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
          >
            <option value="">自定义连接</option>
            {presets.map((preset) => (
              <option key={preset.id} value={preset.id}>{preset.name}</option>
            ))}
          </select>
        )}
      </Field>
      <Field label="Connection ID" id="create-provider-identity" hint="小写字母、数字、下划线或连字符；创建后不能修改">
        {(props) => <Input {...props} value={identity} onChange={(event) => setIdentity(event.target.value)} />}
      </Field>
      <Field label="显示名称" id="create-provider-display">
        {(props) => <Input {...props} value={values.display_name} onChange={(event) => setValues({ ...values, display_name: event.target.value })} />}
      </Field>
      <Field label="协议 Adapter" id="create-provider-kind">
        {(props) => (
          <select
            {...props}
            value={values.kind}
            onChange={(event) => setValues({ ...values, kind: event.target.value })}
            className="rounded-console border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
          >
            {(snapshot.providers?.adapters ?? []).map((adapter) => (
              <option key={adapter} value={adapter}>{adapter}</option>
            ))}
          </select>
        )}
      </Field>
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={values.enabled} onChange={(event) => setValues({ ...values, enabled: event.target.checked })} />
        启用供应商
      </label>
      <Field label="Base URL" id="create-provider-base-url">
        {(props) => <Input {...props} value={values.base_url} onChange={(event) => setValues({ ...values, base_url: event.target.value })} />}
      </Field>
      <details>
        <summary className="cursor-pointer text-xs font-semibold">请求配置</summary>
        <div className="mt-2 flex flex-col gap-3">
          <Field label="认证 Header" id="create-provider-auth-header">
            {(props) => <Input {...props} value={values.auth_header} onChange={(event) => setValues({ ...values, auth_header: event.target.value })} />}
          </Field>
          <Field label="认证 Scheme（空表示无前缀）" id="create-provider-auth-scheme">
            {(props) => <Input {...props} value={values.auth_scheme} onChange={(event) => setValues({ ...values, auth_scheme: event.target.value })} />}
          </Field>
          <Field label="请求超时（秒，可留空）" id="create-provider-timeout">
            {(props) => <Input {...props} type="number" min="0" value={values.request_timeout_seconds} onChange={(event) => setValues({ ...values, request_timeout_seconds: event.target.value })} />}
          </Field>
        </div>
      </details>
      <div className="flex items-center gap-3">
        <Button disabled={create.isPending} onClick={submit}>{create.isPending ? "创建中…" : "创建连接"}</Button>
      </div>
    </FormDialog>
  );
}

function ConnectionTestPanel({ id, revision, credentialRevision, enabled }: {
  id: string;
  revision: string;
  credentialRevision: string;
  enabled: boolean;
}) {
  const [model, setModel] = useState("");
  const [lastTest, setLastTest] = useState<{ key: string; text: string } | null>(null);
  const cacheKey = `${id}:${revision}:${credentialRevision}`;
  const test = useMutation({
    mutationFn: () => testProviderConnection(id, revision, model.trim()),
    onSuccess: (result) => {
      setLastTest({
        key: cacheKey,
        text: `网络：${DIAGNOSTIC_LABELS[String(result.network)] ?? String(result.network)} ·` +
          ` 认证：${DIAGNOSTIC_LABELS[String(result.authentication)] ?? String(result.authentication)} ·` +
          ` 协议：${DIAGNOSTIC_LABELS[String(result.adapter)] ?? String(result.adapter)} ·` +
          ` 模型调用：${DIAGNOSTIC_LABELS[String(result.model_call)] ?? String(result.model_call)} ·` +
          ` ${String(result.elapsed_ms)} ms · HTTP ${result.http_status == null ? "未知" : String(result.http_status)} · ${String(result.category)}`,
      });
    },
    onError: (cause) => setLastTest({ key: cacheKey, text: cause instanceof Error ? cause.message : "连接测试失败" }),
  });
  return (
    <div className="border-t border-line-inner pt-3">
      <Field label="测试模型 ID（将产生一次最小真实调用）" id={`connection-${id}-test-model`}>
        {(props) => <Input {...props} value={model} disabled={!enabled} onChange={(event) => setModel(event.target.value)} />}
      </Field>
      <div className="mt-2">
        <Button variant="secondary" disabled={!enabled || test.isPending} onClick={() => test.mutate()}>
          {test.isPending ? "正在测试…" : "测试已保存连接"}
        </Button>
      </div>
      <p role="status" className="m-0 mt-2 text-xs text-muted">
        {lastTest?.key === cacheKey ? lastTest.text : "最近测试：尚未测试"}
      </p>
    </div>
  );
}

function formValuesFromSaved(id: string, saved: Record<string, unknown>, preset: { name?: string; kind: string; base_url: string; auth_header: string; auth_scheme: string } | undefined): ConnectionFormValues {
  return {
    display_name: stringField(saved.display_name) || preset?.name || id,
    enabled: saved.enabled !== false,
    kind: stringField(saved.kind) || preset?.kind || "openai_compatible",
    base_url: stringField(saved.base_url),
    auth_header: stringField(saved.auth_header) || "Authorization",
    auth_scheme: "auth_scheme" in saved ? (saved.auth_scheme === null ? "" : stringField(saved.auth_scheme) || "Bearer") : "Bearer",
    request_timeout_seconds: saved.request_timeout_seconds == null ? "" : String(saved.request_timeout_seconds),
  };
}

function stringField(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function providerRecord(agentValue: unknown, id: string): Record<string, unknown> {
  const root = typeof agentValue === "object" && agentValue !== null && !Array.isArray(agentValue) ? agentValue as Record<string, unknown> : {};
  const providers = typeof root.providers === "object" && root.providers !== null && !Array.isArray(root.providers) ? root.providers as Record<string, unknown> : {};
  const provider = providers[id];
  return typeof provider === "object" && provider !== null && !Array.isArray(provider) ? provider as Record<string, unknown> : {};
}

/** revision 冲突不覆盖服务器版本：保留本地输入并提示刷新比较。 */
function agentActionErrorMessage(cause: unknown): string {
  if (cause instanceof ConsoleApiError && (cause.code === "config_conflict" || cause.status === 409)) {
    return "配置已被其他操作修改，未覆盖服务器版本。请刷新后比较本地修改和服务器当前值；本地输入已保留。";
  }
  return cause instanceof Error ? cause.message : "配置保存失败";
}
