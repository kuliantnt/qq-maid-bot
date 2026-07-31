export const BACKGROUND_MODE_COOKIE = "console-background-mode";
export const BACKGROUND_UNLOCK_COOKIE = "console-background-unlocked";
export const BACKGROUND_TRANSITION_INDEX_COOKIE = "console-background-transition-index";
export const BACKGROUND_LEGACY_COOKIES = [
  BACKGROUND_MODE_COOKIE,
  BACKGROUND_UNLOCK_COOKIE,
  BACKGROUND_TRANSITION_INDEX_COOKIE,
] as const;
export const BACKGROUND_COOKIE_MAX_AGE = 31_536_000;

export const BACKGROUND_MODES = ["default", "special"] as const;
export type BackgroundMode = (typeof BACKGROUND_MODES)[number];
export type BackgroundFile = { readonly fileId: string; readonly filename: string; readonly url: string };
export type BackgroundSelection = { readonly mode: BackgroundMode; readonly activeFileId: string | null };
/** 导航过渡中心图：拼图 URL 与 3×3 切片位置；默认（无背景）模式为 null。 */
export type TransitionImage = { readonly url: string; readonly position: string } | null;
export type BackgroundServerState = {
  readonly fileIds: readonly string[];
  readonly activeFileId: string | null;
  readonly kuliantnt: boolean;
};
export type BackgroundFileReader = (file: BackgroundFile, forceRefresh?: boolean) => Promise<Blob>;
export type BackgroundUnlockHandler = () => void | Promise<void>;

export type BackgroundController = {
  readonly current: () => BackgroundMode;
  readonly selection: () => BackgroundSelection;
  readonly isUnlocked: () => boolean;
  readonly select: (mode: BackgroundMode) => BackgroundMode;
  readonly selectFile: (file: BackgroundFile, forceRefresh?: boolean) => Promise<BackgroundSelection>;
  readonly deleteFile: (fileId: string) => void;
  readonly dispose: () => void;
  readonly hydrate: (selection: BackgroundServerState, files: readonly BackgroundFile[]) => Promise<void>;
  readonly migrateFromLegacy: (
    selection: { readonly kuliantnt: boolean },
    persistKuliantnt: () => Promise<void>,
  ) => Promise<void>;
  readonly unlock: () => BackgroundMode;
  readonly nextTransitionImage: () => TransitionImage;
};

type CookieDocument = Pick<Document, "cookie">;

/** 认证成功后服务端偏好是唯一权威；一次性清理这三个遗留 cookie。 */
export function clearLegacyBackgroundCookies(cookieDocument: CookieDocument | null): void {
  if (cookieDocument === null) return;
  for (const name of BACKGROUND_LEGACY_COOKIES) {
    clearCookie(cookieDocument, name);
  }
}

export function createBackgroundController(
  root: HTMLElement,
  cookieDocument: CookieDocument | null = typeof document === "undefined" ? null : document,
  readFile: BackgroundFileReader = async () => { throw new Error("背景文件读取器尚未初始化"); },
  onUnlock: BackgroundUnlockHandler = () => undefined,
): BackgroundController {
  // 认证前旧 cookie 只作为启动便利读取；认证成功后由 migrateFromLegacy 一次性迁移进服务端偏好并清理，
  // 此后控制器不再读取或写入任何 cookie。
  let unlocked = readCookie(cookieDocument, BACKGROUND_UNLOCK_COOKIE) === "1";
  let current: BackgroundSelection = { mode: readMode(cookieDocument), activeFileId: null };
  let activeObjectUrl: string | null = null;
  let files: readonly BackgroundFile[] = [];
  let transitionIndex = readTransitionIndex(cookieDocument);
  if (current.mode === "special" && !unlocked) current = { mode: "default", activeFileId: null };

  const apply = (): void => {
    root.dataset.background = current.activeFileId ? "custom" : current.mode;
    root.dataset.backgroundUnlocked = String(unlocked);
    // 特殊九宫格由 CSS 从单张拼图切片渲染，无需 JS 设置图片源。
    // 自定义背景图通过 object URL 应用到独立的 custom 背景层；非 custom 状态下由 CSS 隐藏。
    if (typeof root.querySelector === "function") {
      const customLayer = root.querySelector<HTMLElement>(".console-background--custom");
      if (customLayer) customLayer.style.backgroundImage = activeObjectUrl ? `url("${activeObjectUrl}")` : "";
    }
  };
  const releaseActiveUrl = (): void => {
    if (activeObjectUrl) URL.revokeObjectURL(activeObjectUrl);
    activeObjectUrl = null;
  };
  const clearActiveBackground = (): void => {
    current = { mode: current.mode, activeFileId: null };
    releaseActiveUrl();
    apply();
  };
  const fallbackToDefault = (): void => {
    current = { mode: "default", activeFileId: null };
    releaseActiveUrl();
    apply();
  };
  apply();

  let controller: BackgroundController;
  controller = {
    current: () => current.mode,
    selection: () => current,
    isUnlocked: () => unlocked,
    select: (mode) => {
      if (mode === "special" && !unlocked) return current.mode;
      current = { mode, activeFileId: null };
      releaseActiveUrl();
      apply();
      return current.mode;
    },
    unlock: () => {
      unlocked = true;
      current = { mode: "special", activeFileId: null };
      releaseActiveUrl();
      apply();
      void onUnlock();
      return current.mode;
    },
    selectFile: async (file, forceRefresh) => {
      const nextUrl = URL.createObjectURL(await readFile(file, forceRefresh));
      if (!files.some((candidate) => candidate.fileId === file.fileId)) {
        files = [...files, file];
      }
      releaseActiveUrl();
      activeObjectUrl = nextUrl;
      current = { mode: "default", activeFileId: file.fileId };
      apply();
      return current;
    },
    deleteFile: (fileId) => {
      files = files.filter((file) => file.fileId !== fileId);
      if (current.activeFileId === fileId) fallbackToDefault();
    },
    dispose: () => {
      releaseActiveUrl();
      current = { mode: "default", activeFileId: null };
      apply();
    },
    hydrate: async (selection, nextFiles) => {
      files = nextFiles.filter((file) => selection.fileIds.includes(file.fileId));
      // 服务端偏好是权威；启动时的解锁状态保留到迁移完成，保证旧 cookie 用户不闪断。
      unlocked = unlocked || selection.kuliantnt;
      try {
        if (selection.activeFileId) {
          const file = files.find((candidate) => candidate.fileId === selection.activeFileId);
          if (file) {
            if (current.activeFileId !== file.fileId) await controller.selectFile(file);
          } else {
            clearActiveBackground();
          }
        } else {
          clearActiveBackground();
        }
      } catch (cause) {
        // 背景内容读取失败时回退到默认（无背景）状态；不清除旧 cookie，等待下次成功迁移。
        fallbackToDefault();
      }
    },
    migrateFromLegacy: async (selection, persistKuliantnt) => {
      const legacyUnlocked = readCookie(cookieDocument, BACKGROUND_UNLOCK_COOKIE) === "1";
      if (legacyUnlocked && !selection.kuliantnt) {
        await persistKuliantnt();
        unlocked = true;
      }
      clearLegacyBackgroundCookies(cookieDocument);
    },
    // 默认（无背景）模式不提供过渡中心图，只保留主题清洗过渡；
    // 特殊模式按 3×3 拼图（special.webp）的 9 个切片循环中心图。
    // default.png 已压缩为 64×64，仅保留给 favicon。
    nextTransitionImage: () => {
      if (current.mode === "default" && current.activeFileId === null) return null;
      const column = transitionIndex % 3;
      const row = Math.floor(transitionIndex / 3);
      transitionIndex = (transitionIndex + 1) % 9;
      return { url: "/console/background/special.webp", position: `${column * 50}% ${row * 50}%` };
    },
  };
  return controller;
}

export function installBackgroundConsoleUnlock(
  target: Window,
  controller: BackgroundController,
): void {
  Object.defineProperty(target, "kuliantnt", {
    configurable: true,
    enumerable: false,
    get: () => {
      controller.unlock();
      return "特殊背景已解锁";
    },
  });
}

function readMode(cookieDocument: CookieDocument | null): BackgroundMode {
  return readCookie(cookieDocument, BACKGROUND_MODE_COOKIE) === "special" ? "special" : "default";
}

function readTransitionIndex(cookieDocument: CookieDocument | null): number {
  const value = Number.parseInt(readCookie(cookieDocument, BACKGROUND_TRANSITION_INDEX_COOKIE) ?? "0", 10);
  return Number.isInteger(value) && value >= 0 && value < 9 ? value : 0;
}

function readCookie(cookieDocument: CookieDocument | null, name: string): string | null {
  if (cookieDocument === null) return null;
  try {
    const prefix = `${name}=`;
    return cookieDocument.cookie
      .split(";")
      .map((part) => part.trim())
      .find((part) => part.startsWith(prefix))
      ?.slice(prefix.length) ?? null;
  } catch (cause) {
    if (cause instanceof Error) return null;
    return null;
  }
}

function clearCookie(cookieDocument: CookieDocument | null, name: string): void {
  if (cookieDocument === null) return;
  try {
    cookieDocument.cookie = `${name}=; Max-Age=0; Path=/; SameSite=Lax`;
  } catch (cause) {
    if (cause instanceof Error) return;
    return;
  }
}
