import { useForm } from "@tanstack/react-form";
import { useAtomValue } from "jotai";
import { useMemo, useState } from "react";
import { Button } from "../../components/ui/button.js";
import { Field, Input } from "../../components/ui/field.js";
import {
  authMessageAtom,
  authModeAtom,
  bootstrapStatusAtom,
  cancelPasswordReset,
  startPasswordReset,
  submitAuth,
} from "../../stores/auth.js";
import {
  authCredentialsSchema,
  bootstrapTokenSchema,
  buildAuthFormSchema,
  type AuthFormValues,
} from "./auth-schema.js";

/** 认证门：登录 / 首次初始化 / 密码重置三模式表单。
 * Bootstrap token 只在 initialize 与 password-reset 模式要求；
 * 提交走 store 层动作，敏感输入不写入任何持久存储。 */
export function AuthGate() {
  const mode = useAtomValue(authModeAtom);
  const bootstrapStatus = useAtomValue(bootstrapStatusAtom);
  const message = useAtomValue(authMessageAtom);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const formSchema = useMemo(() => buildAuthFormSchema(mode), [mode]);

  const form = useForm({
    defaultValues: { username: "", password: "", bootstrapToken: "" } as AuthFormValues,
    validators: { onSubmit: formSchema },
    onSubmit: async ({ value }) => {
      setSubmitError(null);
      try {
        await submitAuth(mode, value.username, value.password, value.bootstrapToken);
      } catch (cause) {
        setSubmitError(cause instanceof Error ? cause.message : "认证失败");
      }
    },
  });

  const resetting = mode === "password-reset";
  const tokenRequired = mode !== "login";

  const passwordRef = (payload: { value: string }) =>
    authCredentialsSchema.shape.password.safeParse(payload.value).success
      ? undefined
      : ({ message: "密码至少 6 个字符" } as const);

  return (
    <div className="relative z-1 grid min-h-dvh place-items-center bg-canvas p-4">
      <form
        className="w-full max-w-[30rem] border border-line bg-surface p-8 shadow-console animate-page-in sm:p-12"
        onSubmit={(event) => {
          event.preventDefault();
          event.stopPropagation();
          void form.handleSubmit();
        }}
      >
        <p className="console-mono-tag mb-2">QQ MAID BOT · SECURE CONSOLE</p>
        <h1 className="m-0 mb-1 text-balance text-[clamp(1.55rem,4vw,2.2rem)] font-bold leading-tight">
          {resetting ? "重置部署管理员密码" : bootstrapStatus?.initialized ? "部署管理员登录" : "建立首位部署管理员"}
        </h1>
        <p className="mt-2 max-w-[44rem] text-sm leading-relaxed text-muted">
          {resetting
            ? "请在运行目录读取 config/secrets/bootstrap.token；可粘贴完整令牌字符串或仅粘贴 token。重置成功后令牌与旧管理员会话全部失效。"
            : bootstrapStatus?.initialized
              ? "管理员会话与聊天 session 相互独立。"
              : "请在运行目录读取 bootstrap.token；同一个短时单次令牌只在新生成时输出一次到控制台，使用成功后立即失效。"}
        </p>

        <div className="mt-6 flex flex-col gap-4">
          {!resetting ? (
            <form.Field
              name="username"
              validators={{
                onChange: ({ value }) =>
                  authCredentialsSchema.shape.username.safeParse(value).success
                    ? undefined
                    : ({ message: "用户名至少 3 个字符" } as const),
              }}
            >
              {(field) => (
                <Field label="管理员用户名" id="auth-username" error={fieldError(field.state.meta.errors)}>
                  {(props) => (
                    <Input
                      {...props}
                      name={field.name}
                      autoComplete="username"
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) => field.handleChange(event.target.value)}
                    />
                  )}
                </Field>
              )}
            </form.Field>
          ) : null}

          <form.Field name="password" validators={{ onChange: passwordRef }}>
            {(field) => (
              <Field label={resetting ? "新管理员密码" : "管理员密码"} id="auth-password" error={fieldError(field.state.meta.errors)}>
                {(props) => (
                  <PasswordRevealField
                    {...props}
                    name={field.name}
                    autoComplete={resetting || mode === "initialize" ? "new-password" : "current-password"}
                    value={field.state.value}
                    onBlur={field.handleBlur}
                    onChange={(event) => field.handleChange(event.target.value)}
                  />
                )}
              </Field>
            )}
          </form.Field>

          {tokenRequired ? (
            <form.Field
              name="bootstrapToken"
              validators={{
                onChange: ({ value }) =>
                  bootstrapTokenSchema.safeParse(value).success ? undefined : ({ message: "请粘贴一次性 Bootstrap Token" } as const),
              }}
            >
              {(field) => (
                <Field
                  label="一次性 Bootstrap Token"
                  id="bootstrap-token"
                  error={fieldError(field.state.meta.errors)}
                  hint="令牌只输出一次，使用成功后立即失效。"
                >
                  {(props) => (
                    <PasswordRevealField
                      {...props}
                      name={field.name}
                      autoComplete="off"
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) => field.handleChange(event.target.value)}
                    />
                  )}
                </Field>
              )}
            </form.Field>
          ) : null}
        </div>

        <div className="mt-6 flex flex-col gap-3">
          <form.Subscribe selector={(state) => [state.canSubmit, state.isSubmitting] as const}>
            {([canSubmit, isSubmitting]) => (
              <Button type="submit" disabled={!canSubmit || isSubmitting}>
                {isSubmitting
                  ? "验证中…"
                  : resetting
                    ? "完成密码重置"
                    : bootstrapStatus?.initialized
                      ? "登录控制台"
                      : "完成安全初始化"}
              </Button>
            )}
          </form.Subscribe>
          {bootstrapStatus?.initialized ? (
            <Button
              variant="secondary"
              onClick={() => {
                setSubmitError(null);
                if (resetting) cancelPasswordReset();
                else void startPasswordReset().catch((cause: unknown) => setSubmitError(cause instanceof Error ? cause.message : "密码重置令牌生成失败"));
              }}
            >
              {resetting ? "返回密码登录" : "重置管理员密码"}
            </Button>
          ) : null}
        </div>

        <p aria-live="polite" role={submitError || message?.kind === "error" ? "alert" : "status"} className="m-0 mt-4 text-sm font-semibold text-error">
          {submitError ?? message?.text}
        </p>
      </form>
    </div>
  );
}

/** Standard Schema 校验错误 → 文本；TanStack Form v1 返回 issue 对象数组。 */
function fieldError(errors: ReadonlyArray<unknown>): string | undefined {
  const first = errors[0];
  if (first === undefined) return undefined;
  if (typeof first === "string") return first;
  if (typeof first === "object" && first !== null && "message" in first) return String(first.message);
  return String(first);
}

/** 带明文/密文切换的密码输入；按钮文案与 aria-pressed 同步。 */
function PasswordRevealField(props: React.ComponentProps<"input">) {
  const [revealed, setRevealed] = useState(false);
  return (
    <div className="flex">
      <Input {...props} type={revealed ? "text" : "password"} className="flex-1" />
      <button
        type="button"
        aria-pressed={revealed}
        onClick={() => setRevealed((current) => !current)}
        className="border border-line border-l-0 bg-glass-raised px-3 text-sm font-bold text-ink hover:bg-accent-soft"
      >
        {revealed ? "隐藏" : "显示"}
      </button>
    </div>
  );
}
