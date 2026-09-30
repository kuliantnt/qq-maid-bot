/** 状态值 → 中文标签；未知值保留原始英文标识便于排障。 */
export function stateLabel(value: string): string {
  const labels: Readonly<Record<string, string>> = {
    online: "在线",
    offline: "离线",
    supported: "支持",
    disabled: "未启用",
    unsupported: "不支持",
    unknown: "未知",
    not_available: "不可用",
    not_configured: "未配置",
    available: "可用",
    error: "异常",
    unverified: "未验证",
    not_found: "不存在",
    permission_denied: "权限不足",
    invalid_path: "路径无效",
    invalid_path_type: "路径类型无效",
    unsupported_path_type: "不支持的路径类型",
    io_error: "访问失败",
  };
  return labels[value] ?? `未知（${value}）`;
}

/** unix 时间戳标记（schema/migration marker）转本地时间展示。 */
export function formatMarker(value: string | null): string {
  if (!value) return "不可用";
  if (value.startsWith("unix:")) {
    const seconds = Number(value.slice(5));
    if (Number.isFinite(seconds)) return new Date(seconds * 1000).toLocaleString();
  }
  return value;
}

export function yesNoUnknown(value: boolean | null): string {
  return value === null ? "不可用" : value ? "是" : "否";
}

/** 运行时长：秒 → “N 天 N 小时 N 分钟”的紧凑中文展示。 */
export function formatDuration(seconds: number | null): string {
  if (seconds === null) return "不可用";
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  return [days ? `${days} 天` : "", hours ? `${hours} 小时` : "", `${minutes} 分钟`]
    .filter(Boolean)
    .join(" ");
}
