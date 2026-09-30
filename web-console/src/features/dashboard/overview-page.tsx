import { useConsoleStatusQuery } from "../../queries/console-status.js";
import { Frame, SectionHeader } from "../../components/ui/frame.js";
import { QueryState } from "../../components/ui/query-state.js";
import { StatusBadge, toneForState } from "../../components/ui/status-badge.js";
import { formatDuration, formatMarker, stateLabel, yesNoUnknown } from "../../lib/format.js";

/** 总览：服务健康 + 运行指标 + 上游摘要的只读运行快照。 */
export function OverviewPage() {
  const statusQuery = useConsoleStatusQuery();
  return (
    <Frame variant="panel" className="animate-page-in">
      <SectionHeader
        eyebrow="OVERVIEW / RUNTIME"
        title="运行总览"
        lede="服务、模型与上游连接的只读运行快照。"
        meta={
          <>
            <span className="border border-line bg-accent-soft px-2 py-1 font-mono text-[0.64rem] font-bold tracking-wide text-accent-strong">
              只读
            </span>
            <span className="text-xs text-muted">数据来自最近一次状态刷新</span>
          </>
        }
      />
      <QueryState query={statusQuery}>
        {statusQuery.data ? <OverviewContent status={statusQuery.data} /> : null}
      </QueryState>
    </Frame>
  );
}

function OverviewContent({ status }: { status: NonNullable<ReturnType<typeof useConsoleStatusQuery>["data"]> }) {
  const setupRequired = status.runtime.state === "setup_required";
  const healthTone = setupRequired ? "warning" : status.runtime.ok ? "success" : "error";
  const healthLabel = setupRequired ? "等待首次配置" : status.runtime.ok ? "健康" : "异常";
  return (
    <>
      <Frame className="flex flex-wrap items-center justify-between gap-4 p-5">
        <div>
          <p className="console-mono-tag m-0 mb-1">PRIMARY HEALTH STATE</p>
          <h3 className="m-0 text-base font-bold">服务健康</h3>
          <p className="m-0 text-xs text-muted">运行时状态与基础配置检查结果</p>
        </div>
        <StatusBadge tone={healthTone} label={healthLabel} className="px-3 py-2 text-sm" />
      </Frame>

      <section aria-labelledby="runtime-metrics-heading" className="mt-6">
        <div className="mb-3 flex items-baseline gap-2">
          <h3 id="runtime-metrics-heading" className="m-0 text-base font-bold">运行指标</h3>
          <span className="font-mono text-[0.66rem] tracking-widest text-muted uppercase">Runtime metrics</span>
        </div>
        <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
          <MetricCard label="版本" value={status.runtime.version} />
          <MetricCard label="启动时间" value={formatMarker(status.runtime.startedAt)} />
          <MetricCard label="运行时长" value={formatDuration(status.runtime.uptimeSeconds)} />
          <MetricCard label="监听摘要" value={status.configuration.listen} />
        </div>
      </section>

      <section aria-labelledby="provider-summary-heading" className="mt-6">
        <div className="mb-3 flex items-baseline gap-2">
          <h3 id="provider-summary-heading" className="m-0 text-base font-bold">上游摘要</h3>
          <span className="font-mono text-[0.66rem] tracking-widest text-muted uppercase">Provider summary</span>
        </div>
        <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
          <MetricCard label="Provider" value={status.provider.name} />
          <MetricCard label="模型" value={status.provider.model} />
          <MetricCard label="流式" value={yesNoUnknown(status.provider.streaming)} />
          <Frame className="flex flex-col gap-1 p-4">
            <span className="text-xs text-muted">上游连接</span>
            <StatusBadge tone={toneForState(status.provider.upstreamState)} value={status.provider.upstreamState} />
            <span className="text-xs text-muted">检查于 {formatMarker(status.provider.lastCheckedAt)}</span>
            {status.provider.errorSummary ? (
              <span className="font-mono text-xs text-error">{status.provider.errorSummary}</span>
            ) : null}
          </Frame>
        </div>
        <p className="mt-3 text-xs text-muted">
          状态标识：{stateLabel(status.provider.upstreamState)}
        </p>
      </section>
    </>
  );
}

function MetricCard({ label, value }: { label: string; value: string }) {
  return (
    <Frame className="flex flex-col gap-1 p-4">
      <span className="text-xs text-muted">{label}</span>
      <strong className="font-mono text-sm break-all text-ink">{value}</strong>
    </Frame>
  );
}
