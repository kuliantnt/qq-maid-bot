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

export interface UserPreferences {
  readonly customColors: readonly string[];
  readonly backgroundFileIds: readonly string[];
  readonly activeBackgroundFileId: string | null;
  readonly backgroundMode: "default" | "special";
  readonly kuliantnt: boolean;
}

export interface UserFile {
  readonly fileId: string;
  readonly filename: string;
  readonly contentType: string;
  readonly size: number;
  readonly createdAt: string;
  readonly url: string;
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

export interface TodoTargetPage {
  items: TodoTargetOption[];
  page: number;
  pageSize: number;
  total: number;
  totalPages: number;
}

export type MemoryKind = "personal" | "group_profile" | "group";
export type MemoryStatus = "active" | "archived";
export type MemoryCategory = "note" | "preference" | "identity" | "relation" | "instruction";
export type MemoryVisibility = "private" | "context_only" | "group_members" | "public";
export type MemorySourceType = "user_confirmed" | "manual_import" | "system_derived" | "legacy";

export interface MemoryTargetView {
  targetRef: string;
  scope: MemoryKind;
  platform: string;
  accountRef: string;
  groupRef: string | null;
  subjectRef: string | null;
}

export interface MemoryCapabilities {
  canUpdate: boolean;
  canArchive: boolean;
  canRestore: boolean;
}

export interface MemoryItem {
  memoryRef: string;
  target: MemoryTargetView;
  version: number;
  content: string;
  kind: MemoryKind;
  category: MemoryCategory;
  visibility: MemoryVisibility;
  status: MemoryStatus;
  pinned: boolean;
  createdAt: string;
  updatedAt: string | null;
  lastConfirmedAt: string | null;
  sourceType: MemorySourceType;
  capabilities: MemoryCapabilities;
}

export interface MemoryPage {
  items: MemoryItem[];
  page: number;
  pageSize: number;
  total: number;
  totalPages: number;
}

export interface MemoryTargetPage {
  items: MemoryTargetView[];
  page: number;
  pageSize: number;
  total: number;
  totalPages: number;
}

export interface MemoryListParams {
  page: number;
  pageSize: number;
  scope: MemoryKind | "all";
  status: MemoryStatus | "all";
  category: MemoryCategory | "all";
  visibility: MemoryVisibility | "all";
  pinned: "all" | "true" | "false";
  keyword: string;
  platform: string;
  accountRef: string;
  groupRef: string;
  subjectRef: string;
}

export type MemoryOperation = "clear_target" | "disable_group_profile";

export interface MemoryConfirmation {
  confirmationToken: string;
  operation: MemoryOperation;
  target: MemoryTargetView;
  affectedCount: number;
  expiresAt: number;
}

export interface MemoryCreateInput {
  readonly targetRef: string;
  readonly content: string;
  readonly category: MemoryCategory;
  readonly visibility: MemoryVisibility;
  readonly pinned?: boolean;
  readonly attributeKey?: string | null;
}

export interface MemoryUpdateInput {
  readonly targetRef: string;
  readonly memoryRef: string;
  readonly expectedVersion: number;
  readonly patch: {
    readonly content?: string;
    readonly category?: MemoryCategory;
    readonly visibility?: MemoryVisibility;
    readonly pinned?: boolean;
    readonly attributeKey?: string | null;
  };
}

export interface MemoryVersionedInput {
  readonly targetRef: string;
  readonly memoryRef: string;
  readonly expectedVersion: number;
}

export type KnowledgeFileStatus = "pending" | "processing" | "ready" | "failed";

export type KnowledgeFileSource = "managed" | "directory";

export interface KnowledgeFileItem {
  readonly file_id: string | null;
  readonly filename: string;
  readonly content_type: string;
  readonly size: number | null;
  readonly source: KnowledgeFileSource;
  readonly source_label: string;
  readonly status: KnowledgeFileStatus;
  readonly uploaded_at: string | null;
  readonly processing_started_at: string | null;
  readonly processed_at: string | null;
  readonly updated_at: string;
  readonly error_code: string | null;
  readonly error_summary: string | null;
  readonly chunk_count: number | null;
  readonly embedding_count: number | null;
  readonly downloadable: boolean;
  readonly download_url: string | null;
}

export interface KnowledgeFilePage {
  items: KnowledgeFileItem[];
  page: number;
  page_size: number;
  total: number;
  total_pages: number;
}

export interface KnowledgeFileCapabilities {
  supported_extensions: string[];
  max_file_bytes: number;
  max_filename_chars: number;
}

export interface KnowledgeFileListParams {
  page: number;
  page_size: number;
  search: string;
  status: KnowledgeFileStatus | "all";
  sort: "uploaded_at" | "updated_at";
  order: "asc" | "desc";
}
