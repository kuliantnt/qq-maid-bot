import type { InputHTMLAttributes, ReactNode } from "react";
import { cn } from "../../lib/utils.js";

type FieldProps = {
  label: string;
  /** label 的 for 绑定;不传 id 时必须传。 */
  id: string;
  hint?: ReactNode | undefined;
  error?: string | null | undefined;
  children: (props: { id: string; "aria-describedby"?: string | undefined; "aria-invalid"?: true | undefined }) => ReactNode;
};

/**
 * 表单字段包装：label + 控件 + hint/错误文本，
 * 通过 render prop 把 aria 关联注入控件，保持无障碍契约。
 */
export function Field({ label, id, hint, error, children }: FieldProps) {
  const hintId = hint !== undefined ? `${id}-hint` : undefined;
  const errorId = error ? `${id}-error` : undefined;
  const describedBy = [hintId, errorId].filter(Boolean).join(" ") || undefined;
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="text-sm font-semibold text-ink">
        {label}
      </label>
      {children({ id, "aria-describedby": describedBy, "aria-invalid": error ? true : undefined })}
      {hint ? (
        <p id={hintId} className="m-0 text-xs leading-relaxed text-muted">
          {hint}
        </p>
      ) : null}
      {error ? (
        <p id={errorId} role="alert" className="m-0 text-xs font-semibold text-error">
          {error}
        </p>
      ) : null}
    </div>
  );
}

type InputProps = InputHTMLAttributes<HTMLInputElement>;

/** 文本输入：1px 边框 + input 底色，错误态红色边框。 */
export function Input({ className, ...props }: InputProps) {
  return (
    <input
      className={cn(
        "border border-line bg-input px-3 py-2.5 text-ink outline-none placeholder:text-muted",
        "focus-visible:border-accent",
        props["aria-invalid"] && "border-error",
        className,
      )}
      {...props}
    />
  );
}
