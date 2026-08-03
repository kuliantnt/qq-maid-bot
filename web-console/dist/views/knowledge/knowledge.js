import { deleteKnowledgeFile, downloadKnowledgeFile, fetchKnowledgeCapabilities, listKnowledgeFiles, retryKnowledgeFile, uploadKnowledgeFile } from "../../api.js";
import { requiredElement, setText } from "../../dom.js";
import { renderKnowledgeEmpty, renderKnowledgeList } from "./knowledge-list.js";
import { createKnowledgeActionHandlers, triggerBrowserDownload } from "./knowledge-actions.js";
import { appendKnowledgePage, hasMoreKnowledgePages, initialKnowledgePager } from "./knowledge-paging.js";
import { KnowledgePollingController } from "./knowledge-polling.js";
import { installKnowledgeUpload } from "./knowledge-upload.js";
let capabilities = null;
let pager = initialKnowledgePager();
let loadedItems = [];
let currentParams = defaultKnowledgeParams();
let uploadFlowInstalled = false;
const polling = new KnowledgePollingController({
    isVisible: () => typeof document === "undefined" || document.visibilityState !== "hidden",
    setTimeout: (fn, ms) => window.setTimeout(fn, ms),
    clearTimeout: (id) => window.clearTimeout(id),
    fetchPage: (params) => listKnowledgeFiles({ ...params, page: 1 }),
    onUpdate: (page) => {
        currentParams = { ...currentParams, page: page.page };
        pager = appendKnowledgePage(initialKnowledgePager(), page);
        loadedItems = [...page.items];
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
    getItems: () => loadedItems,
});
export async function initializeKnowledge() {
    bindKnowledgeControls();
    try {
        capabilities = await fetchKnowledgeCapabilities();
    }
    catch (cause) {
        showKnowledgeError(cause, "知识库能力加载失败");
    }
    if (!uploadFlowInstalled) {
        installKnowledgeUpload({
            inputId: "knowledge-upload-input",
            buttonId: "knowledge-upload-open",
            setStatus: (text) => setText("knowledge-result", text),
            getCapabilities: getKnowledgeCapabilities,
            upload: uploadKnowledgeFile,
            onUploaded: () => void refreshKnowledgeList("upload"),
        });
        uploadFlowInstalled = true;
    }
    await refreshKnowledgeList("refresh");
    polling.start(currentParams);
    if (typeof document !== "undefined") {
        document.addEventListener("visibilitychange", () => {
            if (document.visibilityState !== "hidden" && polling.hasActive())
                polling.notifyChange();
        });
    }
    if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
        window.addEventListener("pagehide", () => polling.stop());
    }
}
export function getKnowledgeCapabilities() {
    return capabilities;
}
export async function refreshKnowledgeList(reason) {
    const reset = reason === "refresh" || reason === "filter";
    if (reset) {
        currentParams = { ...currentParams, page: 1 };
        pager = initialKnowledgePager();
        loadedItems = [];
    }
    if (loadedItems.length === 0)
        renderKnowledgeLoading();
    try {
        const page = await listKnowledgeFiles({ ...currentParams, page: 1 });
        currentParams = { ...currentParams, page: page.page };
        pager = appendKnowledgePage(initialKnowledgePager(), page);
        loadedItems = [...page.items];
        polling.setPages(loadedItems);
        polling.notifyChange();
        renderKnowledgeContent();
    }
    catch (cause) {
        showKnowledgeError(cause, "知识库列表加载失败");
    }
}
function bindKnowledgeControls() {
    const search = requiredElement("knowledge-search", HTMLInputElement);
    const status = requiredElement("knowledge-status-filter", HTMLSelectElement);
    const submit = requiredElement("knowledge-filter-submit", HTMLButtonElement);
    const reset = requiredElement("knowledge-filter-reset", HTMLButtonElement);
    const refresh = requiredElement("knowledge-refresh", HTMLButtonElement);
    const apply = () => { syncKnowledgeFilters(search, status); void refreshKnowledgeList("filter"); };
    submit.onclick = apply;
    reset.onclick = () => { search.value = ""; status.value = "all"; syncKnowledgeFilters(search, status); void refreshKnowledgeList("filter"); };
    refresh.onclick = () => void refreshKnowledgeList("refresh");
    search.addEventListener("keydown", (event) => { if (event.key === "Enter") {
        event.preventDefault();
        apply();
    } });
}
function syncKnowledgeFilters(search, status) {
    currentParams = { ...currentParams, page: 1, search: search.value.trim(), status: knowledgeStatusValue(status.value) };
}
function renderKnowledgeLoading() {
    const target = requiredElement("knowledge-list", HTMLElement);
    const hint = document.createElement("p");
    hint.className = "hint";
    hint.textContent = "正在加载知识库…";
    target.replaceChildren(hint);
}
function renderKnowledgeContent() {
    const target = requiredElement("knowledge-list", HTMLElement);
    if (loadedItems.length === 0)
        renderKnowledgeEmpty(target);
    else
        renderKnowledgeList(target, loadedItems, actions);
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
    button.textContent = "加载更多";
    button.onclick = () => void loadMoreKnowledgeFiles();
    target.append(button);
}
async function loadMoreKnowledgeFiles() {
    if (!hasMoreKnowledgePages(pager))
        return;
    try {
        currentParams = { ...currentParams, page: currentParams.page + 1 };
        const page = await listKnowledgeFiles(currentParams);
        pager = appendKnowledgePage(pager, page);
        loadedItems = [...loadedItems, ...page.items];
        polling.setPages(loadedItems);
        polling.notifyChange();
        renderKnowledgeContent();
    }
    catch (cause) {
        showKnowledgeError(cause, "知识库列表加载失败");
    }
}
function showKnowledgeError(cause, fallback) {
    setText("knowledge-result", cause instanceof Error ? cause.message : fallback);
}
function defaultKnowledgeParams() {
    return { page: 1, page_size: 20, search: "", status: "all", sort: "updated_at", order: "desc" };
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
