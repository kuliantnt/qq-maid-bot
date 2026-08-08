import { deleteKnowledgeFile, downloadKnowledgeFile, fetchKnowledgeCapabilities, listKnowledgeFiles, retryKnowledgeFile, uploadKnowledgeFile } from "../../api.js";
import { requiredElement, setText } from "../../dom.js";
import { renderKnowledgeEmpty, renderKnowledgeList } from "./knowledge-list.js";
import { createKnowledgeActionHandlers, triggerBrowserDownload } from "./knowledge-actions.js";
import { appendKnowledgePage, hasMoreKnowledgePages, initialKnowledgePager, type KnowledgePager } from "./knowledge-paging.js";
import { KnowledgePollingController } from "./knowledge-polling.js";
import type { KnowledgeFileCapabilities, KnowledgeFileItem, KnowledgeFileListParams, KnowledgeFilePage } from "../../types.js";
import { installKnowledgeUpload } from "./knowledge-upload.js";

type KnowledgeRefreshReason = "refresh" | "upload" | "retry" | "delete" | "filter";
type KnowledgeFilterParams = Omit<KnowledgeFileListParams, "page">;

let capabilities: KnowledgeFileCapabilities | null = null;
let pager: KnowledgePager = initialKnowledgePager();
let loadedPages = 0;
let itemsByPage = new Map<number, KnowledgeFileItem[]>();
let allItems: KnowledgeFileItem[] = [];
let filterParams: KnowledgeFilterParams = defaultKnowledgeParams();
let uploadFlowInstalled = false;
let loadMoreInFlight = false;
// 用户主动请求与轮询拥有不同的生命周期，轮询不能让刷新/分页请求失效。
let listRequestGeneration = 0;
let knowledgeInitialized = false;
let controlsBound = false;
let documentListenersBound = false;

const onVisibilityChange = () => {
  if (document.visibilityState !== "hidden" && polling.hasActive()) polling.notifyChange();
};
const onPageHide = () => polling.stop();
const onPageShow = () => {
  if (knowledgeInitialized && document.visibilityState !== "hidden" && polling.hasActive()) polling.notifyChange();
};
const onSearchKeydown = (event: KeyboardEvent) => {
  if (event.key === "Enter") {
    event.preventDefault();
    const search = requiredElement("knowledge-search", HTMLInputElement);
    const status = requiredElement("knowledge-status-filter", HTMLSelectElement);
    syncKnowledgeFilters(search, status);
    void refreshKnowledgeList("filter");
  }
};

const polling = new KnowledgePollingController({
  isVisible: () => typeof document === "undefined" || document.visibilityState !== "hidden",
  setTimeout: (fn, ms) => window.setTimeout(fn, ms),
  clearTimeout: (id) => window.clearTimeout(id),
  fetchPages: (params, pageCount) => {
    // 用户请求开始时会停止轮询；PollingController 自身还会丢弃停止前已经发出的响应。
    return Promise.all(Array.from({ length: pageCount }, (_, index) => listKnowledgeFiles({ ...params, page: index + 1 })));
  },
  onUpdate: (pages) => {
    applyKnowledgePages(pages);
    renderKnowledgeContent();
  },
  onTransientError: (message) => setText("knowledge-result", message),
  onTerminalTransition: (message) => setText("knowledge-result", message),
});

const actions = createKnowledgeActionHandlers({
  setStatus: (text) => setText("knowledge-result", text),
  download: downloadKnowledgeFile,
  triggerDownload: triggerBrowserDownload,
  deleteFile: deleteKnowledgeFile,
  retryFile: retryKnowledgeFile,
  refresh: (reason) => void refreshKnowledgeList(reason),
  getItems: () => allItems,
});

export async function initializeKnowledge(): Promise<void> {
  if (knowledgeInitialized) return;
  knowledgeInitialized = true;
  bindKnowledgeControls();
  bindDocumentListeners();
  try {
    capabilities = await fetchKnowledgeCapabilities();
  } catch (cause) {
    showKnowledgeError(cause, "知识库能力加载失败");
  }
  if (!uploadFlowInstalled) {
    installKnowledgeUpload({ inputId: "knowledge-upload-input", buttonId: "knowledge-upload-open", setStatus: (text) => setText("knowledge-result", text), getCapabilities: getKnowledgeCapabilities, upload: uploadKnowledgeFile, onUploaded: () => void refreshKnowledgeList("upload") });
    uploadFlowInstalled = true;
  }
  await refreshKnowledgeList("refresh");
}

export function disposeKnowledge(): void {
  polling.stop();
  listRequestGeneration += 1;
  if (documentListenersBound && typeof document !== "undefined" && typeof document.removeEventListener === "function") document.removeEventListener("visibilitychange", onVisibilityChange);
  if (documentListenersBound && typeof window !== "undefined" && typeof window.removeEventListener === "function") {
    window.removeEventListener("pagehide", onPageHide);
    window.removeEventListener("pageshow", onPageShow);
  }
  const search = typeof document === "undefined" ? null : document.getElementById("knowledge-search");
  if (typeof HTMLInputElement !== "undefined" && search instanceof HTMLInputElement) search.removeEventListener("keydown", onSearchKeydown);
  documentListenersBound = false;
  const input = typeof document === "undefined" ? null : document.getElementById("knowledge-upload-input");
  if (input && typeof input.remove === "function") input.remove();
  capabilities = null;
  pager = initialKnowledgePager();
  loadedPages = 0;
  itemsByPage.clear();
  allItems = [];
  filterParams = defaultKnowledgeParams();
  uploadFlowInstalled = false;
  knowledgeInitialized = false;
  controlsBound = false;
  loadMoreInFlight = false;
  if (typeof document !== "undefined") {
    document.getElementById("knowledge-list")?.replaceChildren();
    document.getElementById("knowledge-pagination")?.replaceChildren();
    if (document.getElementById("knowledge-result")) setText("knowledge-result", "");
  }
}

export function getKnowledgeCapabilities(): KnowledgeFileCapabilities | null {
  return capabilities;
}

export async function refreshKnowledgeList(reason: KnowledgeRefreshReason): Promise<void> {
  const generation = ++listRequestGeneration;
  // 上传、重试、删除都会改变总数或排序，继续复用旧的后续页会造成重复、遗漏或残留。
  // 所有刷新原因统一从第一页重新建立已加载状态。
  const preserveOnFailure = reason === "upload" || reason === "retry" || reason === "delete";
  const previous = {
    itemsByPage: new Map([...itemsByPage.entries()].map(([page, items]) => [page, [...items]])),
    loadedPages,
    pager: { ...pager },
    allItems: [...allItems],
  };
  const hadItems = allItems.length > 0;
  polling.stop();
  itemsByPage.clear();
  loadedPages = 0;
  allItems = [];
  pager = initialKnowledgePager();
  if (!preserveOnFailure || !hadItems) renderKnowledgeLoading();
  const params = { ...filterParams, page: 1 };
  try {
    const page = await listKnowledgeFiles(params);
    if (generation !== listRequestGeneration || !paramsMatch(params, { ...filterParams, page: 1 })) return;
    itemsByPage.set(1, [...page.items]);
    loadedPages = 1;
    rebuildLoadedState(page);
    syncKnowledgePolling();
    renderKnowledgeContent();
  } catch (cause) {
    if (generation !== listRequestGeneration) return;
    if (preserveOnFailure) {
      itemsByPage = previous.itemsByPage;
      loadedPages = previous.loadedPages;
      pager = previous.pager;
      allItems = previous.allItems;
      if (allItems.length > 0) syncKnowledgePolling();
      else renderKnowledgeLoadError();
      showKnowledgeError(cause, "知识库列表加载失败");
      return;
    }
    showKnowledgeError(cause, "知识库列表加载失败");
    if (allItems.length === 0) renderKnowledgeLoadError();
  }
}

function bindKnowledgeControls(): void {
  if (controlsBound) return;
  controlsBound = true;
  const search = requiredElement("knowledge-search", HTMLInputElement);
  const status = requiredElement("knowledge-status-filter", HTMLSelectElement);
  const submit = requiredElement("knowledge-filter-submit", HTMLButtonElement);
  const reset = requiredElement("knowledge-filter-reset", HTMLButtonElement);
  const refresh = requiredElement("knowledge-refresh", HTMLButtonElement);
  const apply = () => { syncKnowledgeFilters(search, status); void refreshKnowledgeList("filter"); };
  submit.onclick = apply;
  reset.onclick = () => { search.value = ""; status.value = "all"; syncKnowledgeFilters(search, status); void refreshKnowledgeList("filter"); };
  refresh.onclick = () => void refreshKnowledgeList("refresh");
  search.addEventListener("keydown", onSearchKeydown);
}

function bindDocumentListeners(): void {
  if (documentListenersBound) return;
  documentListenersBound = true;
  document.addEventListener("visibilitychange", onVisibilityChange);
  if (typeof window.addEventListener === "function") {
    window.addEventListener("pagehide", onPageHide);
    window.addEventListener("pageshow", onPageShow);
  }
}

function syncKnowledgeFilters(search: HTMLInputElement, status: HTMLSelectElement): void {
  filterParams = { ...filterParams, search: search.value.trim(), status: knowledgeStatusValue(status.value) };
}

function rebuildLoadedState(lastPage: KnowledgeFilePage): void {
  pager = appendKnowledgePage({ ...pager, loadedCount: 0 }, lastPage);
  pager = { ...pager, page: loadedPages, loadedCount: allPageItems().length, hasMore: loadedPages < lastPage.total_pages };
  allItems = allPageItems();
}

function applyKnowledgePages(pages: readonly KnowledgeFilePage[]): void {
  if (pages.length === 0) {
    itemsByPage.clear();
    loadedPages = 0;
    allItems = [];
    pager = initialKnowledgePager();
    return;
  }
  const lastPage = pages[pages.length - 1];
  if (lastPage === undefined) return;
  for (const page of pages) itemsByPage.set(page.page, [...page.items]);
  // 删除后总页数可能减少；清掉已经不存在的旧页，避免轮询再次把旧文件渲染出来。
  for (const pageNumber of itemsByPage.keys()) {
    if (pageNumber > lastPage.total_pages) itemsByPage.delete(pageNumber);
  }
  loadedPages = Math.min(
    Math.max(loadedPages, ...pages.map((page) => page.page)),
    Math.max(1, lastPage.total_pages),
  );
  rebuildLoadedState(lastPage);
}

function syncKnowledgePolling(): void {
  polling.setPages(allItems);
  polling.updateParams({ ...filterParams, page: loadedPages || 1 });
}

function allPageItems(): KnowledgeFileItem[] {
  return Array.from({ length: loadedPages }, (_, index) => itemsByPage.get(index + 1) ?? []).flat();
}

function paramsMatch(left: KnowledgeFileListParams, right: KnowledgeFileListParams): boolean {
  return left.page_size === right.page_size && left.search === right.search && left.status === right.status && left.sort === right.sort && left.order === right.order;
}

function renderKnowledgeLoading(): void {
  const hint = document.createElement("p");
  hint.className = "hint";
  hint.textContent = "正在加载知识库…";
  requiredElement("knowledge-list", HTMLElement).replaceChildren(hint);
}

function renderKnowledgeLoadError(): void {
  const target = requiredElement("knowledge-list", HTMLElement);
  const hint = document.createElement("p");
  hint.className = "hint";
  hint.textContent = "知识库列表加载失败";
  const retry = document.createElement("button");
  retry.type = "button";
  retry.textContent = "重试";
  retry.onclick = () => void refreshKnowledgeList("refresh");
  target.replaceChildren(hint, retry);
}

function renderKnowledgeContent(): void {
  const target = requiredElement("knowledge-list", HTMLElement);
  if (allItems.length === 0) renderKnowledgeEmpty(target);
  else renderKnowledgeList(target, allItems, actions);
  renderKnowledgePagination();
}

function renderKnowledgePagination(): void {
  const target = requiredElement("knowledge-pagination", HTMLElement);
  target.replaceChildren();
  if (!hasMoreKnowledgePages(pager)) return;
  const button = document.createElement("button");
  button.type = "button";
  button.className = "secondary";
  button.textContent = loadMoreInFlight ? "加载中…" : "加载更多";
  button.disabled = loadMoreInFlight;
  button.onclick = () => void loadMoreKnowledgeFiles();
  target.append(button);
}

async function loadMoreKnowledgeFiles(): Promise<void> {
  if (loadMoreInFlight || !hasMoreKnowledgePages(pager)) return;
  const previous = { loadedPages, pager: { ...pager }, allItems: [...allItems] };
  const generation = ++listRequestGeneration;
  const pageNumber = loadedPages + 1;
  const params = { ...filterParams, page: pageNumber };
  polling.stop();
  loadMoreInFlight = true;
  renderKnowledgePagination();
  try {
    const page = await listKnowledgeFiles(params);
    if (generation !== listRequestGeneration || !paramsMatch(params, { ...filterParams, page: pageNumber })) return;
    itemsByPage.set(pageNumber, [...page.items]);
    loadedPages = pageNumber;
    rebuildLoadedState(page);
    syncKnowledgePolling();
    renderKnowledgeContent();
  } catch (cause) {
    if (generation === listRequestGeneration) {
      loadedPages = previous.loadedPages;
      pager = previous.pager;
      allItems = previous.allItems;
      showKnowledgeError(cause, "知识库列表加载失败");
      renderKnowledgeContent();
      syncKnowledgePolling();
    }
  } finally {
    loadMoreInFlight = false;
    // 即使加载更多被刷新/筛选淘汰，也必须解锁当前分页控件；否则新列表会永久显示“加载中”。
    if (knowledgeInitialized) renderKnowledgePagination();
  }
}

function showKnowledgeError(cause: unknown, fallback: string): void {
  setText("knowledge-result", cause instanceof Error ? cause.message : fallback);
}

function defaultKnowledgeParams(): KnowledgeFilterParams {
  return { page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" };
}

function knowledgeStatusValue(value: string): KnowledgeFileListParams["status"] {
  switch (value) {
    case "pending":
    case "processing":
    case "ready":
    case "failed": return value;
    default: return "all";
  }
}
