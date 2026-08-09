export function initialKnowledgePager() {
    return { page: 1, totalPages: 0, loadedCount: 0, hasMore: false };
}
export function appendKnowledgePage(pager, page) {
    return {
        page: page.page,
        totalPages: page.total_pages,
        loadedCount: pager.loadedCount + page.items.length,
        hasMore: page.page < page.total_pages,
    };
}
export function hasMoreKnowledgePages(pager) {
    return pager.hasMore;
}
export function pageAfterKnowledgeDelete(pager, deletedCount) {
    return { ...pager, loadedCount: Math.max(0, pager.loadedCount - deletedCount) };
}
