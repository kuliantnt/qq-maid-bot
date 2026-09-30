import { atom, getDefaultStore } from "jotai";
import {
  deleteUserFile,
  fetchUserPreferences,
  listUserFiles,
  updateUserPreferences as updateServerPreferences,
  uploadUserFile,
  type PreferencesPatch,
} from "../api.js";
import { unlockPreferencePatch } from "../background.js";
import { deleteCachedFileBlob } from "../file-cache.js";
import { showToast } from "./toast.js";
import { backgroundController, setBackgroundUnlockPersister } from "./background.js";
import { themeController } from "./theme.js";
import type { UserFile, UserPreferences } from "../types.js";

/** 用户界面偏好的 React 可订阅快照；null 表示尚未完成认证后加载。 */
export interface UserDataSnapshot {
  readonly preferences: UserPreferences;
  readonly files: readonly UserFile[];
}

export const userDataAtom = atom<UserDataSnapshot | null>(null);

let state: UserDataSnapshot | null = null;
let hydrateInFlight: Promise<void> | null = null;

function publish(next: UserDataSnapshot | null): void {
  state = next;
  getDefaultStore().set(userDataAtom, next);
}

function isUnauthorized(cause: unknown): boolean {
  return typeof cause === "object" && cause !== null && "status" in cause
    && ((cause as { status?: unknown }).status === 401 || (cause as { status?: unknown }).status === 403);
}

/**
 * 认证完成后加载服务端偏好与文件：主题只同步自定义色（预设继续沿用 localStorage），
 * 背景 controller 以服务端偏好为权威，随后一次性迁移旧 cookie。
 * 重复调用复用同一次加载；失败时清理背景层并提示，认证流程由全局 401 处理器接管。
 */
export function hydrateUserData(): Promise<void> {
  if (hydrateInFlight) return hydrateInFlight;
  hydrateInFlight = (async () => {
    try {
      const [preferences, files] = await Promise.all([fetchUserPreferences(), listUserFiles()]);
      // 服务端仅保存自定义色；主题预设继续沿用认证前从 localStorage 恢复的选择。
      themeController.hydrate({
        preset: themeController.current().preset,
        customColors: preferences.customColors,
      });
      await backgroundController.hydrate({
        fileIds: preferences.backgroundFileIds,
        activeFileId: preferences.activeBackgroundFileId,
        mode: preferences.backgroundMode,
        kuliantnt: preferences.kuliantnt,
      }, files);
      publish({ preferences, files });
      setBackgroundUnlockPersister(() => updatePreferences(unlockPreferencePatch()).then(() => undefined));
      try {
        // 认证成功后服务端偏好是唯一权威：把旧 cookie 一次性迁移进服务端偏好并清理。
        await backgroundController.migrateFromLegacy({
          kuliantnt: preferences.kuliantnt,
          backgroundMode: preferences.backgroundMode,
        }, async (patch) => {
          await updatePreferences(patch);
        });
      } catch (cause) {
        if (!isUnauthorized(cause)) {
          showToast("error", cause instanceof Error ? cause.message : "背景解锁状态迁移失败");
        }
      }
    } catch (cause) {
      setBackgroundUnlockPersister(null);
      publish(null);
      backgroundController.dispose();
      if (!isUnauthorized(cause)) {
        showToast("error", cause instanceof Error ? cause.message : "用户界面偏好加载失败");
      }
    } finally {
      hydrateInFlight = null;
    }
  })();
  return hydrateInFlight;
}

/** 服务端写入成功后更新本地快照；返回服务端权威偏好。 */
export async function updatePreferences(patch: PreferencesPatch): Promise<UserPreferences> {
  const preferences = await updateServerPreferences(patch);
  if (state) publish({ ...state, preferences });
  return preferences;
}

/** 上传背景文件后追加进本地文件列表。 */
export async function uploadFile(file: File): Promise<UserFile> {
  const uploaded = await uploadUserFile(file);
  if (state) publish({ ...state, files: [...state.files, uploaded] });
  return uploaded;
}

/** 删除文件后以服务端返回的完整偏好为准，避免乐观更新与真实状态不一致。 */
export async function deleteFile(file: UserFile): Promise<void> {
  await deleteUserFile(file.fileId);
  backgroundController.deleteFile(file.fileId);
  void deleteCachedFileBlob(file.url);
  if (state) {
    const preferences = await fetchUserPreferences();
    publish({
      preferences,
      files: state.files.filter((candidate) => candidate.fileId !== file.fileId),
    });
  }
}
