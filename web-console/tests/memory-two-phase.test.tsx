import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MemoryConfirmation, MemoryItem, MemoryTargetView } from "../src/types.js";

vi.mock("../src/memory-api.js", () => ({
  listMemories: vi.fn(),
  listMemoryTargets: vi.fn(),
  getMemory: vi.fn(),
  createMemory: vi.fn(),
  updateMemory: vi.fn(),
  archiveMemory: vi.fn(),
  restoreMemory: vi.fn(),
  prepareMemoryOperation: vi.fn(),
  commitMemoryOperation: vi.fn(),
}));

import {
  commitMemoryOperation,
  listMemories,
  listMemoryTargets,
  prepareMemoryOperation,
} from "../src/memory-api.js";
import { MemoryPage } from "../src/features/memory/memory-page.js";

const personalTarget: MemoryTargetView = {
  targetRef: "qq:personal:user-1",
  scope: "personal",
  platform: "qq_official",
  accountRef: "user-1",
  groupRef: null,
  subjectRef: null,
  capabilities: { canClearTarget: false, canDisableGroupProfile: false },
};

const groupTarget: MemoryTargetView = {
  targetRef: "qq:group:group-1",
  scope: "group",
  platform: "qq_official",
  accountRef: "user-1",
  groupRef: "group-1",
  subjectRef: null,
  capabilities: { canClearTarget: true, canDisableGroupProfile: false },
};

const profileTarget: MemoryTargetView = {
  targetRef: "qq:group_profile:member-1",
  scope: "group_profile",
  platform: "qq_official",
  accountRef: "user-1",
  groupRef: "group-1",
  subjectRef: "member-1",
  capabilities: { canClearTarget: false, canDisableGroupProfile: true },
};

function memoryItem(target: MemoryTargetView): MemoryItem {
  return {
    memoryRef: "mem-1",
    target,
    version: 3,
    content: "测试记忆内容",
    kind: target.scope,
    category: "note",
    visibility: "private",
    status: "active",
    pinned: false,
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: null,
    lastConfirmedAt: null,
    sourceType: "user_confirmed",
    capabilities: { canUpdate: false, canArchive: false, canRestore: false, canDelete: true },
  };
}

function confirmationFor(operation: MemoryConfirmation["operation"], target: MemoryTargetView, affectedCount: number): MemoryConfirmation {
  return {
    confirmationToken: `token-${operation}`,
    operation,
    target,
    affectedCount,
    expiresAt: Date.now() + 60_000,
  };
}

function commitResult(operation: MemoryConfirmation["operation"], target: MemoryTargetView, overrides: Record<string, unknown> = {}) {
  return {
    affectedCount: 1,
    capabilities: target.capabilities,
    target,
    ...overrides,
  };
}

function renderPage(): void {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  render(
    <QueryClientProvider client={queryClient}>
      <MemoryPage />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.mocked(listMemories).mockResolvedValue({
    items: [memoryItem(personalTarget)],
    page: 1,
    pageSize: 20,
    total: 1,
    totalPages: 1,
  });
  vi.mocked(listMemoryTargets).mockResolvedValue({
    items: [groupTarget, profileTarget],
    page: 1,
    pageSize: 100,
    total: 2,
    totalPages: 1,
  });
  vi.mocked(prepareMemoryOperation).mockResolvedValue(confirmationFor("delete_memory", personalTarget, 1));
  vi.mocked(commitMemoryOperation).mockResolvedValue(commitResult("delete_memory", personalTarget, { deleted: true, memoryRef: "mem-1" }));
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Memory 两阶段确认", () => {
  it("prepare 成功后打开确认框；用户取消时不调用 commit", async () => {
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "永久删除" }));

    await waitFor(() => expect(prepareMemoryOperation).toHaveBeenCalledTimes(1));
    expect(prepareMemoryOperation).toHaveBeenCalledWith({
      operation: "delete_memory",
      targetRef: personalTarget.targetRef,
      memoryRef: "mem-1",
      expectedVersion: 3,
    });

    // 确认对话框在 prepare 成功后才出现。
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("永久删除 Memory");

    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(commitMemoryOperation).not.toHaveBeenCalled();
  });

  it("清空范围确认框展示 prepare 返回的 affectedCount", async () => {
    vi.mocked(prepareMemoryOperation).mockResolvedValue(confirmationFor("clear_target", groupTarget, 7));
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "清空此范围" }));

    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("清空 7 条 Memory");
    expect(prepareMemoryOperation).toHaveBeenCalledWith({ operation: "clear_target", targetRef: groupTarget.targetRef });
  });

  it("停止画像确认框展示 prepare 返回的 affectedCount", async () => {
    vi.mocked(prepareMemoryOperation).mockResolvedValue(confirmationFor("disable_group_profile", profileTarget, 4));
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "停止画像" }));

    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("停止画像并归档 4 条 Memory");
    expect(prepareMemoryOperation).toHaveBeenCalledWith({ operation: "disable_group_profile", targetRef: profileTarget.targetRef });
  });

  it("用户确认后只 commit 一次，且使用 prepare 返回的 token", async () => {
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "永久删除" }));
    await screen.findByRole("dialog");
    fireEvent.click(screen.getByRole("button", { name: "确认执行" }));

    await waitFor(() => expect(commitMemoryOperation).toHaveBeenCalledTimes(1));
    expect(commitMemoryOperation).toHaveBeenCalledWith({
      operation: "delete_memory",
      targetRef: personalTarget.targetRef,
      confirmationToken: "token-delete_memory",
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(await screen.findByText("Memory 已由服务端确认删除")).toBeTruthy();
  });

  it("delete_memory 提交结果与原记录不一致时不报告成功", async () => {
    vi.mocked(commitMemoryOperation).mockResolvedValue(
      commitResult("delete_memory", personalTarget, { deleted: true, memoryRef: "mem-other" }),
    );
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "永久删除" }));
    await screen.findByRole("dialog");
    fireEvent.click(screen.getByRole("button", { name: "确认执行" }));

    await waitFor(() => expect(commitMemoryOperation).toHaveBeenCalledTimes(1));
    expect(await screen.findByText("Memory 删除结果无法与原记录确认")).toBeTruthy();
    expect(screen.queryByText("Memory 已由服务端确认删除")).toBeNull();
  });

  it("prepare 失败不会打开可提交的确认状态", async () => {
    vi.mocked(prepareMemoryOperation).mockRejectedValue(new Error("prepare failed"));
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "永久删除" }));

    // 失败原因如实展示，且确认框从未打开、不会发生 commit。
    await waitFor(() => expect(screen.getByText("prepare failed")).toBeTruthy());
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(commitMemoryOperation).not.toHaveBeenCalled();
  });
});
