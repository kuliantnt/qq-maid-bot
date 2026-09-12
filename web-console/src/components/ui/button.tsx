import type { ButtonHTMLAttributes } from "react";
import { cn } from "../../lib/utils.js";

type ButtonVariant = "primary" | "secondary" | "danger";

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: ButtonVariant;
};

const VARIANT_CLASSES: Readonly<Record<ButtonVariant, string>> = {
  primary: "bg-accent text-accent-contrast hover:bg-accent-strong",
  secondary: "bg-glass-raised text-ink hover:bg-accent-soft",
  danger: "bg-error text-error-contrast hover:bg-accent-strong",
};

/** 控制台按钮：0 圆角、1px 边框、hover 轻微反馈；disabled 表示请求进行中。 */
export function Button({ variant = "primary", className, type = "button", ...props }: ButtonProps) {
  return (
    <button
      type={type}
      className={cn(
        "cursor-pointer border border-line px-4 py-2.5 font-bold transition-colors duration-200",
        "active:translate-y-px disabled:cursor-progress disabled:opacity-55",
        VARIANT_CLASSES[variant],
        className,
      )}
      {...props}
    />
  );
}
