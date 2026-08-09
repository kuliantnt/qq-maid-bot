import type { KnowledgeFilePage } from "../../types.js";

export type KnowledgePager = {
  page: number;
  totalPages: number;
  loadedCount: number;
  hasMore: boolean;
};

export function initialKnowledgePager(): KnowledgePager {
  return { page: 1, totalPages: 0, loadedCount: 0, hasMore: false };
}

export function appendKnowledgePage(pager: KnowledgePager, page: KnowledgeFilePage): KnowledgePager {
  return {
    page: page.page,
    totalPages: page.total_pages,
    loadedCount: pager.loadedCount + page.items.length,
    hasMore: page.page < page.total_pages,
  };
}

export function hasMoreKnowledgePages(pager: KnowledgePager): boolean {
  return pager.hasMore;
}

export function pageAfterKnowledgeDelete(pager: KnowledgePager, deletedCount: number): KnowledgePager {
  return { ...pager, loadedCount: Math.max(0, pager.loadedCount - deletedCount) };
}
