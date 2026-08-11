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
} from "../../memory-api.js";
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
let lifecycleGeneration = 0;
let listRequestGeneration = 0;
let page = 1;
let items: MemoryItem[] = [];
let targetOptions: MemoryTargetView[] = [];
// target discovery 使用服务端分页；只把已加载页用于创建、筛选和范围操作。
const MEMORY_TARGET_PAGE_SIZE = 100;
type MemoryListState = "loading" | "ready" | "error";
let memoryListState: MemoryListState = "loading";
let targetPage = 0;
let targetTotal = 0;
let targetTotalPages = 0;
let targetLoading = false;
let targetError: unknown = null;
let targetRequestGeneration = 0;
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
  void refreshMemoryTargets(true);
  await refreshMemories();
}

export function disposeMemory(): void {
  initialized = false;
  lifecycleGeneration += 1;
  listRequestGeneration += 1;
  page = 1;
  items = [];
  targetOptions = [];
  memoryListState = "loading";
  targetPage = 0;
  targetTotal = 0;
  targetTotalPages = 0;
  targetLoading = false;
  targetError = null;
  targetRequestGeneration += 1;
  currentPage = { total: 0, totalPages: 0 };
  for (const [id, value] of Object.entries(memoryFilterDefaults())) {
    const field = document.getElementById(id);
    if (field instanceof HTMLInputElement || field instanceof HTMLSelectElement) field.value = value;
  }
  resetMemoryCreateForm();
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
  refresh.onclick = () => {
    const params = readParams();
    void refreshMemoryTargets(true);
    void refreshMemories(params);
  };
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

function resetMemoryCreateForm(): void {
  const form = document.getElementById("memory-create-form");
  if (form instanceof HTMLFormElement) form.reset();
  const content = document.getElementById("memory-create-content");
  if (content instanceof HTMLTextAreaElement) content.value = "";
  const target = document.getElementById("memory-create-target");
  if (target instanceof HTMLSelectElement) target.value = "";
  const visibility = document.getElementById("memory-create-visibility");
  if (visibility instanceof HTMLSelectElement) visibility.value = "";
  const pinned = document.getElementById("memory-create-pinned");
  if (pinned instanceof HTMLInputElement) pinned.checked = false;
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

async function refreshMemories(params: MemoryListParams = readParams()): Promise<void> {
  const generation = ++listRequestGeneration;
  const lifecycle = lifecycleGeneration;
  renderLoading();
  try {
    const result = await listMemories(params);
    if (!isCurrentLifecycle(lifecycle) || generation !== listRequestGeneration) return;
    // 归档/恢复可能让当前页失效；回到最后有效页，避免空列表把分页控件一起隐藏。
    if (page > Math.max(result.totalPages, 1) && page > 1) {
      page = Math.max(1, result.totalPages);
      return refreshMemories({ ...params, page });
    }
    items = result.items;
    currentPage = { total: result.total, totalPages: result.totalPages };
    renderMemoryContent();
    setResult(`${result.total} 条记忆`, false);
  } catch (cause) {
    if (!isCurrentLifecycle(lifecycle) || generation !== listRequestGeneration) return;
    renderError();
    setResult(cause instanceof Error ? cause.message : "Memory 列表加载失败", true);
  }
}

function isCurrentLifecycle(generation: number): boolean {
  return initialized && generation === lifecycleGeneration;
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
  memoryListState = "ready";
  const list = element("memory-list", HTMLElement);
  list.replaceChildren();
  if (items.length === 0) {
    list.append(Object.assign(document.createElement("p"), { className: "hint", textContent: "当前筛选没有可展示的记忆。" }));
  } else {
    for (const item of items) list.append(memoryCard(item));
  }
  renderMemoryAdvancedFilters();
  renderTargetControls();
  renderMemoryTargets();
  renderPagination();
}

/**
 * 目标列表是独立的渐进式资源：Memory 列表先可用，后续 target 页失败只影响目标控件。
 */
async function refreshMemoryTargets(reset: boolean): Promise<void> {
  const generation = ++targetRequestGeneration;
  if (reset) {
    targetOptions = [];
    targetPage = 0;
    targetTotal = 0;
    targetTotalPages = 0;
  }
  targetLoading = true;
  targetError = null;
  renderTargetState();
  try {
    const result = await listMemoryTargets(1, MEMORY_TARGET_PAGE_SIZE);
    if (!isCurrentTargetRequest(generation)) return;
    targetOptions = result.items;
    targetPage = result.page;
    targetTotal = result.total;
    targetTotalPages = result.totalPages;
  } catch (cause) {
    if (!isCurrentTargetRequest(generation)) return;
    targetError = cause;
  } finally {
    if (isCurrentTargetRequest(generation)) {
      targetLoading = false;
      renderTargetState();
    }
  }
}

async function loadMoreMemoryTargets(): Promise<void> {
  if (targetLoading || targetPage >= targetTotalPages) return;
  const generation = targetRequestGeneration;
  const requestedPage = targetPage + 1;
  targetLoading = true;
  targetError = null;
  renderTargetState();
  try {
    const result = await listMemoryTargets(requestedPage, MEMORY_TARGET_PAGE_SIZE);
    if (!isCurrentTargetRequest(generation)) return;
    targetOptions = mergeMemoryTargets(targetOptions, result.items);
    targetPage = result.page;
    targetTotal = result.total;
    targetTotalPages = result.totalPages;
  } catch (cause) {
    if (!isCurrentTargetRequest(generation)) return;
    targetError = cause;
  } finally {
    if (isCurrentTargetRequest(generation)) {
      targetLoading = false;
      renderTargetState();
    }
  }
}

function isCurrentTargetRequest(generation: number): boolean {
  return initialized && generation === targetRequestGeneration;
}

function mergeMemoryTargets(current: readonly MemoryTargetView[], next: readonly MemoryTargetView[]): MemoryTargetView[] {
  const merged = new Map(current.map((target) => [target.targetRef, target]));
  for (const target of next) merged.set(target.targetRef, target);
  return [...merged.values()];
}

function renderTargetState(): void {
  renderMemoryAdvancedFilters();
  renderTargetControls();
  renderMemoryTargets();
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
    if (item.capabilities.canDelete) actions.append(actionButton("永久删除", () => void deleteItem(item), "danger"));
  } else if (item.capabilities.canRestore) {
    actions.append(actionButton("恢复", () => void restoreItem(item)));
  }
  if (actions.childElementCount > 0) card.append(actions);
  return card;
}

function renderTargetControls(): void {
  const targetContainer = element("memory-targets", HTMLElement);
  targetContainer.replaceChildren();
  if (targetOptions.length === 0) {
    const message = document.createElement("p");
    message.className = "hint";
    message.textContent = targetLoading
      ? "正在加载授权范围…"
      : targetError
        ? "授权范围加载失败，请重试。"
        : "暂无可用授权范围。";
    targetContainer.append(message);
  }
  for (const target of targetOptions) {
    const row = document.createElement("div");
    row.className = "memory-target-row";
    const label = document.createElement("span");
    label.textContent = targetLabel(target);
    row.append(label);
    if (canClearTarget(target)) {
      const clear = actionButton("清空此范围", () => void confirmTargetOperation("clear_target", target), "danger");
      clear.disabled = memoryListState !== "ready";
      row.append(clear);
    }
    if (canDisableGroupProfile(target)) {
      const disable = actionButton("停止画像", () => void confirmTargetOperation("disable_group_profile", target), "danger");
      disable.disabled = memoryListState !== "ready";
      row.append(disable);
    }
    targetContainer.append(row);
  }
  if (targetError) {
    if (targetOptions.length > 0) {
      const message = document.createElement("p");
      message.className = "hint";
      message.textContent = "后续授权范围加载失败，请重试。";
      targetContainer.append(message);
    }
    const retry = actionButton(targetPage === 0 ? "重试加载范围" : "重试后续范围", () => {
      if (targetPage === 0) void refreshMemoryTargets(true);
      else void loadMoreMemoryTargets();
    });
    retry.id = "memory-target-retry";
    targetContainer.append(retry);
  } else if (targetPage < targetTotalPages) {
    const loadMore = actionButton(
      targetLoading ? "正在加载更多范围…" : `加载更多范围（已加载 ${targetOptions.length}/${targetTotal}）`,
      () => void loadMoreMemoryTargets(),
    );
    loadMore.id = "memory-target-load-more";
    loadMore.disabled = targetLoading;
    targetContainer.append(loadMore);
  }
}

function renderMemoryTargets(): void {
  const select = element("memory-create-target", HTMLSelectElement);
  const previousTarget = select.value;
  const creatableTargets = targetOptions.filter(canCreateMemory);
  setMemoryCreateControlsDisabled(memoryListState !== "ready");
  const placeholder = targetLoading && targetOptions.length === 0
    ? "加载范围中…"
    : targetError && targetOptions.length === 0
      ? "范围加载失败"
      : creatableTargets.length > 0
        ? "选择已授权范围…"
        : targetPage < targetTotalPages
          ? "请加载更多范围…"
          : "暂无可用范围";
  select.replaceChildren(new Option(placeholder, ""));
  for (const target of creatableTargets) {
    select.append(new Option(targetLabel(target), target.targetRef));
  }
  select.value = creatableTargets.some((target) => target.targetRef === previousTarget) ? previousTarget : "";
  select.disabled = memoryListState !== "ready" || creatableTargets.length === 0;
  updateCreateVisibilityOptions();
}

function renderMemoryAdvancedFilters(): void {
  renderOpaqueRefFilter("memory-account-filter", "全部账号", uniqueRefs(targetOptions.map((target) => target.accountRef)));
  renderOpaqueRefFilter("memory-group-filter", "全部群组", uniqueRefs(targetOptions.flatMap((target) => target.groupRef ? [target.groupRef] : [])));
  renderOpaqueRefFilter("memory-user-filter", "全部用户", uniqueRefs(targetOptions.flatMap((target) => target.subjectRef ? [target.subjectRef] : [])));
}

function renderOpaqueRefFilter(id: string, emptyLabel: string, refs: readonly string[]): void {
  const select = element(id, HTMLSelectElement);
  const previous = select.value;
  // 刷新 target 时先清空已加载页，但当前 opaque 筛选仍必须保留；否则
  // 列表请求会在 target 响应返回前丢失用户选择，尤其是第二页 target。
  const visibleRefs = uniqueRefs(previous ? [...refs, previous] : refs);
  select.replaceChildren(new Option(emptyLabel, ""), ...visibleRefs.map((ref) => new Option(ref, ref)));
  select.value = visibleRefs.includes(previous) ? previous : "";
  select.disabled = refs.length === 0 && !previous;
}

function uniqueRefs(refs: readonly string[]): string[] {
  return [...new Set(refs)];
}

function updateCreateVisibilityOptions(): void {
  const targetSelect = element("memory-create-target", HTMLSelectElement);
  const visibilitySelect = element("memory-create-visibility", HTMLSelectElement);
  const target = targetOptions.find((option) => option.targetRef === targetSelect.value);
  const options = target ? VISIBILITY_OPTIONS[target.scope] : [];
  const previous = visibilitySelect.value;
  visibilitySelect.replaceChildren(...options.map(([value, label]) => new Option(label, value)));
  visibilitySelect.disabled = memoryListState !== "ready" || options.length === 0;
  if (options.some(([value]) => value === previous)) {
    visibilitySelect.value = previous;
  } else if (options[0] !== undefined) {
    visibilitySelect.value = options[0][0];
  }
}

async function submitCreate(form: HTMLFormElement): Promise<void> {
  if (memoryListState !== "ready") {
    setResult("Memory 列表当前不可用，请刷新后重试", true);
    return;
  }
  const targetRef = element("memory-create-target", HTMLSelectElement).value;
  const content = element("memory-create-content", HTMLTextAreaElement).value.trim();
  const category = asMemoryCategory(element("memory-create-type", HTMLSelectElement).value);
  const visibility = asMemoryVisibility(element("memory-create-visibility", HTMLSelectElement).value);
  const pinned = element("memory-create-pinned", HTMLInputElement).checked;
  const target = targetOptions.find((option) => option.targetRef === targetRef);
  if (!targetRef || !target || !canCreateMemory(target) || !content || category === null || visibility === null) {
    setResult("创建需要选择范围、有效可见性并填写内容", true);
    return;
  }
  if (!VISIBILITY_OPTIONS[target.scope].some(([value]) => value === visibility)) {
    setResult("当前范围不支持所选可见性", true);
    return;
  }
  const submit = form.querySelector<HTMLButtonElement>("button[type=submit]");
  if (submit) submit.disabled = true;
  const generation = lifecycleGeneration;
  try {
    await createMemory({ targetRef, content, category, visibility, pinned });
    if (!isCurrentLifecycle(generation)) return;
    form.reset();
    setResult("Memory 已由服务端确认创建", false);
    await refreshMemories();
  } catch (cause) {
    if (!isCurrentLifecycle(generation)) return;
    setResult(cause instanceof Error ? cause.message : "Memory 创建失败", true);
  } finally {
    if (submit) submit.disabled = false;
  }
}

async function editMemory(item: MemoryItem): Promise<void> {
  const generation = lifecycleGeneration;
  try {
    const latest = await getMemory(item.target.targetRef, item.memoryRef);
    if (!isCurrentLifecycle(generation)) return;
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
    if (!isCurrentLifecycle(generation)) return;
    setResult("Memory 已由服务端确认更新", false);
    await refreshMemories();
  } catch (cause) {
    if (!isCurrentLifecycle(generation)) return;
    setResult(cause instanceof Error ? cause.message : "Memory 更新失败", true);
  }
}

async function deleteItem(item: MemoryItem): Promise<void> {
  const generation = lifecycleGeneration;
  try {
    const confirmation = await prepareMemoryOperation({
      operation: "delete_memory",
      targetRef: item.target.targetRef,
      memoryRef: item.memoryRef,
      expectedVersion: item.version,
    });
    if (!isCurrentLifecycle(generation)) return;
    if (!window.confirm(`确定永久删除这条 Memory 吗？删除后无法恢复。服务端已准备确认。`)) return;
    const result = await commitMemoryOperation({
      operation: confirmation.operation,
      targetRef: item.target.targetRef,
      confirmationToken: confirmation.confirmationToken,
    });
    if (!isCurrentLifecycle(generation)) return;
    if (result.deleted !== true || result.memoryRef !== item.memoryRef) {
      throw new Error("Memory 删除结果无法与原记录确认");
    }
    setResult("Memory 已由服务端确认删除", false);
    // 删除最后一条记录后 target 可能从服务端 discovery 消失，及时丢弃旧创建范围。
    const params = readParams();
    void refreshMemoryTargets(true);
    await refreshMemories(params);
  } catch (cause) {
    if (!isCurrentLifecycle(generation)) return;
    setResult(cause instanceof Error ? cause.message : "Memory 删除失败", true);
  }
}

async function archiveItem(item: MemoryItem): Promise<void> {
  const generation = lifecycleGeneration;
  if (!window.confirm("确定归档这条 Memory 吗？归档后仍可恢复。")) return;
  try {
    await archiveMemory({
      targetRef: item.target.targetRef,
      memoryRef: item.memoryRef,
      expectedVersion: item.version,
    });
    if (!isCurrentLifecycle(generation)) return;
    setResult("Memory 已由服务端确认归档", false);
    await refreshMemories();
  } catch (cause) {
    if (!isCurrentLifecycle(generation)) return;
    setResult(cause instanceof Error ? cause.message : "Memory 归档失败", true);
  }
}

async function confirmTargetOperation(operation: MemoryOperation, target: MemoryTargetView): Promise<void> {
  const generation = lifecycleGeneration;
  if (memoryListState !== "ready") {
    setResult("Memory 列表当前不可用，请刷新后重试", true);
    return;
  }
  try {
    const confirmation = await prepareMemoryOperation({ operation, targetRef: target.targetRef });
    if (!isCurrentLifecycle(generation)) return;
    const noun = operation === "disable_group_profile" ? "停止画像并归档" : "清空";
    if (!window.confirm(`确定${noun} ${confirmation.affectedCount} 条 Memory 吗？此操作需要服务端确认。`)) return;
    const result = await commitMemoryOperation({
      operation,
      targetRef: target.targetRef,
      confirmationToken: confirmation.confirmationToken,
    });
    if (!isCurrentLifecycle(generation)) return;
    targetOptions = targetOptions.map((current) => current.targetRef === result.target.targetRef ? result.target : current);
    renderTargetState();
    setResult(`服务端已完成${noun}：${result.affectedCount} 条`, false);
    await refreshMemories();
  } catch (cause) {
    if (!isCurrentLifecycle(generation)) return;
    setResult(cause instanceof Error ? cause.message : "Memory 操作失败", true);
  }
}

async function restoreItem(item: MemoryItem): Promise<void> {
  const generation = lifecycleGeneration;
  try {
    await restoreMemory({
      targetRef: item.target.targetRef,
      memoryRef: item.memoryRef,
      expectedVersion: item.version,
    });
    if (!isCurrentLifecycle(generation)) return;
    setResult("Memory 已由服务端确认恢复", false);
    await refreshMemories();
  } catch (cause) {
    if (!isCurrentLifecycle(generation)) return;
    setResult(cause instanceof Error ? cause.message : "Memory 恢复失败", true);
  }
}

function renderPagination(): void {
  const target = element("memory-pagination", HTMLElement);
  target.replaceChildren();
  if (memoryListState !== "ready" || currentPage.totalPages <= 1) return;
  const previous = actionButton("上一页", () => { if (page > 1) { page -= 1; void refreshMemories(); } });
  previous.disabled = page <= 1;
  const next = actionButton("下一页", () => { if (page < currentPage.totalPages) { page += 1; void refreshMemories(); } });
  next.disabled = page >= currentPage.totalPages;
  const label = document.createElement("span");
  label.textContent = `第 ${page} / ${currentPage.totalPages} 页 · ${currentPage.total} 条`;
  target.append(previous, label, next);
}

function renderLoading(): void {
  memoryListState = "loading";
  element("memory-list", HTMLElement).replaceChildren(Object.assign(document.createElement("p"), { className: "hint", textContent: "正在加载 Memory…" }));
  clearMemoryPagination();
  renderTargetState();
}

function renderError(): void {
  memoryListState = "error";
  const target = element("memory-list", HTMLElement);
  const retry = actionButton("重试", () => void refreshMemories());
  target.replaceChildren(Object.assign(document.createElement("p"), { className: "hint", textContent: "Memory 列表加载失败，请检查权限或重试。" }), retry);
  clearMemoryPagination();
  renderTargetState();
}

function clearMemoryPagination(): void {
  const target = element("memory-pagination", HTMLElement);
  for (const child of target.children) {
    if (child instanceof HTMLButtonElement) {
      child.disabled = true;
      child.onclick = null;
    }
  }
  target.replaceChildren();
}

function setMemoryCreateControlsDisabled(disabled: boolean): void {
  for (const id of [
    "memory-create-target",
    "memory-create-type",
    "memory-create-visibility",
    "memory-create-content",
    "memory-create-pinned",
  ]) {
    const field = document.getElementById(id);
    if (field instanceof HTMLInputElement || field instanceof HTMLSelectElement || field instanceof HTMLTextAreaElement) {
      field.disabled = disabled;
    }
  }
  const form = document.getElementById("memory-create-form");
  if (form instanceof HTMLFormElement) {
    const submit = form.querySelector<HTMLButtonElement>("button[type=submit]");
    if (submit) submit.disabled = disabled;
    form.setAttribute("aria-disabled", String(disabled));
  }
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

function canClearTarget(target: MemoryTargetView): boolean {
  return target.capabilities.canClearTarget;
}

function canDisableGroupProfile(target: MemoryTargetView): boolean {
  return target.scope === "group_profile" && target.capabilities.canDisableGroupProfile;
}

function canCreateMemory(target: MemoryTargetView): boolean {
  return target.scope !== "group_profile" || target.capabilities.canDisableGroupProfile;
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
