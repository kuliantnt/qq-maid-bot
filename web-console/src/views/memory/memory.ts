import {
  archiveMemory,
  commitMemoryOperation,
  createMemory,
  getMemory,
  listMemoryTargets,
  listMemories,
  prepareMemoryOperation,
  restoreMemory,
  updateMemory,
} from "../../api.js";
import type {
  MemoryCategory,
  MemoryItem,
  MemoryKind,
  MemoryListParams,
  MemoryOperation,
  MemoryTargetView,
  MemoryVisibility,
} from "../../types.js";

let initialized = false;
let viewGeneration = 0;
let page = 1;
let items: MemoryItem[] = [];
let targetOptions: MemoryTargetView[] = [];
let currentPage: { total: number; totalPages: number } = { total: 0, totalPages: 0 };

const KIND_LABELS: Record<MemoryKind, string> = {
  personal: "个人记忆",
  group_profile: "群内用户画像",
  group: "群组记忆",
};

const CATEGORY_LABELS: Record<MemoryCategory, string> = {
  note: "普通记录",
  preference: "偏好",
  identity: "身份",
  relation: "关系",
  instruction: "指令",
};

const VISIBILITY_LABELS: Record<MemoryVisibility, string> = {
  private: "仅本人",
  context_only: "当前上下文",
  group_members: "群成员",
  public: "公开",
};

const CATEGORY_OPTIONS: readonly [MemoryCategory | "", string][] = [
  ["", "全部分类"],
  ["note", "普通记录"],
  ["preference", "偏好"],
  ["identity", "身份"],
  ["relation", "关系"],
  ["instruction", "指令"],
];

const VISIBILITY_OPTIONS: Record<MemoryKind, readonly [MemoryVisibility, string][]> = {
  personal: [
    ["private", "仅本人"],
    ["context_only", "当前上下文"],
    ["public", "公开"],
  ],
  group_profile: [
    ["context_only", "当前上下文"],
    ["group_members", "群成员"],
    ["public", "公开"],
  ],
  group: [
    ["group_members", "群成员"],
    ["public", "公开"],
  ],
};

export async function initializeMemory(): Promise<void> {
  if (initialized) return;
  initialized = true;
  bindControls();
  await refreshMemories();
}

export function disposeMemory(): void {
  initialized = false;
  viewGeneration += 1;
  page = 1;
  items = [];
  targetOptions = [];
  currentPage = { total: 0, totalPages: 0 };
  document.getElementById("memory-list")?.replaceChildren();
  document.getElementById("memory-targets")?.replaceChildren();
  document.getElementById("memory-pagination")?.replaceChildren();
  setResult("");
}

function bindControls(): void {
  const refresh = element("memory-refresh", HTMLButtonElement);
  const apply = element("memory-filter-submit", HTMLButtonElement);
  const reset = element("memory-filter-reset", HTMLButtonElement);
  const form = element("memory-create-form", HTMLFormElement);
  const categoryFilter = element("memory-type-filter", HTMLSelectElement);
  const createTarget = element("memory-create-target", HTMLSelectElement);
  categoryFilter.replaceChildren(...CATEGORY_OPTIONS.map(([value, label]) => new Option(label, value)));
  refresh.onclick = () => void refreshMemories();
  apply.onclick = () => { page = 1; void refreshMemories(); };
  reset.onclick = () => {
    for (const [id, value] of Object.entries(memoryFilterDefaults())) {
      const field = document.getElementById(id);
      if (field instanceof HTMLInputElement || field instanceof HTMLSelectElement) field.value = value;
    }
    page = 1;
    void refreshMemories();
  };
  createTarget.onchange = () => updateCreateVisibilityOptions();
  form.onsubmit = (event) => {
    event.preventDefault();
    void submitCreate(form);
  };
}

function memoryFilterDefaults(): Readonly<Record<string, string>> {
  return {
    "memory-kind-filter": "all",
    "memory-status-filter": "active",
    "memory-type-filter": "",
    "memory-visibility-filter": "all",
    "memory-pinned-filter": "all",
    "memory-query-filter": "",
    "memory-platform-filter": "",
    "memory-account-filter": "",
    "memory-group-filter": "",
    "memory-user-filter": "",
  };
}

async function refreshMemories(): Promise<void> {
  const generation = ++viewGeneration;
  renderLoading();
  try {
    const [result, targets] = await Promise.all([
      listMemories(readParams()),
      loadAllMemoryTargets(),
    ]);
    if (!initialized || generation !== viewGeneration) return;
    items = result.items;
    targetOptions = targets;
    currentPage = { total: result.total, totalPages: result.totalPages };
    renderMemoryContent();
    setResult(`${result.total} 条记忆`, false);
  } catch (cause) {
    if (!initialized || generation !== viewGeneration) return;
    renderError();
    setResult(cause instanceof Error ? cause.message : "Memory 列表加载失败", true);
  }
}

function readParams(): MemoryListParams {
  const value = (id: string): string => {
    const field = document.getElementById(id);
    return field instanceof HTMLInputElement || field instanceof HTMLSelectElement ? field.value : "";
  };
  const scope = value("memory-kind-filter");
  const status = value("memory-status-filter");
  const category = value("memory-type-filter");
  const visibility = value("memory-visibility-filter");
  return {
    page,
    pageSize: 20,
    scope: isMemoryKind(scope) ? scope : "all",
    status: status === "archived" || status === "active" ? status : "all",
    category: isMemoryCategory(category) ? category : "all",
    visibility: isMemoryVisibility(visibility) ? visibility : "all",
    pinned: value("memory-pinned-filter") === "true" || value("memory-pinned-filter") === "false"
      ? value("memory-pinned-filter") as "true" | "false"
      : "all",
    keyword: value("memory-query-filter").trim(),
    platform: value("memory-platform-filter").trim(),
    accountRef: value("memory-account-filter").trim(),
    groupRef: value("memory-group-filter").trim(),
    subjectRef: value("memory-user-filter").trim(),
  };
}

function renderMemoryContent(): void {
  const list = element("memory-list", HTMLElement);
  list.replaceChildren();
  if (items.length === 0) {
    list.append(Object.assign(document.createElement("p"), { className: "hint", textContent: "当前筛选没有可展示的记忆。" }));
  } else {
    for (const item of items) list.append(memoryCard(item));
  }
  renderTargetControls();
  renderMemoryTargets();
  renderPagination();
}

function memoryCard(item: MemoryItem): HTMLElement {
  const card = document.createElement("article");
  card.className = `memory-card memory-card--${item.kind}${item.status === "archived" ? " memory-card--archived" : ""}`;
  const heading = document.createElement("div");
  heading.className = "memory-card-heading";
  const title = document.createElement("h3");
  title.textContent = `${KIND_LABELS[item.kind]} · ${CATEGORY_LABELS[item.category]}`;
  const status = document.createElement("span");
  status.className = `memory-status memory-status--${item.status}`;
  status.textContent = item.status === "active" ? "ACTIVE" : "ARCHIVED";
  heading.append(title, status);
  card.append(heading);

  const target = document.createElement("p");
  target.className = "memory-card-meta";
  target.textContent = targetLabel(item.target);
  card.append(target);

  const content = document.createElement("p");
  content.className = "memory-card-content";
  content.textContent = item.content;
  card.append(content);

  const meta = document.createElement("p");
  meta.className = "memory-card-meta";
  meta.textContent = `${item.pinned ? "已固定 · " : ""}版本 v${item.version} · ${VISIBILITY_LABELS[item.visibility]} · ${item.createdAt}${item.updatedAt ? ` · 更新于 ${item.updatedAt}` : ""}`;
  card.append(meta);

  const actions = document.createElement("div");
  actions.className = "memory-card-actions";
  if (item.status === "active") {
    if (item.capabilities.canUpdate) actions.append(actionButton("纠正内容", () => void editMemory(item)));
    if (item.capabilities.canArchive) actions.append(actionButton("归档", () => void archiveItem(item)));
  } else if (item.capabilities.canRestore) {
    actions.append(actionButton("恢复", () => void restoreItem(item)));
  }
  if (actions.childElementCount > 0) card.append(actions);
  return card;
}

function renderTargetControls(): void {
  const targetContainer = element("memory-targets", HTMLElement);
  targetContainer.replaceChildren();
  for (const target of targetOptions) {
    const row = document.createElement("div");
    row.className = "memory-target-row";
    const label = document.createElement("span");
    label.textContent = targetLabel(target);
    row.append(label);
    row.append(actionButton("清空此范围", () => void confirmTargetOperation("clear_target", target), "danger"));
    if (target.scope === "group_profile") {
      row.append(actionButton("停止画像", () => void confirmTargetOperation("disable_group_profile", target), "danger"));
    }
    targetContainer.append(row);
  }
}

function renderMemoryTargets(): void {
  const select = element("memory-create-target", HTMLSelectElement);
  select.replaceChildren(new Option(targetOptions.length > 0 ? "选择已授权范围…" : "暂无可用范围", ""));
  for (const target of targetOptions) {
    select.append(new Option(targetLabel(target), target.targetRef));
  }
  select.disabled = targetOptions.length === 0;
  updateCreateVisibilityOptions();
}

async function loadAllMemoryTargets(): Promise<MemoryTargetView[]> {
  const first = await listMemoryTargets(1, 100);
  if (first.totalPages <= 1) return first.items;
  const pages = await Promise.all(
    Array.from({ length: first.totalPages - 1 }, (_, index) => listMemoryTargets(index + 2, 100)),
  );
  return [first, ...pages].flatMap((current) => current.items);
}

function updateCreateVisibilityOptions(): void {
  const targetSelect = element("memory-create-target", HTMLSelectElement);
  const visibilitySelect = element("memory-create-visibility", HTMLSelectElement);
  const target = targetOptions.find((option) => option.targetRef === targetSelect.value);
  const options = target ? VISIBILITY_OPTIONS[target.scope] : [];
  const previous = visibilitySelect.value;
  visibilitySelect.replaceChildren(...options.map(([value, label]) => new Option(label, value)));
  visibilitySelect.disabled = options.length === 0;
  if (options.some(([value]) => value === previous)) {
    visibilitySelect.value = previous;
  } else if (options[0] !== undefined) {
    visibilitySelect.value = options[0][0];
  }
}

async function submitCreate(form: HTMLFormElement): Promise<void> {
  const targetRef = element("memory-create-target", HTMLSelectElement).value;
  const content = element("memory-create-content", HTMLTextAreaElement).value.trim();
  const category = asMemoryCategory(element("memory-create-type", HTMLSelectElement).value);
  const visibility = asMemoryVisibility(element("memory-create-visibility", HTMLSelectElement).value);
  const pinned = element("memory-create-pinned", HTMLInputElement).checked;
  const target = targetOptions.find((option) => option.targetRef === targetRef);
  if (!targetRef || !target || !content || category === null || visibility === null) {
    setResult("创建需要选择范围、有效可见性并填写内容", true);
    return;
  }
  if (!VISIBILITY_OPTIONS[target.scope].some(([value]) => value === visibility)) {
    setResult("当前范围不支持所选可见性", true);
    return;
  }
  const submit = form.querySelector<HTMLButtonElement>("button[type=submit]");
  if (submit) submit.disabled = true;
  try {
    await createMemory({ targetRef, content, category, visibility, pinned });
    form.reset();
    setResult("Memory 已由服务端确认创建", false);
    await refreshMemories();
  } catch (cause) {
    setResult(cause instanceof Error ? cause.message : "Memory 创建失败", true);
  } finally {
    if (submit) submit.disabled = false;
  }
}

async function editMemory(item: MemoryItem): Promise<void> {
  try {
    const latest = await getMemory(item.target.targetRef, item.memoryRef);
    if (!latest.capabilities.canUpdate) {
      setResult("该 Memory 当前不可编辑，请刷新后重试", true);
      return;
    }
    const content = window.prompt("修正记忆内容", latest.content);
    if (content === null || content.trim() === latest.content) return;
    await updateMemory({
      targetRef: latest.target.targetRef,
      memoryRef: latest.memoryRef,
      expectedVersion: latest.version,
      patch: { content: content.trim() },
    });
    setResult("Memory 已由服务端确认更新", false);
    await refreshMemories();
  } catch (cause) {
    setResult(cause instanceof Error ? cause.message : "Memory 更新失败", true);
  }
}

async function archiveItem(item: MemoryItem): Promise<void> {
  if (!window.confirm("确定归档这条 Memory 吗？归档后仍可恢复。")) return;
  try {
    await archiveMemory({
      targetRef: item.target.targetRef,
      memoryRef: item.memoryRef,
      expectedVersion: item.version,
    });
    setResult("Memory 已由服务端确认归档", false);
    await refreshMemories();
  } catch (cause) {
    setResult(cause instanceof Error ? cause.message : "Memory 归档失败", true);
  }
}

async function confirmTargetOperation(operation: MemoryOperation, target: MemoryTargetView): Promise<void> {
  try {
    const confirmation = await prepareMemoryOperation({ operation, targetRef: target.targetRef });
    const noun = operation === "disable_group_profile" ? "停止画像并归档" : "清空";
    if (!window.confirm(`确定${noun} ${confirmation.affectedCount} 条 Memory 吗？此操作需要服务端确认。`)) return;
    const result = await commitMemoryOperation({
      operation,
      targetRef: target.targetRef,
      confirmationToken: confirmation.confirmationToken,
    });
    setResult(`服务端已完成${noun}：${result.affectedCount} 条`, false);
    await refreshMemories();
  } catch (cause) {
    setResult(cause instanceof Error ? cause.message : "Memory 操作失败", true);
  }
}

async function restoreItem(item: MemoryItem): Promise<void> {
  try {
    await restoreMemory({
      targetRef: item.target.targetRef,
      memoryRef: item.memoryRef,
      expectedVersion: item.version,
    });
    setResult("Memory 已由服务端确认恢复", false);
    await refreshMemories();
  } catch (cause) {
    setResult(cause instanceof Error ? cause.message : "Memory 恢复失败", true);
  }
}

function renderPagination(): void {
  const target = element("memory-pagination", HTMLElement);
  target.replaceChildren();
  if (currentPage.totalPages <= 1) return;
  const previous = actionButton("上一页", () => { if (page > 1) { page -= 1; void refreshMemories(); } });
  previous.disabled = page <= 1;
  const next = actionButton("下一页", () => { if (page < currentPage.totalPages) { page += 1; void refreshMemories(); } });
  next.disabled = page >= currentPage.totalPages;
  const label = document.createElement("span");
  label.textContent = `第 ${page} / ${currentPage.totalPages} 页 · ${currentPage.total} 条`;
  target.append(previous, label, next);
}

function renderLoading(): void {
  element("memory-list", HTMLElement).replaceChildren(Object.assign(document.createElement("p"), { className: "hint", textContent: "正在加载 Memory…" }));
}

function renderError(): void {
  const target = element("memory-list", HTMLElement);
  const retry = actionButton("重试", () => void refreshMemories());
  target.replaceChildren(Object.assign(document.createElement("p"), { className: "hint", textContent: "Memory 列表加载失败，请检查权限或重试。" }), retry);
}

/** 管理 API 只返回 opaque ref；页面不尝试反解析原始账号、群组或用户 ID。 */
function targetLabel(target: MemoryTargetView): string {
  return [
    KIND_LABELS[target.scope],
    target.platform,
    `账号 ${compactRef(target.accountRef)}`,
    target.groupRef ? `群组 ${compactRef(target.groupRef)}` : null,
    target.subjectRef ? `用户 ${compactRef(target.subjectRef)}` : null,
  ].filter((value): value is string => value !== null).join(" · ");
}

function compactRef(value: string): string {
  return value.length <= 32 ? value : `${value.slice(0, 22)}…${value.slice(-8)}`;
}

function isMemoryKind(value: string): value is MemoryKind {
  return value === "personal" || value === "group_profile" || value === "group";
}

function isMemoryCategory(value: string): value is MemoryCategory {
  return value === "note" || value === "preference" || value === "identity" || value === "relation" || value === "instruction";
}

function asMemoryCategory(value: string): MemoryCategory | null {
  return isMemoryCategory(value) ? value : null;
}

function isMemoryVisibility(value: string): value is MemoryVisibility {
  return value === "private" || value === "context_only" || value === "group_members" || value === "public";
}

function asMemoryVisibility(value: string): MemoryVisibility | null {
  return isMemoryVisibility(value) ? value : null;
}

function actionButton(label: string, callback: () => void, variant = "secondary"): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = `memory-action ${variant}`;
  button.textContent = label;
  button.onclick = callback;
  return button;
}

function setResult(message: string, error = false): void {
  const target = document.getElementById("memory-result");
  if (!(target instanceof HTMLElement)) return;
  target.textContent = message;
  target.classList.toggle("error", error);
}

function element<T extends HTMLElement>(id: string, type: { new(): T }): T {
  const value = document.getElementById(id);
  if (!(value instanceof type)) throw new Error(`Memory 页面缺少控件 ${id}`);
  return value;
}
