import { z } from "zod";
import type { AuthMode } from "../../stores/auth.js";

/**
 * 认证表单 schema 是“schema 即类型”模式的示范：
 * z.infer 导出的类型同时用于表单默认值、TanStack Form 泛型与 API 请求负载，
 * 运行时校验与编译期类型来自同一份定义，不会漂移。
 */
export const authCredentialsSchema = z.object({
  username: z.string().min(3, "用户名至少 3 个字符").max(64, "用户名最长 64 个字符"),
  password: z.string().min(6, "密码至少 6 个字符").max(256, "密码最长 256 个字符"),
});

export type AuthCredentials = z.infer<typeof authCredentialsSchema>;

export const bootstrapTokenSchema = z.string().min(1, "请粘贴运行目录中的一次性 Bootstrap Token");

/** 标准化表单值：三种模式共用同一组输入，按 authMode 做差异化校验。 */
export type AuthFormValues = AuthCredentials & { bootstrapToken: string };

/** 按 authMode 生成整表校验规则；字段级即时校验由表单字段单独引用子 schema。 */
export function buildAuthFormSchema(mode: AuthMode) {
  return z
    .object({
      username: z.string(),
      password: authCredentialsSchema.shape.password,
      bootstrapToken: z.string(),
    })
    .superRefine((value, ctx) => {
      if (mode !== "password-reset") {
        const username = authCredentialsSchema.shape.username.safeParse(value.username);
        if (!username.success) {
          ctx.addIssue({ code: "custom", path: ["username"], message: username.error.issues[0]?.message ?? "用户名格式不正确" });
        }
      }
      if (mode !== "login") {
        const token = bootstrapTokenSchema.safeParse(value.bootstrapToken);
        if (!token.success) {
          ctx.addIssue({ code: "custom", path: ["bootstrapToken"], message: token.error.issues[0]?.message ?? "Bootstrap Token 不能为空" });
        }
      }
    });
}
