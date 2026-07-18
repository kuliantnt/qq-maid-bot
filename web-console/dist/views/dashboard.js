import { formatMarker, setText, stateLabel, yesNoUnknown } from "../dom.js";
export function renderDashboard(status) {
    const healthLabel = status.runtime.state === "setup_required"
        ? "等待首次配置"
        : status.runtime.ok
            ? "健康"
            : "异常";
    setText("health-value", healthLabel);
    setText("version-value", status.runtime.version);
    setText("started-value", formatMarker(status.runtime.startedAt));
    setText("uptime-value", formatDuration(status.runtime.uptimeSeconds));
    setText("provider-value", status.provider.name);
    setText("model-value", status.provider.model);
    setText("stream-value", yesNoUnknown(status.provider.streaming));
    setText("upstream-value", stateLabel(status.provider.upstreamState));
    setText("upstream-time", formatMarker(status.provider.lastCheckedAt));
    setText("upstream-error", status.provider.errorSummary ?? "无");
    setText("listen-value", status.configuration.listen);
}
function formatDuration(seconds) {
    if (seconds === null)
        return "不可用";
    const days = Math.floor(seconds / 86_400);
    const hours = Math.floor((seconds % 86_400) / 3_600);
    const minutes = Math.floor((seconds % 3_600) / 60);
    return [days ? `${days} 天` : "", hours ? `${hours} 小时` : "", `${minutes} 分钟`]
        .filter(Boolean)
        .join(" ");
}
