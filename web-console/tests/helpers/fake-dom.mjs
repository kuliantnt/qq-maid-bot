// 最小 DOM 桩：只覆盖配置页面测试路径用到的属性与方法。
// 不要在真实浏览器或 jsdom 上运行；仅用于 dist 模块的 Node 单测。

const REGISTERED_TAGS = new Set([
  "div", "section", "label", "input", "select", "option", "button", "span", "p",
  "fieldset", "legend", "details", "summary", "article", "h3", "h4", "code",
  "small", "strong", "form", "main", "nav", "ul", "li",
]);

function attributeName(prop) {
  return `data-${prop.replace(/[A-Z]/g, (char) => `-${char.toLowerCase()}`)}`;
}

function attributeValue(element, name) {
  if (element === null || typeof element !== "object" || element.dataset === undefined) return null;
  if (name.startsWith("data-")) {
    const camel = name.slice(5).replace(/-([a-z])/g, (_, char) => char.toUpperCase());
    if (camel in element.dataset) return String(element.dataset[camel]);
  }
  if (element.attributes.has(name)) return element.attributes.get(name);
  if (name in element && name !== "dataset" && name !== "children") return String(element[name]);
  return null;
}

function parseSimple(compound) {
  const tokens = [];
  let rest = compound.trim();
  while (rest.length > 0) {
    const char = rest[0];
    if (char === "#" || char === ".") {
      const restAfter = rest.slice(1);
      const end = restAfter.search(/[#.[]/g);
      const value = end === -1 ? restAfter : restAfter.slice(0, end);
      tokens.push([char === "#" ? "id" : "class", value]);
      rest = end === -1 ? "" : restAfter.slice(end);
    } else if (char === "[") {
      const close = rest.indexOf("]");
      if (close === -1) break;
      const expression = rest.slice(1, close);
      rest = rest.slice(close + 1);
      const equal = expression.indexOf("=");
      if (equal === -1) {
        tokens.push(["attr", expression.trim()]);
      } else {
        const name = expression.slice(0, equal).trim();
        let value = expression.slice(equal + 1).trim();
        if (value.startsWith('"') || value.startsWith("'")) value = value.slice(1, -1);
        tokens.push(["attr-value", name, value]);
      }
    } else {
      const end = rest.search(/[#.[]/g);
      const tag = (end === -1 ? rest : rest.slice(0, end)).toLowerCase();
      tokens.push(["tag", tag]);
      rest = end === -1 ? "" : rest.slice(end);
    }
  }
  return tokens;
}

function matchesTokens(element, tokens) {
  return tokens.every(([kind, ...args]) => {
    if (kind === "tag") return element.tagName.toLowerCase() === args[0];
    if (kind === "id") return element.id === args[0];
    if (kind === "class") return element.classList.contains(args[0]);
    if (kind === "attr") return attributeValue(element, args[0]) !== null;
    return attributeValue(element, args[0]) === args[1];
  });
}

function matchesSelector(element, selector) {
  return selector
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean)
    .some((compound) => {
      // 只支持无后代组合器的简单选择器；配置页面测试路径未用到后代组合器。
      if (compound.includes(" ")) return false;
      return matchesTokens(element, parseSimple(compound));
    });
}

function walkTree(element, visit) {
  if (element && typeof element === "object" && "tagName" in element && visit(element)) return true;
  for (const child of element?.children ?? []) {
    if (walkTree(child, visit)) return true;
  }
  return false;
}

export function createFakeDom() {
  const registry = new Map();
  const allElements = [];
  const documentListeners = new Map();

  class FakeHTMLElement {
    constructor(tag) {
      this.tagName = tag.toUpperCase();
      this.children = [];
      this.parentNode = null;
      this.attributes = new Map();
      const attributeMap = this.attributes;
      this.dataset = new Proxy({}, {
        set(target, prop, value) {
          target[prop] = value;
          attributeMap.set(attributeName(prop), String(value));
          return true;
        },
        get(target, prop) {
          return target[prop];
        },
      });
      this.classTokens = new Set();
      const styleProperties = {};
      this.style = {
        ...styleProperties,
        setProperty: (name, value) => {
          styleProperties[name] = String(value);
        },
        removeProperty: (name) => {
          delete styleProperties[name];
        },
      };
      this.listeners = new Map();
      this.disabled = false;
      this.hidden = false;
      this._value = "";
      this.checked = false;
      this._type = "text";
      this._id = "";
      this.textContent = "";
      this.htmlFor = "";
      this.title = "";
      this.placeholder = "";
      this.min = "";
      this.max = "";
      this.step = "";
      this.required = false;
      this.tabIndex = 0;
      this.name = "";
      this.onclick = null;
      this.scrollTop = 0;
      this.href = "";
      this.offsetWidth = 0;
      allElements.push(this);
    }

    get id() {
      return this._id;
    }

    set id(value) {
      this._id = value;
      if (value) registry.set(value, this);
    }

    get type() {
      return this._type;
    }

    set type(value) {
      this._type = value;
      this.attributes.set("type", String(value));
    }

    get value() {
      return this._value;
    }

    set value(next) {
      this._value = next === null || next === undefined ? "" : String(next);
    }

    get options() {
      return this.children.filter((child) => child.tagName === "OPTION");
    }

    get classList() {
      const tokens = this.classTokens;
      return {
        add: (...names) => names.forEach((name) => tokens.add(name)),
        remove: (...names) => names.forEach((name) => tokens.delete(name)),
        toggle: (name, force) => {
          if (force === undefined) {
            if (tokens.has(name)) {
              tokens.delete(name);
              return false;
            }
            tokens.add(name);
            return true;
          }
          if (force) tokens.add(name);
          else tokens.delete(name);
          return force;
        },
        contains: (name) => tokens.has(name),
      };
    }

    get className() {
      return [...this.classTokens].join(" ");
    }

    set className(value) {
      this.classTokens = new Set(String(value).split(/\s+/).filter(Boolean));
    }

    append(...nodes) {
      for (const node of nodes) {
        if (node === null || node === undefined) continue;
        if (Array.isArray(node)) {
          this.append(...node);
          continue;
        }
        node.parentNode = this;
        this.children.push(node);
      }
    }

    replaceChildren(...nodes) {
      this.children.length = 0;
      this.append(...nodes);
    }

    setAttribute(name, value) {
      this.attributes.set(name, String(value));
      if (name === "id") this.id = String(value);
    }

    removeAttribute(name) {
      this.attributes.delete(name);
    }

    getAttribute(name) {
      return attributeValue(this, name);
    }

    addEventListener(type, listener) {
      const list = this.listeners.get(type) ?? [];
      list.push(listener);
      this.listeners.set(type, list);
    }

    removeEventListener(type, listener) {
      const list = this.listeners.get(type) ?? [];
      this.listeners.set(type, list.filter((entry) => entry !== listener));
    }

    querySelectorAll(selector) {
      const matches = [];
      for (const child of this.children) {
        walkTree(child, (candidate) => {
          if (matchesSelector(candidate, selector)) matches.push(candidate);
          return false;
        });
      }
      return matches;
    }

    querySelector(selector) {
      return this.querySelectorAll(selector)[0] ?? null;
    }

    closest(selector) {
      let current = this;
      while (current) {
        if (matchesSelector(current, selector)) return current;
        current = current.parentNode;
      }
      return null;
    }

    focus() {
      this.focused = true;
    }

    checkValidity() {
      return true;
    }

    reportValidity() {
      return true;
    }
  }

  // 保持与真实 DOM 一致的 instanceof 层级：input/select/button 分别是独立子类，
  // 否则 `input instanceof HTMLSelectElement` 会对所有元素返回 true。
  class FakeHTMLInputElement extends FakeHTMLElement {}
  class FakeHTMLSelectElement extends FakeHTMLElement {}
  class FakeHTMLButtonElement extends FakeHTMLElement {}
  class FakeHTMLOptionElement extends FakeHTMLElement {}

  class FakeTextNode {
    constructor(text) {
      this.nodeType = 3;
      this.textContent = String(text);
      this.parentNode = null;
    }
  }

  const document = {
    getElementById(id) {
      return registry.get(id) ?? null;
    },
    createElement(tag) {
      const normalized = REGISTERED_TAGS.has(tag.toLowerCase()) ? tag.toLowerCase() : "div";
      if (normalized === "input") return new FakeHTMLInputElement(normalized);
      if (normalized === "select") return new FakeHTMLSelectElement(normalized);
      if (normalized === "button") return new FakeHTMLButtonElement(normalized);
      if (normalized === "option") return new FakeHTMLOptionElement(normalized);
      return new FakeHTMLElement(normalized);
    },
    createElementNS(_namespace, tag) {
      return document.createElement(tag);
    },
    createTextNode(text) {
      return new FakeTextNode(text);
    },
    querySelector(selector) {
      for (const element of allElements) {
        if (matchesSelector(element, selector)) return element;
      }
      return null;
    },
    querySelectorAll(selector) {
      return allElements.filter((element) => matchesSelector(element, selector));
    },
    addEventListener(type, listener) {
      const list = documentListeners.get(type) ?? [];
      list.push(listener);
      documentListeners.set(type, list);
    },
    get documentElement() {
      return document.getElementById("document-root") ?? null;
    },
    // 测试辅助：按 id 创建顶层结构元素并注册。
    registerStaticId(id, tag = "div") {
      const element = document.createElement(tag);
      element.id = id;
      return element;
    },
    listeners: documentListeners,
    registry,
  };

  return {
    document,
    FakeHTMLElement,
    FakeHTMLInputElement,
    FakeHTMLSelectElement,
    FakeHTMLButtonElement,
    FakeHTMLOptionElement,
    matchesSelector,
  };
}

export function jsonResponse(data, status = 200) {
  const body = JSON.stringify(data);
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: { get: () => null },
    json: async () => data,
    text: async () => body,
    blob: async () => new Blob([body]),
  };
}

export function installDomGlobals(fakeDom) {
  const {
    document,
    FakeHTMLElement,
    FakeHTMLInputElement,
    FakeHTMLSelectElement,
    FakeHTMLButtonElement,
    FakeHTMLOptionElement,
  } = fakeDom;
  globalThis.HTMLElement = FakeHTMLElement;
  globalThis.HTMLButtonElement = FakeHTMLButtonElement;
  globalThis.HTMLInputElement = FakeHTMLInputElement;
  globalThis.HTMLSelectElement = FakeHTMLSelectElement;
  globalThis.HTMLOptionElement = FakeHTMLOptionElement;
  globalThis.Document = class {};
  globalThis.document = document;
  globalThis.window = {
    setTimeout: () => 1,
    clearTimeout: () => undefined,
    confirm: () => false,
    localStorage: null,
  };
  globalThis.URL = URL;
}

export function clearDomGlobals() {
  for (const name of [
    "HTMLElement", "HTMLButtonElement", "HTMLInputElement", "HTMLSelectElement",
    "HTMLOptionElement", "Document", "document", "window",
  ]) {
    delete globalThis[name];
  }
}

export async function waitFor(predicate, message = "condition not met", timeoutMs = 2_000) {
  const start = Date.now();
  while (!predicate()) {
    if (Date.now() - start > timeoutMs) throw new Error(`waitFor timeout: ${message}`);
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
}

export const flushMicrotasks = () => new Promise((resolve) => setTimeout(resolve, 0));
