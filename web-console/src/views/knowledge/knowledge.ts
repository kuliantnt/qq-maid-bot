import { fetchKnowledgeCapabilities, listKnowledgeFiles } from "../../api.js";
import { requiredElement, setText } from "../../dom.js";
import { renderKnowledgeEmpty, renderKnowledgeList } from "./knowledge-list.js";
import { appendKnowledgePage, hasMoreKnowledgePages, initialKnowledgePager } from "./knowledge-paging.js";
import type { KnowledgeFileCapabilities, KnowledgeFileItem, KnowledgeFileListParams } from "../../types.js";
import type { KnowledgePager } from "./knowledge-paging.js";

type KnowledgeRefreshReason = "refresh" | "upload" | "retry" | "delete" | "filter";

let capabilities: KnowledgeFileCapabilities | null = null;
let pager: KnowledgePager = initialKnowledgePager();
let loadedItems: KnowledgeFileItem[] = [];
let currentParams: KnowledgeFileListParams = defaultKnowledgeParams();
let uploadHandler: (() => void) | null = null;

export async function initializeKnowledge(): Promise<void> {
  bindKnowledgeControls();
  try {
    capabilities = await fetchKnowledgeCapabilities();
  } catch (cause) {
    showKnowledgeError(cause, "知识库能力加载失败");
  }
  await refreshKnowledgeList("refresh");
}

export function setKnowledgeUploadHandler(handler: () => void): void {
  uploadHandler = handler;
}

export function getKnowledgeCapabilities(): KnowledgeFileCapabilities | null {
  return capabilities;
}

export async function refreshKnowledgeList(reason: KnowledgeRefreshReason): Promise<void> {
  const reset = reason === "refresh" || reason === "filter";
  if (reset) {
    currentParams = { ...currentParams, page: 1 };
    pager = initialKnowledgePager();
    loadedItems = [];
  }
  if (loadedItems.length === 0) renderKnowledgeLoading();
  try {
    const page = await listKnowledgeFiles({ ...currentParams, page: 1 });
    currentParams = { ...currentParams, page: page.page };
    pager = appendKnowledgePage(initialKnowledgePager(), page);
    loadedItems = [...page.items];
    renderKnowledgeContent();
  } catch (cause) {
    showKnowledgeError(cause, "知识库列表加载失败");
  }
}

function bindKnowledgeControls(): void {
  const search = requiredElement("knowledge-search", HTMLInputElement);
  const status = requiredElement("knowledge-status-filter", HTMLSelectElement);
  const submit = requiredElement("knowledge-filter-submit", HTMLButtonElement);
  const reset = requiredElement("knowledge-filter-reset", HTMLButtonElement);
  const refresh = requiredElement("knowledge-refresh", HTMLButtonElement);
  const upload = requiredElement("knowledge-upload-open", HTMLButtonElement);
  const apply = () => { syncKnowledgeFilters(search, status); void refreshKnowledgeList("filter"); };
  submit.onclick = apply;
  reset.onclick = () => { search.value = ""; status.value = "all"; syncKnowledgeFilters(search, status); void refreshKnowledgeList("filter"); };
  refresh.onclick = () => void refreshKnowledgeList("refresh");
  upload.onclick = () => uploadHandler?.();
  search.addEventListener("keydown", (event) => { if (event.key === "Enter") { event.preventDefault(); apply(); } });
}

function syncKnowledgeFilters(search: HTMLInputElement, status: HTMLSelectElement): void {
  currentParams = { ...currentParams, page: 1, search: search.value.trim(), status: knowledgeStatusValue(status.value) };
}

function renderKnowledgeLoading(): void {
  const target = requiredElement("knowledge-list", HTMLElement);
  const hint = document.createElement("p");
  hint.className = "hint";
  hint.textContent = "正在加载知识库…";
  target.replaceChildren(hint);
}

function renderKnowledgeContent(): void {
  const target = requiredElement("knowledge-list", HTMLElement);
  if (loadedItems.length === 0) renderKnowledgeEmpty(target);
  else renderKnowledgeList(target, loadedItems, {});
  renderKnowledgePagination();
}

function renderKnowledgePagination(): void {
  const target = requiredElement("knowledge-pagination", HTMLElement);
  target.replaceChildren();
  if (!hasMoreKnowledgePages(pager)) return;
  const button = document.createElement("button");
  button.type = "button";
  button.className = "secondary";
  button.textContent = "加载更多";
  button.onclick = () => void loadMoreKnowledgeFiles();
  target.append(button);
}

async function loadMoreKnowledgeFiles(): Promise<void> {
  if (!hasMoreKnowledgePages(pager)) return;
  try {
    currentParams = { ...currentParams, page: currentParams.page + 1 };
    const page = await listKnowledgeFiles(currentParams);
    pager = appendKnowledgePage(pager, page);
    loadedItems = [...loadedItems, ...page.items];
    renderKnowledgeContent();
  } catch (cause) {
    showKnowledgeError(cause, "知识库列表加载失败");
  }
}

function showKnowledgeError(cause: unknown, fallback: string): void {
  setText("knowledge-result", cause instanceof Error ? cause.message : fallback);
}

function defaultKnowledgeParams(): KnowledgeFileListParams {
  return { page: 1, page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" };
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
