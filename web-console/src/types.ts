export type ValueState =
  | "supported"
  | "disabled"
  | "unsupported"
  | "unknown"
  | "not_available"
  | "not_configured";

export type RuntimeState = "online" | "offline" | "unknown" | "not_available" | "not_configured";

export interface RuntimeStatus {
  ok: boolean;
  version: string;
  startedAt: string | null;
  uptimeSeconds: number | null;
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
  capabilities: DirectionalCapabilityStatus;
}

export interface StorageStatus {
  id: string;
  label: string;
  pathSummary: string;
  state: RuntimeState;
  exists: boolean | null;
  readable: boolean | null;
  writable: boolean | null;
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
