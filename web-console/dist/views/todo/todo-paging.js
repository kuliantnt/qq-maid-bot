export const TARGET_PAGE_SIZE = 100;
export function initialTargetPager() {
    return { items: [], page: 0, totalPages: 0 };
}
export function hasMoreTargetPages(pager) {
    return pager.page < pager.totalPages;
}
export function appendTargetPage(pager, next) {
    return { items: [...pager.items, ...next.items], page: next.page, totalPages: next.totalPages };
}
export function initialRefreshPage(trigger, currentPage) {
    return trigger === "filter" ? 1 : Math.max(1, currentPage);
}
export function pageAfterDelete(page, totalPages) {
    return page > totalPages ? Math.max(1, totalPages) : page;
}
