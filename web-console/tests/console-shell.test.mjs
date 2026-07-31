import assert from "node:assert/strict";
import test from "node:test";

import {
  CONSOLE_PAGES,
  createConsoleNavigationShell,
  pageForTarget,
} from "../dist/console-shell.js";

function fakeElement() {
  const attributes = new Map();
  const classes = new Set();
  const listeners = new Map();
  return {
    dataset: {},
    hidden: false,
    href: "",
    className: "",
    textContent: "",
    offsetWidth: 0,
    scrollTop: 0,
    classList: {
      add: (...tokens) => tokens.forEach((token) => classes.add(token)),
      remove: (...tokens) => tokens.forEach((token) => classes.delete(token)),
      toggle: (token, force) => {
        if (force === undefined) {
          if (classes.has(token)) {
            classes.delete(token);
            return false;
          }
          classes.add(token);
          return true;
        }
        if (force) classes.add(token);
        else classes.delete(token);
        return force;
      },
    },
    setAttribute: (name, value) => attributes.set(name, String(value)),
    removeAttribute: (name) => attributes.delete(name),
    getAttribute: (name) => (attributes.has(name) ? attributes.get(name) : null),
    append: () => {},
    addEventListener: (type, listener) => listeners.set(type, listener),
    hasClass: (token) => classes.has(token),
    _fire: (type, event) => {
      const listener = listeners.get(type);
      if (listener) listener(event);
    },
  };
}

function createFakeShell({ hash = "", reducedMotion = false, nextImage = () => "/console/background/01.png" } = {}) {
  const containers = CONSOLE_PAGES.map((page) => {
    const container = fakeElement();
    container.dataset.consolePage = page.id;
    container.hidden = page.id !== "overview";
    return container;
  });
  const links = [];
  const byId = new Map();
  byId.set("console-nav", fakeElement());
  byId.set("console-nav-list", {
    replaceChildren: (...children) => {
      links.length = 0;
      links.push(...children);
    },
  });
  byId.set("console-content", { scrollTop: 0 });
  byId.set("console-transition", { ...fakeElement(), hidden: true });
  byId.set("console-transition-image", fakeElement());

  const timers = [];
  const waitCalls = [];
  const hashListeners = [];
  const state = { hash };

  const document = {
    getElementById: (id) => byId.get(id) ?? null,
    querySelectorAll: (selector) => {
      if (selector === "[data-console-page]") return containers;
      if (selector === ".bottom-nav-link") return links;
      return [];
    },
    createElement: () => fakeElement(),
    createElementNS: () => fakeElement(),
  };
  const windowObj = {
    location: {
      get hash() {
        return state.hash;
      },
      set hash(value) {
        state.hash = value;
      },
    },
    history: {
      pushState: (_data, _unused, url) => {
        state.hash = typeof url === "string" ? url : "";
      },
      replaceState: (_data, _unused, url) => {
        state.hash = typeof url === "string" ? url : "";
      },
    },
    matchMedia: () => ({ matches: reducedMotion }),
    addEventListener: (type, listener) => {
      if (type === "hashchange") hashListeners.push(listener);
    },
  };

  return {
    environment: {
      document,
      window: windowObj,
      backgroundController: { nextTransitionImage: nextImage },
      wait: (ms) => {
        waitCalls.push(ms);
        return new Promise((resolve) => timers.push({ ms, resolve }));
      },
    },
    timers,
    waitCalls,
    links,
    containers,
    state,
    transition: byId.get("console-transition"),
    fireHashchange: () => {
      for (const listener of [...hashListeners]) listener();
    },
  };
}

// 触发一个待处理的转换计时器，并在宏观任务边界等待微任务链继续执行。
async function flushOne(fake) {
  const timer = fake.timers.shift();
  assert.ok(timer, "期望存在一个待处理的转换计时器");
  timer.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

async function settle() {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function clickLink(fake, pageId) {
  const link = fake.links.find((candidate) => candidate.dataset.pageId === pageId);
  assert.ok(link, `缺少导航链接 ${pageId}`);
  link._fire("click", { preventDefault() {} });
}

function assertPageShown(fake, pageId) {
  const page = CONSOLE_PAGES.find((candidate) => candidate.id === pageId);
  assert.ok(page, `未知页面 ${pageId}`);
  for (const container of fake.containers) {
    assert.equal(container.hidden, container.dataset.consolePage !== pageId, `页面 ${container.dataset.consolePage} 的可见性`);
  }
  for (const link of fake.links) {
    const expected = link.dataset.pageId === pageId;
    assert.equal(link.hasClass("active"), expected, `导航 ${link.dataset.pageId} 的 active 状态`);
    assert.equal(link.getAttribute("aria-current"), expected ? "page" : null, `导航 ${link.dataset.pageId} 的 aria-current`);
  }
  assert.equal(fake.state.hash, `#${page.targetId}`, "地址哈希应与当前页面一致");
}

test("默认入口（无哈希或未知哈希）激活总览并标记导航为当前页", async () => {
  for (const hash of ["", "#unknown"]) {
    const fake = createFakeShell({ hash });
    const shell = createConsoleNavigationShell(fake.environment);
    await shell.initialNavigation;
    assertPageShown(fake, "overview");
  }
});

test("点击导航链接立即更新哈希，并在转换完成后设置 aria-current", async () => {
  const fake = createFakeShell({ hash: "" });
  const shell = createConsoleNavigationShell(fake.environment);
  await shell.initialNavigation;

  clickLink(fake, "tools");
  await flushOne(fake);
  await flushOne(fake);

  assertPageShown(fake, "tools");
});

test("转换期间连续导航只执行最后一次请求（合并快速点击）", async () => {
  const fake = createFakeShell({ hash: "" });
  const shell = createConsoleNavigationShell(fake.environment);
  await shell.initialNavigation;

  clickLink(fake, "platforms");
  clickLink(fake, "storage");
  clickLink(fake, "configuration");

  await flushOne(fake); // platforms 封面结束 → 换页并清洗
  await flushOne(fake); // platforms 清洗结束 → 开始 configuration
  await flushOne(fake); // configuration 封面结束
  await flushOne(fake); // configuration 清洗结束

  assertPageShown(fake, "configuration");
  assert.deepEqual(fake.waitCalls, [784, 896, 784, 896]);
});

test("hashchange（后退/前进）驱动同一状态机切换页面和导航", async () => {
  const fake = createFakeShell({ hash: "" });
  const shell = createConsoleNavigationShell(fake.environment);
  await shell.initialNavigation;

  fake.state.hash = "#platforms";
  fake.fireHashchange();
  await flushOne(fake);
  await flushOne(fake);
  assertPageShown(fake, "platforms");

  fake.state.hash = "#dashboard";
  fake.fireHashchange();
  await flushOne(fake);
  await flushOne(fake);
  assertPageShown(fake, "overview");
});

test("转换失败时释放转换锁，后续导航仍可执行", async () => {
  let failImage = true;
  const fake = createFakeShell({
    hash: "",
    nextImage: () => {
      if (failImage) throw new Error("转换图片加载失败");
      return "/console/background/01.png";
    },
  });
  const shell = createConsoleNavigationShell(fake.environment);
  await shell.initialNavigation;

  await assert.rejects(() => shell.navigate("platforms", true));

  // 锁已释放，后续点击导航可以正常完成（含 pushState 更新哈希）。
  failImage = false;
  clickLink(fake, "configuration");
  await flushOne(fake);
  await flushOne(fake);

  assertPageShown(fake, "configuration");
});

test("prefers-reduced-motion 下不播放动画但完整同步状态", async () => {
  const fake = createFakeShell({ hash: "", reducedMotion: true });
  const shell = createConsoleNavigationShell(fake.environment);
  await shell.initialNavigation;

  assert.equal(fake.waitCalls.length, 0, "初始导航不应等待动画");

  clickLink(fake, "storage");
  await settle();

  assert.equal(fake.waitCalls.length, 0, "减少动态下不应注册动画计时器");
  assertPageShown(fake, "storage");
  assert.equal(fake.transition.hidden, true, "转换遮罩应保持隐藏");
});

test("pageForTarget 映射保持正确", () => {
  assert.equal(pageForTarget("dashboard")?.id, "overview");
  assert.equal(pageForTarget("capabilities")?.id, "platforms");
  assert.equal(pageForTarget("markdown")?.id, "tools");
  assert.equal(pageForTarget("unknown"), undefined);
});
