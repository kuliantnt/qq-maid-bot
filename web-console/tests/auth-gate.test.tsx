import { render, screen } from "@testing-library/react";
import { Provider as JotaiProvider } from "jotai";
import { describe, expect, it } from "vitest";
import { AuthGate } from "../src/features/auth/auth-gate.js";

describe("AuthGate 认证门", () => {
  it("默认登录模式渲染用户名、密码与提交按钮，不渲染 token 输入", () => {
    render(
      <JotaiProvider>
        <AuthGate />
      </JotaiProvider>,
    );
    expect(screen.getByText("管理员用户名")).toBeInTheDocument();
    expect(screen.getByText("管理员密码")).toBeInTheDocument();
    expect(screen.queryByText("一次性 Bootstrap Token")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "完成安全初始化" })).toBeInTheDocument();
  });

  it("密码输入默认密文显示且带显示/隐藏切换", () => {
    render(
      <JotaiProvider>
        <AuthGate />
      </JotaiProvider>,
    );
    const password = screen.getByLabelText("管理员密码");
    expect(password).toHaveAttribute("type", "password");
    const toggle = screen.getByRole("button", { name: "显示" });
    expect(toggle).toHaveAttribute("aria-pressed", "false");
  });
});
