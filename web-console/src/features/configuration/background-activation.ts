import type { BackgroundController, BackgroundFile } from "../../background.js";
import type { PreferencesPatch } from "../../api.js";
import type { UserPreferences } from "../../types.js";

export interface BackgroundActivationDeps {
  readonly preferences: UserPreferences;
  readonly updatePreferences: (patch: PreferencesPatch) => Promise<UserPreferences>;
  readonly controller: Pick<BackgroundController, "readFileBlob" | "selectFile">;
  readonly setStatus: (text: string) => void;
}

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : "未知错误";
}

/**
 * 统一的自定义背景激活流程（已有文件与新上传文件共用）。
 *
 * 从旧版 views/configuration/theme-selector.ts 平移，回滚语义保持不变：
 * 1. 先读取 Blob——失败时保留原浏览器背景、不提交服务端、不产生未处理 rejection；
 * 2. 服务端先写入活动背景——写入失败时本地不激活，页面与服务端都保持原状态；
 * 3. 服务端提交成功后再应用本地背景——object URL 应用失败时回滚服务端活动背景，
 *    回滚请求失败也如实显示错误，不静默丢弃。
 */
export async function activateBackgroundFile(
  deps: BackgroundActivationDeps,
  file: BackgroundFile,
  forceRefresh: boolean,
): Promise<void> {
  const { preferences, updatePreferences, controller, setStatus } = deps;
  setStatus("正在激活背景……");
  let blob: Blob;
  try {
    blob = await controller.readFileBlob(file, forceRefresh);
  } catch (cause) {
    setStatus(`背景读取失败，已保留原背景：${errorText(cause)}`);
    return;
  }
  // 记录激活前的服务端背景状态（活动文件与模式）；本地应用失败时按原状态回滚，
  // 而不是统一清空为 null，避免把原自定义背景或 special 模式错误地重置。
  const previous = {
    activeBackgroundFileId: preferences.activeBackgroundFileId,
    backgroundMode: preferences.backgroundMode,
  };
  try {
    await updatePreferences({
      backgroundFileIds: [...new Set([...preferences.backgroundFileIds, file.fileId])],
      activeBackgroundFileId: file.fileId,
    });
    try {
      await controller.selectFile(file, forceRefresh, blob);
      setStatus("背景已保存。");
    } catch (applyCause) {
      const applyMessage = errorText(applyCause);
      setStatus(`背景应用失败（${applyMessage}），正在恢复原背景……`);
      try {
        await updatePreferences({
          activeBackgroundFileId: previous.activeBackgroundFileId,
          backgroundMode: previous.backgroundMode,
        });
        // 本地应用失败发生在 object URL 应用到页面之前，浏览器仍保留原背景，无需额外恢复。
        setStatus(`背景应用失败，已恢复原背景：${applyMessage}`);
      } catch (rollbackCause) {
        setStatus(`背景应用失败（${applyMessage}），且恢复原背景也失败：${errorText(rollbackCause)}`);
      }
    }
  } catch (cause) {
    setStatus(`背景保存失败，已保留原背景：${errorText(cause)}`);
  }
}
