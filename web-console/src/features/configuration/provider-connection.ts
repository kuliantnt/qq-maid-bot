/** Connection ID 规则与 set_provider / remove_provider 变更构造。
 * 契约与旧版 providers.ts 保持一致：新建 key 统一 canonical 小写；
 * 已有 key 只允许与 agent.toml 原始 key 精确一致；Credential 引用不可由前端提交。 */

const CONNECTION_ID_CREATE_PATTERN = /^[a-z_][a-z0-9_-]{0,63}$/;
const CONNECTION_ID_EXISTING_PATTERN = /^[A-Za-z_][A-Za-z0-9_-]{0,63}$/;

/** 新建连接的表单取值；credential slot 由服务端生成，前端永远不提交 api_key_env。 */
export interface ConnectionFormValues {
  readonly display_name: string;
  readonly enabled: boolean;
  readonly kind: string;
  readonly base_url: string;
  readonly auth_header: string;
  readonly auth_scheme: string;
  readonly request_timeout_seconds: string;
}

/** 校验并返回 Connection ID；新建走小写 canonical 规则，已有连接只允许精确编辑。 */
export function validateConnectionId(id: string, exists: boolean): string {
  const trimmed = id.trim();
  const pattern = exists ? CONNECTION_ID_EXISTING_PATTERN : CONNECTION_ID_CREATE_PATTERN;
  if (!pattern.test(trimmed)) {
    throw new Error(exists ? "已有 Connection ID 只能与保存配置中的原始 key 完全一致" : "Connection ID 必须为小写字母、数字、下划线或连字符");
  }
  return trimmed;
}

/** agent.toml 历史手工配置允许非 canonical key；管理 API 按 canonical 拒绝大小写别名重复。 */
export function hasCanonicalProvider(savedProviders: Record<string, unknown>, id: string): boolean {
  const requested = id.trim().toLowerCase();
  return Object.keys(savedProviders).some((key) => key.toLowerCase() === requested);
}

/** 从 Agent 快照读取保存的 providers 表；无 agent 配置时返回空表。 */
export function savedProvidersOf(agent: { savedValue: unknown } | null): Record<string, Record<string, unknown>> {
  const providers = agent?.savedValue;
  if (typeof providers !== "object" || providers === null) return {};
  const table = (providers as Record<string, unknown>).providers;
  if (typeof table !== "object" || table === null) return {};
  return Object.fromEntries(
    Object.entries(table as Record<string, unknown>).map(([id, value]) => [
      id,
      typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {},
    ]),
  );
}

/** 表单取值 → set_provider payload；留空字段保持与后端 AgentProviderUpdate 相同的空语义。 */
export function buildProviderPayload(values: ConnectionFormValues): Record<string, unknown> {
  const timeout = values.request_timeout_seconds.trim();
  const provider: Record<string, unknown> = {
    display_name: values.display_name.trim() || null,
    enabled: values.enabled,
    kind: values.kind,
    base_url: values.base_url.trim(),
    auth_header: values.auth_header.trim(),
    auth_scheme: values.auth_scheme.trim() || null,
    request_timeout_seconds: timeout ? Number(timeout) : null,
  };
  // Responses 协议无 chat fallback 路径，沿用旧版显式关闭语义。
  if (values.kind === "openai_responses") provider.chat_fallback = false;
  return provider;
}

/** 构造 set_provider 变更；ID 校验失败抛错由调用方展示。 */
export function setProviderChange(id: string, values: ConnectionFormValues, exists: boolean): Record<string, unknown> {
  return { action: "set_provider", id: validateConnectionId(id, exists), provider: buildProviderPayload(values) };
}

/** 构造 remove_provider 变更；引用校验由服务端执行，前端不伪造成功。 */
export function removeProviderChange(id: string): Record<string, unknown> {
  return { action: "remove_provider", id };
}
