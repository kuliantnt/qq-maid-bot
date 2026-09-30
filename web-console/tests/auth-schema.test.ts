import { describe, expect, it } from "vitest";
import {
  authCredentialsSchema,
  bootstrapTokenSchema,
  buildAuthFormSchema,
  type AuthFormValues,
} from "../src/features/auth/auth-schema.js";

const baseValues: AuthFormValues = { username: "admin", password: "secret-password", bootstrapToken: "" };

describe("认证表单 schema", () => {
  it("登录模式：不要求 bootstrap token，凭据通过即有效", () => {
    const result = buildAuthFormSchema("login").safeParse(baseValues);
    expect(result.success).toBe(true);
  });

  it("初始化模式：缺少 bootstrap token 时失败并指向该字段", () => {
    const result = buildAuthFormSchema("initialize").safeParse(baseValues);
    expect(result.success).toBe(false);
    if (!result.success) {
      const paths = result.error.issues.map((issue) => issue.path.join("."));
      expect(paths).toContain("bootstrapToken");
    }
  });

  it("密码重置模式：不校验用户名，但校验新密码长度", () => {
    const resetValues: AuthFormValues = { username: "", password: "123", bootstrapToken: "token" };
    const result = buildAuthFormSchema("password-reset").safeParse(resetValues);
    expect(result.success).toBe(false);
    if (!result.success) {
      const paths = result.error.issues.map((issue) => issue.path.join("."));
      expect(paths).toContain("password");
      expect(paths).not.toContain("username");
    }
  });

  it("凭据子 schema 的类型契约与最小长度约束保持一致", () => {
    expect(authCredentialsSchema.shape.username.safeParse("ab").success).toBe(false);
    expect(authCredentialsSchema.shape.username.safeParse("abc").success).toBe(true);
    expect(bootstrapTokenSchema.safeParse("").success).toBe(false);
  });
});
