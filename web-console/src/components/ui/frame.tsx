import type { HTMLAttributes } from "react";
import { cn } from "../../lib/utils.js";

type FrameProps = HTMLAttributes<HTMLElement> & {
  /** as panel 时带内边距与投影，作为页面一级分区；否则是紧凑框体。 */
  variant?: "panel" | "compact";
};

/** 方形双线框体（console-frame）：外 1px 线 + 1px 空隙 + 内 1px 线。 */
export function Frame({ variant = "compact", className, ...props }: FrameProps) {
  const Tag = variant === "panel" ? "section" : "article";
  return (
    <Tag
      className={cn(
        "console-frame text-[color:var(--console-text-primary)]",
        variant === "panel" && "mb-6 p-6 shadow-console md:p-8",
        className,
      )}
      {...props}
    />
  );
}

type SectionHeaderProps = {
  eyebrow: string;
  title: string;
  lede?: string;
  meta?: React.ReactNode;
};

/** 页面一级标题区：eyebrow 定位 + 标题判断 + 短说明，右侧放只读标记等 meta。 */
export function SectionHeader({ eyebrow, title, lede, meta }: SectionHeaderProps) {
  return (
    <header className="flex flex-wrap items-start justify-between gap-4">
      <div>
        <p className="console-mono-tag mb-1.5">{eyebrow}</p>
        <h2 className="m-0 text-balance text-[clamp(1.4rem,3vw,2rem)] font-bold leading-tight">{title}</h2>
        {lede ? <p className="mt-2 max-w-2xl text-sm leading-relaxed text-muted">{lede}</p> : null}
      </div>
      {meta ? <div className="flex items-center gap-2">{meta}</div> : null}
    </header>
  );
}
