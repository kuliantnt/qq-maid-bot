import { deleteKnowledgeFile, downloadKnowledgeFile, fetchKnowledgeCapabilities, listKnowledgeFiles, retryKnowledgeFile, uploadKnowledgeFile } from "../../api.js";
import { requiredElement, setText } from "../../dom.js";
import { renderKnowledgeEmpty, renderKnowledgeList } from "./knowledge-list.js";
import { createKnowledgeActionHandlers, triggerBrowserDownload } from "./knowledge-actions.js";
import { appendKnowledgePage, hasMoreKnowledgePages, initialKnowledgePager } from "./knowledge-paging.js";
import { KnowledgePollingController } from "./knowledge-polling.js";
import { installKnowledgeUpload } from "./knowledge-upload.js";
let capabilities = null;
let pager = initialKnowledgePager();
let loadedPages = 0;
let itemsByPage = new Map();
let allItems = [];
let filterParams = defaultKnowledgeParams();
let uploadFlowInstalled = false;
let loadMoreInFlight = false;
let requestGeneration = 0;
let knowledgeInitialized = false;
let controlsBound = false;
let documentListenersBound = false;
const onVisibilityChange = () => {
    if (document.visibilityState !== "hidden" && polling.hasActive())
        polling.notifyChange();
};
const onPageHide = () => polling.stop();
const onSearchKeydown = (event) => {
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
        const generation = ++requestGeneration;
        return Promise.all(Array.from({ length: pageCount }, (_, index) => listKnowledgeFiles({ ...params, page: index + 1 }))).then((pages) => {
            if (generation !== requestGeneration || !paramsMatch({ ...params, page: 1 }, { ...filterParams, page: 1 }))
                return [];
            return pages;
        });
    },
    onUpdate: (pages) => {
        if (pages.length === 0)
            return;
        for (const page of pages)
            itemsByPage.set(page.page, [...page.items]);
        const lastPage = pages[pages.length - 1];
        if (lastPage === undefined)
            return;
        rebuildLoadedState(lastPage);
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
export async function initializeKnowledge() {
    if (knowledgeInitialized)
        return;
    knowledgeInitialized = true;
    bindKnowledgeControls();
    bindDocumentListeners();
    try {
        capabilities = await fetchKnowledgeCapabilities();
    }
    catch (cause) {
        showKnowledgeError(cause, "知识库能力加载失败");
    }
    if (!uploadFlowInstalled) {
        installKnowledgeUpload({ inputId: "knowledge-upload-input", buttonId: "knowledge-upload-open", setStatus: (text) => setText("knowledge-result", text), getCapabilities: getKnowledgeCapabilities, upload: uploadKnowledgeFile, onUploaded: () => void refreshKnowledgeList("upload") });
        uploadFlowInstalled = true;
    }
    await refreshKnowledgeList("refresh");
    polling.start({ ...filterParams, page: loadedPages || 1 });
}
export function disposeKnowledge() {
    polling.stop();
    requestGeneration += 1;
    if (documentListenersBound && typeof document !== "undefined" && typeof document.removeEventListener === "function")
        document.removeEventListener("visibilitychange", onVisibilityChange);
    if (documentListenersBound && typeof window !== "undefined" && typeof window.removeEventListener === "function")
        window.removeEventListener("pagehide", onPageHide);
    const search = typeof document === "undefined" ? null : document.getElementById("knowledge-search");
    if (typeof HTMLInputElement !== "undefined" && search instanceof HTMLInputElement)
        search.removeEventListener("keydown", onSearchKeydown);
    documentListenersBound = false;
    const input = typeof document === "undefined" ? null : document.getElementById("knowledge-upload-input");
    if (input && typeof input.remove === "function")
        input.remove();
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
        if (document.getElementById("knowledge-result"))
            setText("knowledge-result", "");
    }
}
export function getKnowledgeCapabilities() {
    return capabilities;
}
export async function refreshKnowledgeList(reason) {
    const generation = ++requestGeneration;
    const reset = reason === "refresh" || reason === "filter";
    if (reset) {
        itemsByPage.clear();
        loadedPages = 0;
        allItems = [];
        pager = initialKnowledgePager();
    }
    if (allItems.length === 0)
        renderKnowledgeLoading();
    const params = { ...filterParams, page: 1 };
    try {
        const page = await listKnowledgeFiles(params);
        if (generation !== requestGeneration || !paramsMatch(params, { ...filterParams, page: 1 }))
            return;
        itemsByPage.set(1, [...page.items]);
        loadedPages = reset ? 1 : Math.max(loadedPages, 1);
        rebuildLoadedState(page);
        polling.updateParams({ ...filterParams, page: loadedPages });
        polling.setPages(allItems);
        renderKnowledgeContent();
        polling.notifyChange();
    }
    catch (cause) {
        if (generation !== requestGeneration)
            return;
        showKnowledgeError(cause, "知识库列表加载失败");
        if (allItems.length === 0)
            renderKnowledgeLoadError();
    }
}
function bindKnowledgeControls() {
    if (controlsBound)
        return;
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
function bindDocumentListeners() {
    if (documentListenersBound)
        return;
    documentListenersBound = true;
    document.addEventListener("visibilitychange", onVisibilityChange);
    if (typeof window.addEventListener === "function")
        window.addEventListener("pagehide", onPageHide);
}
function syncKnowledgeFilters(search, status) {
    requestGeneration += 1;
    filterParams = { ...filterParams, search: search.value.trim(), status: knowledgeStatusValue(status.value) };
}
function rebuildLoadedState(lastPage) {
    pager = appendKnowledgePage({ ...pager, loadedCount: 0 }, lastPage);
    pager = { ...pager, page: loadedPages, loadedCount: allPageItems().length, hasMore: loadedPages < lastPage.total_pages };
    allItems = allPageItems();
}
function allPageItems() {
    return Array.from({ length: loadedPages }, (_, index) => itemsByPage.get(index + 1) ?? []).flat();
}
function paramsMatch(left, right) {
    return left.page_size === right.page_size && left.search === right.search && left.status === right.status && left.sort === right.sort && left.order === right.order;
}
function renderKnowledgeLoading() {
    const hint = document.createElement("p");
    hint.className = "hint";
    hint.textContent = "正在加载知识库…";
    requiredElement("knowledge-list", HTMLElement).replaceChildren(hint);
}
function renderKnowledgeLoadError() {
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
function renderKnowledgeContent() {
    const target = requiredElement("knowledge-list", HTMLElement);
    if (allItems.length === 0)
        renderKnowledgeEmpty(target);
    else
        renderKnowledgeList(target, allItems, actions);
    renderKnowledgePagination();
}
function renderKnowledgePagination() {
    const target = requiredElement("knowledge-pagination", HTMLElement);
    target.replaceChildren();
    if (!hasMoreKnowledgePages(pager))
        return;
    const button = document.createElement("button");
    button.type = "button";
    button.className = "secondary";
    button.textContent = loadMoreInFlight ? "加载中…" : "加载更多";
    button.disabled = loadMoreInFlight;
    button.onclick = () => void loadMoreKnowledgeFiles();
    target.append(button);
}
async function loadMoreKnowledgeFiles() {
    if (loadMoreInFlight || !hasMoreKnowledgePages(pager))
        return;
    const previous = { loadedPages, pager: { ...pager }, allItems: [...allItems] };
    const generation = ++requestGeneration;
    const pageNumber = loadedPages + 1;
    const params = { ...filterParams, page: pageNumber };
    loadMoreInFlight = true;
    renderKnowledgePagination();
    try {
        const page = await listKnowledgeFiles(params);
        if (generation !== requestGeneration || !paramsMatch(params, { ...filterParams, page: pageNumber }))
            return;
        itemsByPage.set(pageNumber, [...page.items]);
        loadedPages = pageNumber;
        rebuildLoadedState(page);
        polling.updateParams({ ...filterParams, page: loadedPages });
        polling.setPages(allItems);
        renderKnowledgeContent();
        polling.notifyChange();
    }
    catch (cause) {
        if (generation === requestGeneration) {
            loadedPages = previous.loadedPages;
            pager = previous.pager;
            allItems = previous.allItems;
            showKnowledgeError(cause, "知识库列表加载失败");
            renderKnowledgeContent();
        }
    }
    finally {
        loadMoreInFlight = false;
        if (generation === requestGeneration)
            renderKnowledgePagination();
    }
}
function showKnowledgeError(cause, fallback) {
    setText("knowledge-result", cause instanceof Error ? cause.message : fallback);
}
function defaultKnowledgeParams() {
    return { page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" };
}
function knowledgeStatusValue(value) {
    switch (value) {
        case "pending":
        case "processing":
        case "ready":
        case "failed": return value;
        default: return "all";
    }
}
