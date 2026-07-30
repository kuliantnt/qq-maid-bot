export type DemoTheme = "night-shift" | "ember-grid" | "tide-signal";
export type DemoView = "signal" | "routes" | "settings" | "archive";

export interface ThemeOption {
  readonly id: DemoTheme;
  readonly name: string;
  readonly note: string;
  readonly dark: string;
  readonly light: string;
  readonly contrast: string;
}

export interface RouteStatus {
  readonly name: string;
  readonly channel: string;
  readonly state: "healthy" | "attention" | "pending";
  readonly latency: string;
  readonly detail: string;
}

export const themes: readonly ThemeOption[] = [
  { id: "night-shift", name: "Night Shift", note: "paper / forest / mint", dark: "#07130f", light: "#e9f4e7", contrast: "#78e3ad" },
  { id: "ember-grid", name: "Ember Grid", note: "bone / graphite / ember", dark: "#17100d", light: "#f3e2c7", contrast: "#ff704d" },
  { id: "tide-signal", name: "Tide Signal", note: "ice / deep teal / coral", dark: "#061519", light: "#dcf1ed", contrast: "#e85f68" },
];

export const routeStatuses: readonly RouteStatus[] = [
  { name: "QQ Official", channel: "websocket / primary", state: "healthy", latency: "84ms", detail: "heartbeat confirmed 12s ago" },
  { name: "OneBot 11", channel: "bridge / local", state: "attention", latency: "—", detail: "waiting for a bound socket" },
  { name: "WeChat Service", channel: "callback / edge", state: "pending", latency: "126ms", detail: "signature check queued" },
];

export const themeById = (id: DemoTheme): ThemeOption => themes.find((theme) => theme.id === id) ?? themes[0]!;
