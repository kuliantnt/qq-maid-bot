import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { usePageTransition } from "../src/components/layout/app-shell.js";
import type { TransitionImage } from "../src/background.js";

vi.mock("../src/stores/background.js", () => ({
  backgroundController: {
    // 断言切换时会取过渡中心切片；default 模式返回 null 表示仅主题清洗过渡。
    nextTransitionImage: vi.fn((): TransitionImage => null),
  },
}));

const COVER_DURATION_MS = 784;
const WASHOUT_DURATION_MS = 896;

function Harness({ navigate }: { navigate: (to: string) => Promise<void> }) {
  const { navigateTo, transitionOverlay } = usePageTransition(navigate);
  return (
    <>
      <button type="button" onClick={() => void navigateTo("/a")}>go-a</button>
      <button type="button" onClick={() => void navigateTo("/b")}>go-b</button>
      {transitionOverlay}
    </>
  );
}

function overlay(): HTMLElement | null {
  // 过渡层常驻渲染，hidden 属性表达运行状态。
  return document.querySelector<HTMLElement>(".console-transition");
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  cleanup();
  vi.clearAllMocks();
});

describe("页面切换过渡", () => {
  it("点击导航后 overlay 进入运行态，cover 结束才导航，washout 结束 overlay 隐藏", async () => {
    const navigate = vi.fn(() => Promise.resolve());
    render(<Harness navigate={navigate} />);

    fireEvent.click(screen.getByRole("button", { name: "go-a" }));

    // cover 阶段：overlay 显示，导航尚未发生。
    const layer = overlay();
    expect(layer).not.toBeNull();
    expect(layer).not.toHaveAttribute("hidden");
    expect(layer).toHaveClass("is-running");
    expect(navigate).not.toHaveBeenCalled();

    act(() => { vi.advanceTimersByTime(COVER_DURATION_MS - 1); });
    expect(navigate).not.toHaveBeenCalled();

    // cover 结束：先执行导航，washout 期间 overlay 仍显示。
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(navigate).toHaveBeenCalledTimes(1);
    expect(navigate).toHaveBeenCalledWith("/a");
    expect(overlay()).not.toHaveAttribute("hidden");

    // washout 结束：overlay 回到隐藏状态。
    await act(async () => { await vi.advanceTimersByTimeAsync(WASHOUT_DURATION_MS); });
    expect(overlay()).toHaveAttribute("hidden");
    expect(overlay()).not.toHaveClass("is-running");
  });

  it("prefers-reduced-motion 时不等待直接导航，overlay 不出现", async () => {
    const matchesSpy = vi.spyOn(window, "matchMedia").mockReturnValue({
      matches: true,
      media: "(prefers-reduced-motion: reduce)",
      onchange: null,
      addListener: () => undefined,
      removeListener: () => undefined,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
      dispatchEvent: () => false,
    } as MediaQueryList);
    const navigate = vi.fn(() => Promise.resolve());
    render(<Harness navigate={navigate} />);

    fireEvent.click(screen.getByRole("button", { name: "go-a" }));

    await act(async () => { await Promise.resolve(); });
    expect(navigate).toHaveBeenCalledTimes(1);
    expect(navigate).toHaveBeenCalledWith("/a");
    expect(overlay()).toHaveAttribute("hidden");
    act(() => { vi.advanceTimersByTime(COVER_DURATION_MS + WASHOUT_DURATION_MS); });
    // 不再有第二次导航或额外等待。
    expect(navigate).toHaveBeenCalledTimes(1);
    matchesSpy.mockRestore();
  });

  it("转换进行中的连续导航保留最新目标（latest-wins），不并发执行", async () => {
    const navigate = vi.fn(() => Promise.resolve());
    render(<Harness navigate={navigate} />);

    fireEvent.click(screen.getByRole("button", { name: "go-a" }));
    // cover 期间再点 go-b：不会立刻导航。
    fireEvent.click(screen.getByRole("button", { name: "go-b" }));
    await act(async () => { await vi.advanceTimersByTimeAsync(COVER_DURATION_MS); });
    expect(navigate).toHaveBeenCalledTimes(1);
    expect(navigate).toHaveBeenCalledWith("/a");

    // 第一轮 washout 结束后，自动排队执行最新目标 /b。
    await act(async () => { await vi.advanceTimersByTimeAsync(WASHOUT_DURATION_MS); });
    expect(overlay()).not.toHaveAttribute("hidden");
    await act(async () => { await vi.advanceTimersByTimeAsync(COVER_DURATION_MS); });
    expect(navigate).toHaveBeenCalledTimes(2);
    expect(navigate).toHaveBeenLastCalledWith("/b");
    await act(async () => { await vi.advanceTimersByTimeAsync(WASHOUT_DURATION_MS); });
    expect(overlay()).toHaveAttribute("hidden");
    expect(navigate).toHaveBeenCalledTimes(2);
  });
});
