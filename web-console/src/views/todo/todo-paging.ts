import type { TodoTargetOption, TodoTargetPage } from "../../types.js";

export const TARGET_PAGE_SIZE = 100;

export type TodoRefreshTrigger = "refresh" | "filter";

export interface TargetPager {
  items: readonly TodoTargetOption[];
  page: number;
  totalPages: number;
}

export function initialTargetPager(): TargetPager {
  return { items: [], page: 0, totalPages: 0 };
}

export function hasMoreTargetPages(pager: TargetPager): boolean {
  return pager.page < pager.totalPages;
}

export function appendTargetPage(pager: TargetPager, next: TodoTargetPage): TargetPager {
  return { items: [...pager.items, ...next.items], page: next.page, totalPages: next.totalPages };
}

export function initialRefreshPage(trigger: TodoRefreshTrigger, currentPage: number): number {
  return trigger === "filter" ? 1 : Math.max(1, currentPage);
}

export function pageAfterDelete(page: number, totalPages: number): number {
  return page > totalPages ? Math.max(1, totalPages) : page;
}
