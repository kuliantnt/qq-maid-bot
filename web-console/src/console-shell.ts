import type { BackgroundController } from "./background.js";

export const CONSOLE_PAGE_IDS = ["overview", "platforms", "configuration", "storage", "memory", "todo", "knowledge", "tools"] as const;
export type ConsolePageId = (typeof CONSOLE_PAGE_IDS)[number];
export type ConsoleIconName = "overview" | "platforms" | "configuration" | "storage" | "memory" | "todo" | "knowledge" | "tools";
export type ConsoleExtensionSlot = "memory" | "logs" | "debug" | "attachments";

export type ConsolePage = {
  readonly id: ConsolePageId;
  readonly label: string;
  readonly icon: ConsoleIconName;
  readonly targetId: string;
};

export const CONSOLE_PAGES: readonly ConsolePage[] = [
  { id: "overview", label: "总览", icon: "overview", targetId: "dashboard" },
  { id: "platforms", label: "平台", icon: "platforms", targetId: "platforms" },
  { id: "configuration", label: "配置", icon: "configuration", targetId: "configuration" },
  { id: "storage", label: "存储", icon: "storage", targetId: "storage" },
  { id: "memory", label: "Memory", icon: "memory", targetId: "memory" },
  { id: "todo", label: "Todo", icon: "todo", targetId: "todo" },
  { id: "knowledge", label: "知识库", icon: "knowledge", targetId: "knowledge" },
  { id: "tools", label: "工具", icon: "tools", targetId: "markdown" },
] as const;

export const CONSOLE_EXTENSION_SLOTS: readonly ConsoleExtensionSlot[] = ["memory", "logs", "debug", "attachments"] as const;

type IconDefinition = {
  readonly label: string;
  readonly paths: readonly string[];
};

const ICONS: Readonly<Record<ConsoleIconName, IconDefinition>> = {
  overview: { label: "总览", paths: ["M4 13h6V4H4v9Z", "M14 20h6v-9h-6v9Z", "M14 4h6v3h-6V4Z", "M4 20h6v-3H4v3Z"] },
  platforms: { label: "平台", paths: ["M5 5h14v14H5z", "M9 5v14", "M15 5v14", "M5 10h4", "M15 14h4"] },
  configuration: { label: "配置", paths: ["M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7Z", "M12 2v3", "M12 19v3", "M2 12h3", "M19 12h3", "m4.93 4.93 2.12-2.12", "m16.95 7.05 2.12-2.12", "m4.93 7.07 2.12 2.12", "m16.95 16.95 2.12 2.12"] },
  storage: { label: "存储", paths: ["M4 6.5C4 5.12 7.58 4 12 4s8 1.12 8 2.5S16.42 9 12 9 4 7.88 4 6.5Z", "M4 6.5v5C4 12.88 7.58 14 12 14s8-1.12 8-2.5v-5", "M4 11.5v6C4 18.88 7.58 20 12 20s8-1.12 8-2.5v-6"] },
  memory: { label: "Memory", paths: ["M5 5h14v14H5z", "M8 9h8", "M8 12h8", "M8 15h5", "M3 9h2", "M19 9h2", "M3 15h2", "M19 15h2"] },
  todo: { label: "Todo", paths: ["M5 6h14", "M5 12h14", "M5 18h9", "M3 6h.01", "M3 12h.01", "M3 18h.01"] },
  knowledge: { label: "知识库", paths: ["M4 5a2 2 0 0 1 2-2h9l5 5v11a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V5Z", "M14 3v5h5", "M8 12h8", "M8 16h5"] },
  tools: { label: "工具", paths: ["m14.7 6.3 3-3 3 3-3 3", "m17.7 3.3-7.1 7.1", "M5 20h4l8.7-8.7-4-4L5 16v4Z"] },
};

export function pageForTarget(targetId: string): ConsolePage | undefined {
  switch (targetId) {
    case "dashboard":
      return CONSOLE_PAGES[0];
    case "platforms":
    case "capabilities":
      return CONSOLE_PAGES[1];
    case "configuration":
      return CONSOLE_PAGES[2];
    case "storage":
      return CONSOLE_PAGES[3];
    case "memory":
      return CONSOLE_PAGES[4];
    case "todo":
      return CONSOLE_PAGES[5];
    case "knowledge":
      return CONSOLE_PAGES[6];
    case "markdown":
      return CONSOLE_PAGES[7];
    default:
      return undefined;
  }
}

function createConsoleIconWith(document: Document, name: ConsoleIconName): SVGSVGElement {
  const definition = ICONS[name];
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.8");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  for (const pathData of definition.paths) {
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", pathData);
    svg.append(path);
  }
  return svg;
}

export function createConsoleIcon(name: ConsoleIconName): SVGSVGElement {
  return createConsoleIconWith(document, name);
}

export function iconLabel(name: ConsoleIconName): string {
  return ICONS[name].label;
}

// 环境注入：真实浏览器由 bindConsoleNavigation 提供全局 document/window，
// node:test 通过 createConsoleNavigationShell 注入轻量 fixture。
export type ConsoleNavigationEnvironment = {
  readonly document: Document;
  readonly window: Window;
  readonly backgroundController: Pick<BackgroundController, "nextTransitionImage">;
  readonly wait: (duration: number) => Promise<void>;
};

export type ConsoleNavigationShell = {
  readonly navigate: (pageId: ConsolePageId, animate: boolean) => Promise<void>;
  readonly initialNavigation: Promise<void>;
};

const COVER_DURATION_MS = 784;
const WASHOUT_DURATION_MS = 896;

function hashForPage(page: ConsolePage | undefined): string {
  return page === undefined ? "#dashboard" : `#${page.targetId}`;
}

export function createConsoleNavigationShell(environment: ConsoleNavigationEnvironment): ConsoleNavigationShell {
  const { document, window, backgroundController, wait } = environment;
  const nav = document.getElementById("console-nav");
  const list = document.getElementById("console-nav-list");
  const content = document.getElementById("console-content");
  if (nav === null || list === null || content === null) {
    throw new Error("页面缺少控制台壳层元素");
  }
  const transition = document.getElementById("console-transition");
  const image = document.getElementById("console-transition-image") as HTMLElement | null;

  let currentPageId: ConsolePageId | null = null;
  let pendingRequest: { readonly pageId: ConsolePageId; readonly animate: boolean } | null = null;
  let transitionInFlight = false;
  let activeChain: Promise<void> = Promise.resolve();

  const prefersReducedMotion = (): boolean => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  const syncActiveLink = (page: ConsolePage): void => {
    for (const link of document.querySelectorAll<HTMLAnchorElement>(".bottom-nav-link")) {
      const active = link.dataset.pageId === page.id;
      link.classList.toggle("active", active);
      if (active) link.setAttribute("aria-current", "page");
      else link.removeAttribute("aria-current");
    }
  };

  // 单次转换：封面(784ms)后换页，再清洗(896ms)。目标页已显示时只做状态同步、不再播动画，
  // 保证“已显示但 aria-current 缺失”的入口（如默认总览）也能补齐导航状态。
  const runTransition = async (pageId: ConsolePageId, animate: boolean): Promise<void> => {
    const page = CONSOLE_PAGES.find((candidate) => candidate.id === pageId);
    if (page === undefined) return;
    const needsSwitch = currentPageId !== page.id;
    const animated = needsSwitch && animate && !prefersReducedMotion() && transition !== null && image !== null;
    if (animated) {
      // 默认（无背景）模式返回 null：只播主题清洗过渡、不显示中心图；
      // 特殊模式返回拼图 URL + 切片位置，通过 CSS background 显示对应切片。
      const transitionImage = backgroundController.nextTransitionImage();
      if (transitionImage === null) {
        image.style.backgroundImage = "none";
        image.style.backgroundPosition = "";
      } else {
        image.style.backgroundImage = `url("${transitionImage.url}")`;
        image.style.backgroundPosition = transitionImage.position;
      }
      transition.hidden = false;
      transition.classList.remove("is-running");
      void transition.offsetWidth;
      transition.classList.add("is-running");
      await wait(COVER_DURATION_MS);
    }
    if (needsSwitch) {
      for (const container of document.querySelectorAll<HTMLElement>("[data-console-page]")) {
        container.hidden = container.dataset.consolePage !== page.id;
      }
      currentPageId = page.id;
    }
    syncActiveLink(page);
    content.scrollTop = 0;
    if (animated) {
      await wait(WASHOUT_DURATION_MS);
      transition.classList.remove("is-running");
      transition.hidden = true;
    }
  };

  // 状态机：转换中到达的请求不丢弃，只保留最新一个（合并连续点击）；
  // 转换结束后切换到最新请求。锁在任何路径（含失败）都在 finally 中释放。
  const run = async (pageId: ConsolePageId, animate: boolean): Promise<void> => {
    transitionInFlight = true;
    try {
      await runTransition(pageId, animate);
    } finally {
      transitionInFlight = false;
    }
    const next = pendingRequest;
    if (next !== null) {
      pendingRequest = null;
      await run(next.pageId, next.animate);
    }
  };

  const navigate = (pageId: ConsolePageId, animate: boolean): Promise<void> => {
    if (transitionInFlight) {
      pendingRequest = { pageId, animate };
      return activeChain;
    }
    activeChain = run(pageId, animate);
    return activeChain;
  };

  list.replaceChildren(...CONSOLE_PAGES.map((page) => {
    const link = document.createElement("a");
    link.href = `#${page.targetId}`;
    link.className = "bottom-nav-link";
    link.dataset.pageId = page.id;
    link.setAttribute("aria-label", page.label);
    // 点击立即 pushState 形成历史记录，后退/前进由 hashchange 驱动同一状态机。
    link.addEventListener("click", (event) => {
      event.preventDefault();
      window.history.pushState(null, "", `#${page.targetId}`);
      void navigate(page.id, true);
    });
    link.append(createConsoleIconWith(document, page.icon));
    const label = document.createElement("span");
    label.textContent = page.label;
    link.append(label);
    return link;
  }));

  window.addEventListener("hashchange", () => {
    const page = pageForTarget(window.location.hash.slice(1));
    if (page !== undefined) {
      void navigate(page.id, true);
    } else {
      // 未知哈希回退到总览并修正地址，避免页面与哈希长期不一致。
      window.history.replaceState(null, "", hashForPage(undefined));
      void navigate("overview", true);
    }
  });

  const initialHash = window.location.hash.slice(1);
  const initialPage = pageForTarget(initialHash);
  if (initialPage === undefined) {
    window.history.replaceState(null, "", hashForPage(undefined));
  }
  const initialNavigation = navigate(initialPage?.id ?? "overview", false);

  return { navigate, initialNavigation };
}

export function bindConsoleNavigation(backgroundController: BackgroundController): void {
  createConsoleNavigationShell({
    document,
    window,
    backgroundController,
    wait: (duration) => new Promise((resolve) => window.setTimeout(resolve, duration)),
  });
}
