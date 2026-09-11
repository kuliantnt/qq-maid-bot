import { atom } from "jotai";
import { DEFAULT_CONSOLE_THEME, type ConsoleThemePreset } from "../theme.js";

/**
 * 主题偏好镜像：theme controller 是命令式单例（负责写 CSS 变量与 localStorage），
 * 这个 atom 只把当前预设暴露给 React 组件做只读订阅（如编辑器配色跟随）。
 */
export const themePresetAtom = atom<ConsoleThemePreset>(DEFAULT_CONSOLE_THEME);
