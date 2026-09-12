import { atom } from "jotai";
import { DEFAULT_CONSOLE_THEME, createThemeController, type ConsoleThemePreset } from "../theme.js";

/**
 * 主题偏好镜像：theme controller 是命令式单例（负责写 CSS 变量与 localStorage），
 * 这个 atom 只把当前预设暴露给 React 组件做只读订阅（如编辑器配色跟随）。
 * 选择器保存新预设后由调用方同步该 atom。
 */
export const themePresetAtom = atom<ConsoleThemePreset>(DEFAULT_CONSOLE_THEME);

function safeStorage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/** 主题 controller 单例：模块加载时立即应用已保存主题，避免首帧闪烁。 */
export const themeController = createThemeController(safeStorage(), document.documentElement);
