import { useConsoleStatusQuery } from "../../queries/console-status.js";
import { DataTable } from "../../components/ui/data-table.js";
import { Frame, SectionHeader } from "../../components/ui/frame.js";
import { QueryState } from "../../components/ui/query-state.js";
import { StatusBadge, toneForState } from "../../components/ui/status-badge.js";
import { formatMarker, stateLabel, yesNoUnknown } from "../../lib/format.js";

/** 平台页：平台身份状态 + 双向能力矩阵，与旧版信息架构一致。 */
export function PlatformsPage() {
  const statusQuery = useConsoleStatusQuery();
  return (
    <Frame variant="panel" className="animate-page-in">
      <SectionHeader
        eyebrow="PLATFORMS / CAPABILITIES"
        title="平台与能力"
        lede="各接入平台的身份状态、最近事件与消息能力矩阵。"
        meta={<span className="border border-line bg-accent-soft px-2 py-1 font-mono text-[0.64rem] font-bold tracking-wide text-accent-strong">只读</span>}
      />
      <QueryState query={statusQuery}>
        {statusQuery.data ? (
          <>
            <section aria-label="平台状态" className="mt-2">
              <h3 className="m-0 mb-3 text-base font-bold">平台状态</h3>
              <DataTable
                columns={["平台", "已配置", "已启用", "状态", "最近事件", "最近错误"]}
                rows={statusQuery.data.platforms.map((platform) => [
                  <strong key="label" className="font-mono text-ink">{platform.label}</strong>,
                  yesNoUnknown(platform.configured),
                  yesNoUnknown(platform.enabled),
                  <StatusBadge key="state" tone={toneForState(platform.state)} value={platform.state} />,
                  formatMarker(platform.lastEventAt),
                  <span key="error" className={platform.lastErrorSummary ? "text-error" : "text-muted"}>
                    {platform.lastErrorSummary ?? "无"}
                  </span>,
                ])}
                empty="尚未接入任何平台"
              />
            </section>
            <section aria-label="能力矩阵" className="mt-8">
              <h3 className="m-0 mb-3 text-base font-bold">能力矩阵</h3>
              <DataTable
                columns={["平台", "范围", "方向", "文本", "Markdown", "图片", "文件", "混合", "流式"]}
                rows={statusQuery.data.platforms.flatMap((platform) =>
                  platform.capabilityScopes.flatMap((scope) =>
                    (["接收", "发送"] as const).map((direction) => {
                      const capabilities = direction === "接收" ? scope.capabilities.inbound : scope.capabilities.outbound;
                      return [
                        <strong key={`${platform.id}-label`} className="font-mono text-ink">{platform.label}</strong>,
                        scope.label,
                        direction,
                        ...(["text", "markdown", "image", "file", "mixedMessage", "streaming"] as const).map((key) => (
                          <StatusBadge key={`${platform.id}-${scope.label}-${direction}-${key}`} tone={toneForState(capabilities[key])} value={capabilities[key]} />
                        )),
                      ];
                    }),
                  ),
                )}
                empty="暂无能力数据"
              />
            </section>
          </>
        ) : null}
      </QueryState>
    </Frame>
  );
}
