import { cn } from "../../lib/utils.js";
import { stateLabel } from "../../lib/format.js";

export type SignalTone = "success" | "warning" | "error" | "neutral";

/** 语义状态 → 几何信号与配色（DESIGN.md §2：成功方形、警告菱形、中性圆形、错误三角形）。 */
const TONE_SHAPES: Readonly<Record<SignalTone, string>> = {
  success: "■",
  warning: "◆",
  neutral: "●",
  error: "▲",
};

const TONE_CLASSES: Readonly<Record<SignalTone, string>> = {
  success: "text-success",
  warning: "text-warning",
  neutral: "text-muted",
  error: "text-error",
};

type StatusBadgeProps = {
  tone: SignalTone;
  /** 状态文本；不传时由 value 查中文标签。 */
  label?: string;
  /** 原始英文状态值（用于未知值回显），或直接传 label。 */
  value?: string;
  className?: string;
};

/** 状态徽章：几何形状 + 颜色 + 文本三重表达，满足“状态不能只靠颜色”。 */
export function StatusBadge({ tone, label, value, className }: StatusBadgeProps) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 border border-line bg-glass-muted px-2 py-1 font-mono text-xs font-bold tracking-wide",
        TONE_CLASSES[tone],
        className,
      )}
    >
      <span aria-hidden="true">{TONE_SHAPES[tone]}</span>
      <span>{label ?? stateLabel(value ?? "unknown")}</span>
    </span>
  );
}

/** 已知语义状态值 → tone 的保守映射；未知值一律中性，避免误导。 */
export function toneForState(value: string): SignalTone {
  switch (value) {
    case "online":
    case "available":
    case "supported":
    case "ready":
      return "success";
    case "warning":
    case "unverified":
    case "not_configured":
    case "degraded":
      return "warning";
    case "offline":
    case "error":
    case "unsupported":
    case "not_found":
    case "permission_denied":
    case "invalid_path":
    case "invalid_path_type":
    case "unsupported_path_type":
    case "io_error":
      return "error";
    default:
      return "neutral";
  }
}
