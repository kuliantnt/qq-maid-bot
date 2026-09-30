import { Frame, SectionHeader } from "../ui/frame.js";

/** 页面迁移过渡占位：分支中间态使用，全部页面迁移完成后删除。 */
export function MigrationPlaceholder({ title }: { title: string }) {
  return (
    <Frame variant="panel" className="animate-page-in">
      <SectionHeader eyebrow={`MIGRATION / ${title}`} title={`${title} · 迁移中`} lede="该页面正在迁移到 React 实现，功能将在迁移提交中恢复。" />
      <p className="mt-6 text-sm text-muted" role="status">
        当前分支的构建产物中此页面暂不可用；这不是生产状态。
      </p>
    </Frame>
  );
}
