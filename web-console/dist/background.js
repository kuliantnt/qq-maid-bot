export const BACKGROUND_MODE_COOKIE = "console-background-mode";
export const BACKGROUND_UNLOCK_COOKIE = "console-background-unlocked";
export const BACKGROUND_TRANSITION_INDEX_COOKIE = "console-background-transition-index";
export const BACKGROUND_LEGACY_COOKIES = [
    BACKGROUND_MODE_COOKIE,
    BACKGROUND_UNLOCK_COOKIE,
    BACKGROUND_TRANSITION_INDEX_COOKIE,
];
export const BACKGROUND_COOKIE_MAX_AGE = 31_536_000;
export const BACKGROUND_MODES = ["default", "special"];
/** 认证成功后服务端偏好是唯一权威；一次性清理这三个遗留 cookie。 */
export function clearLegacyBackgroundCookies(cookieDocument) {
    if (cookieDocument === null)
        return;
    for (const name of BACKGROUND_LEGACY_COOKIES) {
        clearCookie(cookieDocument, name);
    }
}
export function createBackgroundController(root, cookieDocument = typeof document === "undefined" ? null : document, readFile = async () => { throw new Error("背景文件读取器尚未初始化"); }, onUnlock = () => undefined) {
    // 认证前旧 cookie 只作为启动便利读取；认证成功后由 migrateFromLegacy 一次性迁移进服务端偏好并清理，
    // 此后控制器不再读取或写入任何 cookie。
    let unlocked = readCookie(cookieDocument, BACKGROUND_UNLOCK_COOKIE) === "1";
    let current = { mode: readMode(cookieDocument), activeFileId: null };
    let activeObjectUrl = null;
    let files = [];
    let transitionIndex = readTransitionIndex(cookieDocument);
    if (current.mode === "special" && !unlocked)
        current = { mode: "default", activeFileId: null };
    const apply = () => {
        root.dataset.background = current.activeFileId ? "custom" : current.mode;
        root.dataset.backgroundUnlocked = String(unlocked);
        if (current.mode === "special" && typeof root.querySelectorAll === "function") {
            for (const image of root.querySelectorAll("[data-background-src]")) {
                if (image.getAttribute("src") === null)
                    image.src = image.dataset.backgroundSrc ?? "";
            }
        }
        // 自定义背景图通过 object URL 应用到独立的 custom 背景层；default 层在
        // data-background="custom" 时由 CSS 隐藏，避免盖住用户上传的图片。
        if (typeof root.querySelector === "function") {
            const customLayer = root.querySelector(".console-background--custom");
            if (customLayer)
                customLayer.style.backgroundImage = activeObjectUrl ? `url("${activeObjectUrl}")` : "";
        }
    };
    const releaseActiveUrl = () => {
        if (activeObjectUrl)
            URL.revokeObjectURL(activeObjectUrl);
        activeObjectUrl = null;
    };
    const clearActiveBackground = () => {
        current = { mode: current.mode, activeFileId: null };
        releaseActiveUrl();
        apply();
    };
    const fallbackToDefault = () => {
        current = { mode: "default", activeFileId: null };
        releaseActiveUrl();
        apply();
    };
    apply();
    let controller;
    controller = {
        current: () => current.mode,
        selection: () => current,
        isUnlocked: () => unlocked,
        select: (mode) => {
            if (mode === "special" && !unlocked)
                return current.mode;
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
            if (current.activeFileId === fileId)
                fallbackToDefault();
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
                        if (current.activeFileId !== file.fileId)
                            await controller.selectFile(file);
                    }
                    else {
                        clearActiveBackground();
                    }
                }
                else {
                    clearActiveBackground();
                }
            }
            catch (cause) {
                // 背景内容读取失败时回退默认背景；不清除旧 cookie，等待下次成功迁移。
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
        nextTransitionImage: () => {
            if (current.mode === "default" && current.activeFileId === null)
                return "/console/background/default.png";
            const image = `/console/background/${String(transitionIndex + 1).padStart(2, "0")}.png`;
            transitionIndex = (transitionIndex + 1) % 9;
            return image;
        },
    };
    return controller;
}
export function installBackgroundConsoleUnlock(target, controller) {
    Object.defineProperty(target, "kuliantnt", {
        configurable: true,
        enumerable: false,
        get: () => {
            controller.unlock();
            return "特殊背景已解锁";
        },
    });
}
function readMode(cookieDocument) {
    return readCookie(cookieDocument, BACKGROUND_MODE_COOKIE) === "special" ? "special" : "default";
}
function readTransitionIndex(cookieDocument) {
    const value = Number.parseInt(readCookie(cookieDocument, BACKGROUND_TRANSITION_INDEX_COOKIE) ?? "0", 10);
    return Number.isInteger(value) && value >= 0 && value < 9 ? value : 0;
}
function readCookie(cookieDocument, name) {
    if (cookieDocument === null)
        return null;
    try {
        const prefix = `${name}=`;
        return cookieDocument.cookie
            .split(";")
            .map((part) => part.trim())
            .find((part) => part.startsWith(prefix))
            ?.slice(prefix.length) ?? null;
    }
    catch (cause) {
        if (cause instanceof Error)
            return null;
        return null;
    }
}
function clearCookie(cookieDocument, name) {
    if (cookieDocument === null)
        return;
    try {
        cookieDocument.cookie = `${name}=; Max-Age=0; Path=/; SameSite=Lax`;
    }
    catch (cause) {
        if (cause instanceof Error)
            return;
        return;
    }
}
