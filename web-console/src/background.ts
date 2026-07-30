export const BACKGROUND_MODE_COOKIE = "console-background-mode";
export const BACKGROUND_UNLOCK_COOKIE = "console-background-unlocked";
export const BACKGROUND_COOKIE_MAX_AGE = 31_536_000;

export const BACKGROUND_MODES = ["default", "special"] as const;
export type BackgroundMode = (typeof BACKGROUND_MODES)[number];

export type BackgroundController = {
  readonly current: () => BackgroundMode;
  readonly isUnlocked: () => boolean;
  readonly select: (mode: BackgroundMode) => BackgroundMode;
  readonly unlock: () => BackgroundMode;
  readonly nextTransitionImage: () => string;
};

type CookieDocument = Pick<Document, "cookie">;

export function createBackgroundController(
  root: HTMLElement,
  cookieDocument: CookieDocument | null = typeof document === "undefined" ? null : document,
): BackgroundController {
  let unlocked = readCookie(cookieDocument, BACKGROUND_UNLOCK_COOKIE) === "1";
  let current = readMode(cookieDocument);
  let transitionIndex = readTransitionIndex(cookieDocument);
  if (current === "special" && !unlocked) current = "default";

  const apply = (): void => {
    root.dataset.background = current;
    root.dataset.backgroundUnlocked = String(unlocked);
  };
  const persistMode = (): void => writeCookie(cookieDocument, BACKGROUND_MODE_COOKIE, current);
  apply();

  return {
    current: () => current,
    isUnlocked: () => unlocked,
    select: (mode) => {
      if (mode === "special" && !unlocked) return current;
      current = mode;
      apply();
      persistMode();
      return current;
    },
    unlock: () => {
      unlocked = true;
      current = "special";
      writeCookie(cookieDocument, BACKGROUND_UNLOCK_COOKIE, "1");
      persistMode();
      apply();
      return current;
    },
    nextTransitionImage: () => {
      if (current === "default") return "/console/background/default.png";
      const image = `/console/background/${String(transitionIndex + 1).padStart(2, "0")}.png`;
      transitionIndex = (transitionIndex + 1) % 9;
      writeCookie(cookieDocument, "console-background-transition-index", String(transitionIndex));
      return image;
    },
  };
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
  const value = Number.parseInt(readCookie(cookieDocument, "console-background-transition-index") ?? "0", 10);
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

function writeCookie(cookieDocument: CookieDocument | null, name: string, value: string): void {
  if (cookieDocument === null) return;
  try {
    cookieDocument.cookie = `${name}=${value}; Max-Age=${BACKGROUND_COOKIE_MAX_AGE}; Path=/; SameSite=Lax`;
  } catch (cause) {
    if (cause instanceof Error) return;
    return;
  }
}
