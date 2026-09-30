import { useConsoleStatusQuery } from "../../queries/console-status.js";
import { DataTable } from "../../components/ui/data-table.js";
import { Frame, SectionHeader } from "../../components/ui/frame.js";
import { QueryState } from "../../components/ui/query-state.js";
import { StatusBadge, toneForState } from "../../components/ui/status-badge.js";
import { stateLabel, yesNoUnknown } from "../../lib/format.js";

/** 存储页：数据库、缓存、附件等资源行的健康清单。 */
export function StoragePage() {
  const statusQuery = useConsoleStatusQuery();
  return (
    <Frame variant="panel" className="animate-page-in">
      <SectionHeader
        eyebrow="STORAGE / RESOURCES"
        title="存储与资源"
        lede="路径可达性、读写能力与 schema/migration 健康信号。"
        meta={<span className="border border-line bg-accent-soft px-2 py-1 font-mono text-[0.64rem] font-bold tracking-wide text-accent-strong">只读</span>}
      />
      <QueryState query={statusQuery}>
        {statusQuery.data ? (
          <DataTable
            columns={["资源", "路径", "状态", "存在", "可读", "可写", "错误", "Schema"]}
            rows={statusQuery.data.storage.map((item) => [
              <strong key="label" className="font-mono text-ink">{item.label}</strong>,
              <span key="path" className="font-mono text-xs">{item.pathSummary}</span>,
              <StatusBadge key="state" tone={toneForState(item.state)} value={item.state} />,
              yesNoUnknown(item.exists),
              yesNoUnknown(item.readable),
              yesNoUnknown(item.writable),
              <span key="error" className={item.errorSummary ? "text-warning" : "text-muted"}>
                {item.errorSummary ? stateLabel(item.errorSummary) : "无"}
              </span>,
              <span key="schema" className="font-mono text-xs">{item.schemaSummary ?? "不适用"}</span>,
            ])}
            empty="暂无存储资源信息"
          />
        ) : null}
      </QueryState>
    </Frame>
  );
}
