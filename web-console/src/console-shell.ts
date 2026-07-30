export const CONSOLE_PAGE_IDS = ["overview", "platforms", "configuration", "storage", "todo", "tools"] as const;
export type ConsolePageId = (typeof CONSOLE_PAGE_IDS)[number];
export type ConsoleIconName = "overview" | "platforms" | "configuration" | "storage" | "todo" | "tools";
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
  { id: "todo", label: "Todo", icon: "todo", targetId: "todo" },
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
  todo: { label: "Todo", paths: ["M5 6h14", "M5 12h14", "M5 18h9", "M3 6h.01", "M3 12h.01", "M3 18h.01"] },
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
    case "todo":
      return CONSOLE_PAGES[4];
    case "markdown":
      return CONSOLE_PAGES[5];
    default:
      return undefined;
  }
}

export function createConsoleIcon(name: ConsoleIconName): SVGSVGElement {
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

export function iconLabel(name: ConsoleIconName): string {
  return ICONS[name].label;
}

import type { BackgroundController } from "./background.js";

export function bindConsoleNavigation(backgroundController: BackgroundController): void {
  const nav = document.getElementById("console-nav");
  const list = document.getElementById("console-nav-list");
  const content = document.getElementById("console-content");
  if (!(nav instanceof HTMLElement) || !(list instanceof HTMLElement) || !(content instanceof HTMLElement)) {
    throw new Error("页面缺少控制台壳层元素");
  }
  list.replaceChildren(...CONSOLE_PAGES.map((page) => {
    const link = document.createElement("a");
    link.href = `#${page.targetId}`;
    link.className = "bottom-nav-link";
    link.dataset.pageId = page.id;
    link.setAttribute("aria-label", page.label);
    link.addEventListener("click", (event) => {
      event.preventDefault();
       void activatePage(page.id, true, content, backgroundController, true);
    });
    link.append(createConsoleIcon(page.icon));
    const label = document.createElement("span");
    label.textContent = page.label;
    link.append(label);
    return link;
  }));
  const initialPage = pageForTarget(window.location.hash.slice(1));
  void activatePage(initialPage?.id ?? "overview", false, content, backgroundController, false);
  window.addEventListener("hashchange", () => {
    const page = pageForTarget(window.location.hash.slice(1));
    if (page) void activatePage(page.id, false, content, backgroundController, true);
  });
}

let transitionInFlight = false;

async function activatePage(
  pageId: ConsolePageId,
  updateHistory: boolean,
  content: HTMLElement,
  backgroundController: BackgroundController,
  animate: boolean,
): Promise<void> {
  const page = CONSOLE_PAGES.find((candidate) => candidate.id === pageId);
  if (!page) return;
  const current = document.querySelector<HTMLElement>("[data-console-page]:not([hidden])")?.dataset.consolePage;
  if (current === page.id) return;
  if (transitionInFlight) return;
  transitionInFlight = true;
  const transition = document.getElementById("console-transition");
  const image = document.getElementById("console-transition-image");
  const animated = animate && transition instanceof HTMLElement && image instanceof HTMLImageElement && !window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (animated) {
    image.src = backgroundController.nextTransitionImage();
    transition.hidden = false;
    transition.classList.remove("is-running");
    void transition.offsetWidth;
    transition.classList.add("is-running");
    await wait(784);
  }
  for (const container of document.querySelectorAll<HTMLElement>("[data-console-page]")) {
    container.hidden = container.dataset.consolePage !== page.id;
  }
  for (const link of document.querySelectorAll<HTMLAnchorElement>(".bottom-nav-link")) {
    const active = link.dataset.pageId === page.id;
    link.classList.toggle("active", active);
    if (active) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
  content.scrollTop = 0;
  if (updateHistory) window.history.replaceState(null, "", `#${page.targetId}`);
  if (animated) {
    await wait(896);
    transition.classList.remove("is-running");
    transition.hidden = true;
  }
  transitionInFlight = false;
}

function wait(duration: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, duration));
}
