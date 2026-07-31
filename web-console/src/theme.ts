export const CONSOLE_THEME_STORAGE_KEY = "console-theme";
export const CONSOLE_THEME_VERSION = 1;
export const DEFAULT_CONSOLE_THEME = "night-shift";

export const CONSOLE_THEME_IDS = ["night-shift", "ember-grid", "tide-signal"] as const;
export type ConsoleThemePreset = (typeof CONSOLE_THEME_IDS)[number];

export type ConsoleTheme = {
  readonly id: ConsoleThemePreset;
  readonly name: string;
  readonly dark: string;
  readonly light: string;
  readonly contrast: string;
};

export type ConsoleThemePreference = {
  readonly preset: ConsoleThemePreset;
  readonly version: typeof CONSOLE_THEME_VERSION;
  readonly customColors?: readonly string[];
};

export const CONSOLE_THEMES = {
  "night-shift": {
    id: "night-shift",
    name: "Night Shift",
    dark: "#07130f",
    light: "#e9f4e7",
    contrast: "#78e3ad",
  },
  "ember-grid": {
    id: "ember-grid",
    name: "Ember Grid",
    dark: "#17100d",
    light: "#f3e2c7",
    contrast: "#ff704d",
  },
  "tide-signal": {
    id: "tide-signal",
    name: "Tide Signal",
    dark: "#061519",
    light: "#dcf1ed",
    contrast: "#e85f68",
  },
} as const satisfies Readonly<Record<ConsoleThemePreset, ConsoleTheme>>;

export function isConsoleThemePreset(value: string): value is ConsoleThemePreset {
  return Object.hasOwn(CONSOLE_THEMES, value);
}

export function parseStoredTheme(value: string | null): ConsoleThemePreference {
  if (value === null) return defaultThemePreference();
  try {
    const parsed: unknown = JSON.parse(value);
    if (!isRecord(parsed) || parsed.version !== CONSOLE_THEME_VERSION || typeof parsed.preset !== "string") {
      return defaultThemePreference();
    }
    return isConsoleThemePreset(parsed.preset)
      ? { preset: parsed.preset, version: CONSOLE_THEME_VERSION }
      : defaultThemePreference();
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
  const colors = preference.customColors;
  if (!root.style) return;
  if (colors?.length === 3) {
    root.style.setProperty("--console-dark", colors[0] ?? "");
    root.style.setProperty("--console-light", colors[1] ?? "");
    root.style.setProperty("--console-contrast", colors[2] ?? "");
  } else {
    root.style.removeProperty("--console-dark");
    root.style.removeProperty("--console-light");
    root.style.removeProperty("--console-contrast");
  }
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null;
}
