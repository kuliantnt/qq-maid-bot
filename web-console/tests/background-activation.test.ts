import { afterEach, describe, expect, it, vi } from "vitest";
import { createBackgroundController, type BackgroundController } from "../src/background.js";
import { activateBackgroundFile, type BackgroundActivationDeps } from "../src/features/configuration/background-activation.js";
import type { UserPreferences } from "../src/types.js";

function fakeRoot(customLayer: { style: Record<string, string> }): HTMLElement {
  return {
    dataset: {},
    style: {},
    querySelector: (selector: string) => (selector === ".console-background--custom" ? customLayer : null),
  } as unknown as HTMLElement;
}

interface FakeUserData {
  preferences: UserPreferences;
  updateCalls: Array<Record<string, unknown>>;
  updatePreferences: (patch: Record<string, unknown>) => Promise<UserPreferences>;
}

function makeUserData({ failPersist = false, failRollback = false, preferences }: {
  failPersist?: boolean;
  failRollback?: boolean;
  preferences?: Partial<UserPreferences>;
} = {}): FakeUserData {
  const userData: FakeUserData = {
    preferences: {
      customColors: [],
      backgroundFileIds: [],
      activeBackgroundFileId: null,
      backgroundMode: "default",
      kuliantnt: false,
      ...preferences,
    },
    updateCalls: [],
    updatePreferences: async (patch) => {
      userData.updateCalls.push(patch);
      // 回滚补丁只包含 activeBackgroundFileId 与 backgroundMode，不含 backgroundFileIds。
      const isRollback = patch.activeBackgroundFileId !== undefined && patch.backgroundFileIds === undefined;
      if (isRollback && failRollback) throw new Error("rollback failed");
      if (!isRollback && failPersist) throw new Error("persist failed");
      userData.preferences = { ...userData.preferences, ...patch };
      return userData.preferences;
    },
  };
  return userData;
}

function makeDeps(userData: FakeUserData, controller: Pick<BackgroundController, "readFileBlob" | "selectFile">, statuses: string[]): BackgroundActivationDeps {
  return {
    preferences: userData.preferences,
    updatePreferences: async (patch) => {
      const next = await userData.updatePreferences(patch);
      // 激活流程内后续回滚要看到最新偏好。
      userData.preferences = next;
      return next;
    },
    controller,
    setStatus: (text) => statuses.push(text),
  };
}

function stubObjectUrl(impl: () => string): void {
  vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: impl }));
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("自定义背景激活流程", () => {
  it("激活成功：先读 Blob，再写服务端，最后应用本地背景", async () => {
    const customLayer = { style: {} as Record<string, string> };
    const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
    const userData = makeUserData();
    const statuses: string[] = [];
    stubObjectUrl(() => "blob:activated");

    await activateBackgroundFile(
      makeDeps(userData, controller, statuses),
      { fileId: "a", filename: "a.png", url: "/a" },
      true,
    );

    expect(userData.updateCalls).toEqual([{ backgroundFileIds: ["a"], activeBackgroundFileId: "a" }]);
    expect(controller.selection().activeFileId).toBe("a");
    expect(customLayer.style.backgroundImage).toBe('url("blob:activated")');
    expect(statuses.at(-1)).toBe("背景已保存。");
  });

  it("背景读取失败时不留下活动背景 ID，且不提交服务端", async () => {
    const customLayer = { style: {} as Record<string, string> };
    const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => {
      if (file.fileId === "bad") throw new Error("read failed");
      return new Blob([file.fileId]);
    });
    const userData = makeUserData();
    const statuses: string[] = [];
    stubObjectUrl(() => "blob:kept");
    await controller.selectFile({ fileId: "existing", filename: "existing.png", url: "/existing" });
    const kept = customLayer.style.backgroundImage;

    await activateBackgroundFile(
      makeDeps(userData, controller, statuses),
      { fileId: "bad", filename: "bad.png", url: "/bad" },
      true,
    );

    expect(statuses.at(-1)).toBe("背景读取失败，已保留原背景：read failed");
    expect(userData.updateCalls).toHaveLength(0);
    expect(controller.selection().activeFileId).toBe("existing");
    expect(customLayer.style.backgroundImage).toBe(kept);
  });

  it("服务端写入失败时不激活本地，原 object URL 保留", async () => {
    const customLayer = { style: {} as Record<string, string> };
    const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
    const userData = makeUserData({ failPersist: true });
    const statuses: string[] = [];
    stubObjectUrl(() => "blob:kept");
    await controller.selectFile({ fileId: "old", filename: "old.png", url: "/old" });
    const kept = customLayer.style.backgroundImage;

    await activateBackgroundFile(
      makeDeps(userData, controller, statuses),
      { fileId: "new", filename: "new.png", url: "/new" },
      true,
    );

    expect(statuses.at(-1)).toMatch(/背景保存失败，已保留原背景：persist failed/);
    expect(controller.selection().activeFileId).toBe("old");
    expect(customLayer.style.backgroundImage).toBe(kept);
  });

  it("本地应用失败但回滚成功时恢复服务端活动背景并显示明确信息", async () => {
    const customLayer = { style: {} as Record<string, string> };
    const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
    const userData = makeUserData();
    const statuses: string[] = [];
    stubObjectUrl(() => {
      throw new Error("object url failed");
    });

    await activateBackgroundFile(
      makeDeps(userData, controller, statuses),
      { fileId: "a", filename: "a.png", url: "/a" },
      true,
    );

    expect(statuses.at(-1)).toBe("背景应用失败，已恢复原背景：object url failed");
    expect(controller.selection().activeFileId).toBeNull();
  });

  it("本地应用失败且回滚失败时如实显示，不产生未处理 rejection", async () => {
    const customLayer = { style: {} as Record<string, string> };
    const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
    const userData = makeUserData({ failRollback: true });
    const statuses: string[] = [];
    stubObjectUrl(() => {
      throw new Error("object url failed");
    });

    await activateBackgroundFile(
      makeDeps(userData, controller, statuses),
      { fileId: "a", filename: "a.png", url: "/a" },
      true,
    );

    const finalStatus = statuses.at(-1) ?? "";
    expect(finalStatus).toMatch(/背景应用失败（object url failed）/);
    expect(finalStatus).toMatch(/恢复原背景也失败：rollback failed/);
    expect(userData.updateCalls).toEqual([
      { backgroundFileIds: ["a"], activeBackgroundFileId: "a" },
      { activeBackgroundFileId: null, backgroundMode: "default" },
    ]);
    expect(controller.selection().activeFileId).toBeNull();
  });

  it("原背景 A 激活 B 本地应用失败：服务端恢复 A，浏览器始终保留 A", async () => {
    const customLayer = { style: {} as Record<string, string> };
    const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
    const userData = makeUserData({
      preferences: { backgroundFileIds: ["a"], activeBackgroundFileId: "a" },
    });
    const statuses: string[] = [];
    stubObjectUrl(() => "blob:a");
    await controller.selectFile({ fileId: "a", filename: "a.png", url: "/a" }, false, new Blob(["a"]));
    const kept = customLayer.style.backgroundImage;
    stubObjectUrl(() => {
      throw new Error("object url failed");
    });

    await activateBackgroundFile(
      makeDeps(userData, controller, statuses),
      { fileId: "b", filename: "b.png", url: "/b" },
      true,
    );

    // 回滚到原活动背景 A 而不是清空为 null。
    expect(userData.updateCalls).toEqual([
      { backgroundFileIds: ["a", "b"], activeBackgroundFileId: "b" },
      { activeBackgroundFileId: "a", backgroundMode: "default" },
    ]);
    expect(controller.selection().activeFileId).toBe("a");
    expect(customLayer.style.backgroundImage).toBe(kept);
    expect(statuses.at(-1)).toBe("背景应用失败，已恢复原背景：object url failed");
  });

  it("原 special 激活 B 本地应用失败：服务端恢复 special 而不是重置为 default", async () => {
    const customLayer = { style: {} as Record<string, string> };
    const controller = createBackgroundController(fakeRoot(customLayer), null, async (file) => new Blob([file.fileId]));
    const userData = makeUserData({
      preferences: { backgroundMode: "special", kuliantnt: true },
    });
    const statuses: string[] = [];
    stubObjectUrl(() => {
      throw new Error("object url failed");
    });

    await activateBackgroundFile(
      makeDeps(userData, controller, statuses),
      { fileId: "b", filename: "b.png", url: "/b" },
      true,
    );

    expect(userData.updateCalls).toEqual([
      { backgroundFileIds: ["b"], activeBackgroundFileId: "b" },
      { activeBackgroundFileId: null, backgroundMode: "special" },
    ]);
    expect(controller.selection().activeFileId).toBeNull();
    expect(statuses.at(-1)).toBe("背景应用失败，已恢复原背景：object url failed");
  });
});
