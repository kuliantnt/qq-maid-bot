import type { InputHTMLAttributes } from "react";
import { cn } from "../../lib/utils.js";

type SwitchProps = Omit<InputHTMLAttributes<HTMLInputElement>, "type"> & {
  className?: string;
};

/** 轻量开关：原生 checkbox + peer 样式；disabled 与 aria 语义与原生一致。
 * 布局参考 Cherry Studio 的服务商/模型启停开关，配色沿用控制台主题。 */
export function Switch({ className, ...props }: SwitchProps) {
  return (
    <label
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center",
        props.disabled ? "cursor-not-allowed opacity-60" : "cursor-pointer",
        className,
      )}
    >
      <input type="checkbox" role="switch" className="peer sr-only" {...props} />
      <span
        aria-hidden
        className="pointer-events-none absolute inset-0 rounded-full border border-line bg-input transition-colors peer-checked:border-accent peer-checked:bg-accent"
      />
      <span
        aria-hidden
        className="pointer-events-none absolute left-0.5 top-0.5 h-4 w-4 rounded-full bg-ink transition-transform peer-checked:translate-x-4"
      />
    </label>
  );
}
