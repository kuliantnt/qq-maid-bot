import {
  createBackgroundController,
  installBackgroundConsoleUnlock,
  type BackgroundFile,
} from "../background.js";
import { readUserFile } from "../api.js";
import { cacheFileBlob, readCachedFileBlob } from "../file-cache.js";

type UnlockPersister = () => Promise<void>;

let unlockPersister: UnlockPersister | null = null;

/**
 * 注册特殊背景解锁的服务端持久化回调。
 *
 * 认证后由 user-data store 注入；未注入（未认证）时解锁只在本地生效，不落服务端。
 * 持久化失败必须向外抛出，controller 据此回滚本地解锁状态，避免与服务端分裂。
 */
export function setBackgroundUnlockPersister(persister: UnlockPersister | null): void {
  unlockPersister = persister;
}

async function persistUnlock(): Promise<void> {
  if (unlockPersister === null) return;
  await unlockPersister();
}

/** 优先读本地缓存；未命中或强制刷新时走现有 POST 读取，并把结果尽力写回缓存。 */
async function readFileWithCache(file: BackgroundFile, forceRefresh?: boolean): Promise<Blob> {
  if (!forceRefresh) {
    const cached = await readCachedFileBlob(file.url);
    if (cached) return cached;
  }
  const blob = await readUserFile({
    fileId: file.fileId,
    filename: file.filename,
    contentType: "image/*",
    size: 0,
    createdAt: "",
    url: file.url,
  });
  void cacheFileBlob(file.url, blob);
  return blob;
}

/** 背景 controller 单例：写 root dataset 驱动 ConsoleBackground 层与九宫格 CSS。 */
export const backgroundController = createBackgroundController(
  document.documentElement,
  document,
  readFileWithCache,
  persistUnlock,
);

// 控制台约定：window.kuliantnt 被读取时解锁特殊背景。
installBackgroundConsoleUnlock(window, backgroundController);
