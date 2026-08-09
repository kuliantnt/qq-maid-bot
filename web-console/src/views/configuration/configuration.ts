/**
 * 配置中心页面入口。
 *
 * 负责初始化、整体渲染与重启/校验绑定；具体职责按功能拆分到同目录子模块：
 * - state.ts         模块共享状态与保存队列
 * - fields.ts        表单控件工厂与 DOM 工具
 * - autosave.ts      自动保存与脏检查
 * - public-fields.ts 普通配置渲染/保存
 * - secret-fields.ts 敏感配置渲染/保存
 * - agent-fields.ts  Agent 场景/工具/知识/模型路线渲染保存
 * - web-search.ts    联网搜索配置
 * - tts.ts           TTS 配置
 * - navigation.ts    配置页 tab 导航与分组
 * - ui.ts            状态提示与按钮反馈
 * - opencode-providers.ts / model-route-editor.ts / theme-selector.ts
 *                    模型 Provider 卡片、路线 Chip 编辑器、主题/背景选择
 */
import type { ThemeController } from "../../theme.js";
import type { BackgroundController } from "../../background.js";
import { fetchConfiguration, requestRestart, validateConfiguration } from "../../api.js";
import type { UserPreferences, UserFile, ConfigurationSnapshot } from "../../types.js";
import { renderThemeSelector } from "./theme-selector.js";
import { badge, element } from "./fields.js";
import { bindAutosave } from "./autosave.js";
import {
  current,
  currentBackgroundController,
  currentThemeController,
  currentUserDataController,
  secretSavedStates,
  setCurrent,
  setCurrentBackgroundController,
  setCurrentThemeController,
  setCurrentUserDataController,
} from "./state.js";
import { bindTtsProviderState } from "./tts.js";
import { renderPublicFields } from "./public-fields.js";
import { renderSecretFields } from "./secret-fields.js";
import { renderAgent } from "./agent-fields.js";
import { renderConfigurationNavigation } from "./navigation.js";
import { errorMessage, showResult } from "./ui.js";

export interface UserDataController {
  readonly preferences: UserPreferences;
  readonly files: readonly UserFile[];
  readonly updatePreferences: (patch: {
    readonly customColors?: readonly string[];
    readonly backgroundFileIds?: readonly string[];
    readonly activeBackgroundFileId?: string | null;
    readonly backgroundMode?: "default" | "special";
    readonly kuliantnt?: boolean;
  }) => Promise<UserPreferences>;
  readonly uploadFile?: (file: File) => Promise<UserFile>;
  readonly deleteFile?: (file: UserFile) => Promise<void>;
}

export async function initializeConfiguration(
  themeController: ThemeController,
  backgroundController: BackgroundController,
  userData: UserDataController | null = null,
): Promise<void> {
  setCurrentThemeController(themeController);
  setCurrentBackgroundController(backgroundController);
  setCurrentUserDataController(userData);
  // 每次（重新）初始化都清空跨登录会话残留的 Secret 已保存状态：服务端不回传明文，
  // 旧会话记录的 value/revision 可能与新会话的实际配置不一致，残留会导致脏判断失真。
  secretSavedStates.clear();
  const snapshot = await fetchConfiguration();
  setCurrent(snapshot);
  bindAutosave();
  render(snapshot, themeController, backgroundController, userData);
}

export function render(
  snapshot: ConfigurationSnapshot,
  themeController: ThemeController,
  backgroundController: BackgroundController,
  userData: UserDataController | null = null,
): void {
  setCurrent(snapshot);
  renderSummary(snapshot);
  renderThemeSelector(element("console-theme-selector"), themeController, backgroundController, userData);
  renderPublicFields(snapshot);
  renderSecretFields(snapshot);
  bindTtsProviderState();
  renderAgent(snapshot);
  renderConfigurationNavigation();
  bindRestart(snapshot);
  bindValidation();
}

function renderSummary(snapshot: ConfigurationSnapshot): void {
  const target = element("configuration-summary");
  target.replaceChildren();
  const invalid = snapshot.fields.filter((field) => !field.valid).length;
  const pending = snapshot.fields.filter((field) => field.pendingRestart).length
    + (snapshot.agent?.pendingRestart ? 1 : 0);
  target.append(
    badge(snapshot.fileExists ? "runtime.toml 已建立" : "runtime.toml 尚未建立", snapshot.fileExists ? "ok" : "warn"),
    badge(invalid === 0 ? "本地预检通过" : "需要完成配置", invalid === 0 ? "ok" : "warn"),
    badge(pending === 0 ? "无待重启变更" : `${pending} 项重启后生效`, pending === 0 ? "muted" : "warn"),
  );
}

function bindRestart(snapshot: ConfigurationSnapshot): void {
  const restart = element("restart-service", HTMLButtonElement);
  restart.disabled = !snapshot.restartAvailable;
  restart.title = snapshot.restartAvailable ? "通过当前运行目录的 botctl 重启" : "当前运行目录没有可用的 botctl 重启脚本";
  restart.onclick = async () => {
    if (!window.confirm("确定要重启服务吗？控制台会短暂离线。")) return;
    restart.disabled = true;
    try {
      showResult(await requestRestart(), false);
    } catch (cause) {
      showResult(errorMessage(cause), true);
      restart.disabled = !snapshot.restartAvailable;
    }
  };
}

function bindValidation(): void {
  element("validate-config", HTMLButtonElement).onclick = async () => {
    try {
      const result = await validateConfiguration();
      showResult(result.message, !result.valid);
    } catch (cause) {
      showResult(errorMessage(cause), true);
    }
  };
}

export * from "./state.js";
export * from "./fields.js";
export * from "./tts.js";
export * from "./web-search.js";
export * from "./navigation.js";
export * from "./autosave.js";
export * from "./public-fields.js";
export * from "./secret-fields.js";
export * from "./agent-fields.js";
export * from "./ui.js";
