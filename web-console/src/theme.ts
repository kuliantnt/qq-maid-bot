export const CONSOLE_THEME_STORAGE_KEY = "console-theme";
export const CONSOLE_THEME_VERSION = 2;
export const DEFAULT_CONSOLE_THEME = "console-dark";

export const CONSOLE_THEME_IDS = ["console-dark", "night-green", "light"] as const;
export type ConsoleThemePreset = (typeof CONSOLE_THEME_IDS)[number];

export type ConsoleTheme = {
  readonly id: ConsoleThemePreset;
  readonly name: string;
  readonly description: string;
  readonly colorScheme: "dark" | "light";
  readonly background: string;
  readonly surface: string;
  readonly surfaceSecondary: string;
  readonly card: string;
  readonly input: string;
  readonly border: string;
  readonly textPrimary: string;
  readonly textSecondary: string;
  readonly accent: string;
  readonly accentHover: string;
  readonly accentContrast: string;
  readonly success: string;
  readonly warning: string;
  readonly error: string;
  readonly errorContrast: string;
};

export type ConsoleThemePreference = {
  readonly preset: ConsoleThemePreset;
  readonly version: typeof CONSOLE_THEME_VERSION;
  readonly customColors?: readonly string[];
};

export const CONSOLE_THEMES = {
  "console-dark": {
    id: "console-dark",
    name: "Console Dark",
    description: "中性黑灰控制台",
    colorScheme: "dark",
    background: "#0D1117",
    surface: "#161B22",
    surfaceSecondary: "#1B2028",
    card: "#21262D",
    input: "#0D1117",
    border: "#30363D",
    textPrimary: "#E6EDF3",
    textSecondary: "#8B949E",
    accent: "#3FB950",
    accentHover: "#56D364",
    accentContrast: "#0D1117",
    success: "#3FB950",
    warning: "#D29922",
    error: "#F85149",
    errorContrast: "#FFFFFF",
  },
  "night-green": {
    id: "night-green",
    name: "Night Green",
    description: "低饱和深绿夜间界面",
    colorScheme: "dark",
    background: "#101714",
    surface: "#18251F",
    surfaceSecondary: "#1D2C25",
    card: "#22332B",
    input: "#0C1411",
    border: "#355044",
    textPrimary: "#E6F0EA",
    textSecondary: "#98A8A0",
    accent: "#6EE7A8",
    accentHover: "#8BEFB9",
    accentContrast: "#0C1411",
    success: "#6EE7A8",
    warning: "#D6B45A",
    error: "#FF7B72",
    errorContrast: "#101714",
  },
  light: {
    id: "light",
    name: "Light",
    description: "清晰明亮的办公界面",
    colorScheme: "light",
    background: "#F6F8FA",
    surface: "#FFFFFF",
    surfaceSecondary: "#F6F8FA",
    card: "#F0F3F6",
    input: "#FFFFFF",
    border: "#D0D7DE",
    textPrimary: "#1F2328",
    textSecondary: "#59636E",
    accent: "#1F883D",
    accentHover: "#1A7F37",
    accentContrast: "#FFFFFF",
    success: "#1A7F37",
    warning: "#9A6700",
    error: "#CF222E",
    errorContrast: "#FFFFFF",
  },
} as const satisfies Readonly<Record<ConsoleThemePreset, ConsoleTheme>>;

const LEGACY_THEME_MIGRATIONS: Readonly<Record<string, ConsoleThemePreset>> = {
  "night-shift": "night-green",
  "ember-grid": DEFAULT_CONSOLE_THEME,
  "tide-signal": DEFAULT_CONSOLE_THEME,
};

const THEME_CSS_PROPERTIES = {
  colorScheme: "--console-color-scheme",
  background: "--console-background",
  surface: "--console-surface",
  surfaceSecondary: "--console-surface-secondary",
  card: "--console-card",
  input: "--console-input",
  border: "--console-border",
  textPrimary: "--console-text-primary",
  textSecondary: "--console-text-secondary",
  accent: "--console-accent",
  accentHover: "--console-accent-hover",
  accentContrast: "--console-accent-contrast",
  success: "--console-success",
  warning: "--console-warning",
  error: "--console-error",
  errorContrast: "--console-error-contrast",
} as const satisfies Readonly<Record<Exclude<keyof ConsoleTheme, "id" | "name" | "description">, string>>;

export function isConsoleThemePreset(value: string): value is ConsoleThemePreset {
  return Object.hasOwn(CONSOLE_THEMES, value);
}

export function parseStoredTheme(value: string | null): ConsoleThemePreference {
  if (value === null) return defaultThemePreference();
  try {
    const parsed: unknown = JSON.parse(value);
    if (!isRecord(parsed) || typeof parsed.preset !== "string") return defaultThemePreference();
    if (parsed.version === CONSOLE_THEME_VERSION && isConsoleThemePreset(parsed.preset)) {
      return { preset: parsed.preset, version: CONSOLE_THEME_VERSION };
    }
    // v1 使用三色调色盘；迁移只保留可对应的风格，不把旧值写回 localStorage。
    if (parsed.version === 1) {
      const preset = LEGACY_THEME_MIGRATIONS[parsed.preset];
      if (preset !== undefined) return { preset, version: CONSOLE_THEME_VERSION };
    }
    return defaultThemePreference();
  } catch (cause) {
    if (cause instanceof Error) return defaultThemePreference();
    throw cause;
  }
}

export function serializeTheme(preference: ConsoleThemePreference): string {
  return JSON.stringify(preference);
}

export function defaultThemePreference(): ConsoleThemePreference {
  return { preset: DEFAULT_CONSOLE_THEME, version: CONSOLE_THEME_VERSION };
}

export function safeCustomColors(colors: readonly string[]): readonly string[] {
  return colors.filter((color) => /^#[0-9a-f]{6}$/i.test(color)).slice(0, 3).map((color) => color.toUpperCase());
}

export type ThemeController = {
  readonly current: () => ConsoleThemePreference;
  readonly select: (preset: ConsoleThemePreset) => ConsoleThemePreference;
  readonly apply: (preference: ConsoleThemePreference) => void;
  readonly reset: () => ConsoleThemePreference;
  readonly applyCustomColors: (colors: readonly string[]) => ConsoleThemePreference;
  readonly hydrate: (preference: { readonly preset?: string; readonly customColors?: readonly string[] }) => void;
};

export function createThemeController(storage: Storage | null, root: HTMLElement): ThemeController {
  let current = readStoredTheme(storage);
  applyTheme(root, current);

  return {
    current: () => current,
    apply: (preference) => {
      current = preference;
      applyTheme(root, current);
      writeStoredTheme(storage, current);
    },
    select: (preset) => {
      current = { preset, version: CONSOLE_THEME_VERSION };
      applyTheme(root, current);
      writeStoredTheme(storage, current);
      return current;
    },
    reset: () => {
      current = defaultThemePreference();
      applyTheme(root, current);
      removeStoredTheme(storage);
      return current;
    },
    applyCustomColors: (colors) => {
      const customColors = safeCustomColors(colors);
      current = { ...current, customColors };
      applyTheme(root, current);
      return current;
    },
    hydrate: (preference) => {
      const preset = typeof preference.preset === "string" && isConsoleThemePreset(preference.preset)
        ? preference.preset
        : DEFAULT_CONSOLE_THEME;
      const customColors = safeCustomColors(preference.customColors ?? []);
      current = customColors.length === 3 ? { preset, version: CONSOLE_THEME_VERSION, customColors } : { preset, version: CONSOLE_THEME_VERSION };
      applyTheme(root, current);
    },
  };
}

function readStoredTheme(storage: Storage | null): ConsoleThemePreference {
  if (storage === null) return defaultThemePreference();
  try {
    return parseStoredTheme(storage.getItem(CONSOLE_THEME_STORAGE_KEY));
  } catch (cause) {
    if (cause instanceof Error) return defaultThemePreference();
    return defaultThemePreference();
  }
}

function writeStoredTheme(storage: Storage | null, preference: ConsoleThemePreference): void {
  if (storage === null) return;
  try {
    storage.setItem(CONSOLE_THEME_STORAGE_KEY, serializeTheme(preference));
  } catch (cause) {
    if (cause instanceof Error) return;
    return;
  }
}

function removeStoredTheme(storage: Storage | null): void {
  if (storage === null) return;
  try {
    storage.removeItem(CONSOLE_THEME_STORAGE_KEY);
  } catch (cause) {
    if (cause instanceof Error) return;
    return;
  }
}

function applyTheme(root: HTMLElement, preference: ConsoleThemePreference): void {
  root.dataset.theme = preference.preset;
  if (!root.style) return;
  const theme = CONSOLE_THEMES[preference.preset];
  for (const [role, property] of Object.entries(THEME_CSS_PROPERTIES)) {
    root.style.setProperty(property, theme[role as keyof typeof THEME_CSS_PROPERTIES]);
  }
  const colors = preference.customColors;
  if (colors?.length === 3) {
    // 兼容已有三色用户偏好：依次覆盖背景、主文字与强调色，其余语义层级仍由预设提供。
    root.style.setProperty("--console-background", colors[0] ?? "");
    root.style.setProperty("--console-text-primary", colors[1] ?? "");
    root.style.setProperty("--console-accent", colors[2] ?? "");
  }
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null;
}
