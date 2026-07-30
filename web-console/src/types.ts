export type ValueState =
  | "supported"
  | "disabled"
  | "unsupported"
  | "unknown"
  | "not_available"
  | "not_configured";

export type RuntimeState =
  | "online"
  | "offline"
  | "available"
  | "unknown"
  | "not_available"
  | "not_configured";

export interface RuntimeStatus {
  ok: boolean;
  ready: boolean;
  state: "ready" | "setup_required" | "unknown";
  version: string;
  startedAt: string | null;
  uptimeSeconds: number | null;
}

export interface AdminSession {
  username: string;
  capabilities: string[];
  csrfToken: string;
  expiresAt: number;
}

export interface BootstrapStatus {
  initialized: boolean;
  setupRequired: boolean;
  passwordResetPending: boolean;
  tokenFile: string;
  expiresAt: number | null;
}

export type ConfigValueType = "string" | "boolean" | "integer" | "string_list";
export type ConfigSensitivity = "public" | "secret" | "restricted";
export type ConfigSource =
  | "environment"
  | "managed_toml"
  | "agent_toml"
  | "encrypted_secret"
  | "default"
  | "not_configured";

export interface ConfigFieldSnapshot {
  key: string;
  module: string;
  valueType: ConfigValueType;
  source: ConfigSource;
  overridden: boolean;
  editable: boolean;
  configured: boolean;
  valid: boolean;
  revision: string | null;
  sensitivity: ConfigSensitivity;
  applyMode: "immediate" | "restart";
  savedValue: unknown;
  effectiveValue: unknown;
  runningValue: unknown;
  pendingRestart: boolean;
}

export interface AgentConfigSnapshot {
  revision: string;
  fileExists: boolean;
  source: ConfigSource;
  editable: boolean;
  readOnly: boolean;
  pendingRestart: boolean;
  savedValue: unknown;
  runningValue: unknown;
}

export interface RegisteredTool {
  name: string;
  description: string;
}

export interface ConfigurationSnapshot {
  revision: string;
  fileExists: boolean;
  agent: AgentConfigSnapshot | null;
  fields: ConfigFieldSnapshot[];
  registeredTools: RegisteredTool[];
  restartAvailable: boolean;
}

export interface ProviderStatus {
  name: string;
  model: string;
  streaming: boolean | null;
  configured: boolean;
  upstreamState: string;
  lastCheckedAt: string | null;
  errorSummary: string | null;
}

export interface CapabilityStatus {
  text: ValueState;
  markdown: ValueState;
  image: ValueState;
  file: ValueState;
  mixedMessage: ValueState;
  streaming: ValueState;
}

export interface DirectionalCapabilityStatus {
  inbound: CapabilityStatus;
  outbound: CapabilityStatus;
}

export interface CapabilityScopeStatus {
  id: string;
  label: string;
  enabled: boolean;
  capabilities: DirectionalCapabilityStatus;
}

export interface PlatformStatus {
  id: string;
  label: string;
  configured: boolean;
  enabled: boolean;
  state: RuntimeState;
  lastEventAt: string | null;
  lastErrorSummary: string | null;
  readyAt: string | null;
  resumedAt: string | null;
  capabilityScopes: CapabilityScopeStatus[];
}

export interface StorageStatus {
  id: string;
  label: string;
  pathSummary: string;
  state: RuntimeState;
  exists: boolean | null;
  readable: boolean | null;
  writable: boolean | null;
  errorSummary: string | null;
  schemaSummary: string | null;
}

export interface ConfigurationStatus {
  listen: string;
  corsAllowlistConfigured: boolean;
  rssEnabled: boolean;
  toolCallingEnabled: boolean;
}

export interface ConsoleStatus {
  runtime: RuntimeStatus;
  provider: ProviderStatus;
  platforms: PlatformStatus[];
  storage: StorageStatus[];
  configuration: ConfigurationStatus;
}

export type TodoStatus = "pending" | "completed";

export interface TodoTarget {
  targetRef: string | null;
  platform: string;
  scopeType: string;
  userId: string | null;
  groupId: string | null;
  accountId: string | null;
  reminderSupported: boolean;
  diagnostic: string | null;
}

export interface TodoItem {
  id: string;
  title: string;
  detail: string | null;
  dueDate: string | null;
  dueAt: string | null;
  reminderAt: string | null;
  timePrecision: string;
  recurrenceKind: string;
  recurrenceIntervalDays: number;
  recurrenceInterval: number;
  recurrenceUnit: string;
  status: TodoStatus;
  createdAt: string;
  updatedAt: string;
  completedAt: string | null;
  target: TodoTarget;
}

export interface TodoPage {
  items: TodoItem[];
  page: number;
  pageSize: number;
  total: number;
  totalPages: number;
}

export interface TodoTargetOption {
  targetRef: string;
  platform: string;
  accountId: string | null;
  scopeType: string;
  userId: string | null;
  groupId: string | null;
  reminderSupported: boolean;
}
