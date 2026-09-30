/** Connection 发现列表是实际暴露集合；目录与本地覆盖只补充展示，不验证运行能力。
 * 合并语义与旧版 model-data.ts 一致：发现优先、未知私有模型保留、仅精确 ID 补充目录来源。 */

export interface ManagedModel {
  id: string;
  metadata: Record<string, unknown>;
  sources: string[];
  override: Record<string, unknown>;
}

function entriesOf(value: unknown): Array<[string, unknown]> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return [];
  return Object.entries(value as Record<string, unknown>);
}

function modelsOf(value: unknown): Record<string, unknown>[] {
  if (typeof value !== "object" || value === null || !Array.isArray((value as Record<string, unknown>).models)) return [];
  return ((value as Record<string, unknown>).models as unknown[]).filter(
    (item): item is Record<string, unknown> => typeof item === "object" && item !== null && !Array.isArray(item),
  );
}

/** 本地 metadata（目录条目 + 本地覆盖）与 discovery 结果合并为展示行。 */
export function mergeModels(discovery: Record<string, unknown> | null, metadata: Record<string, unknown>): ManagedModel[] {
  const rows = new Map<string, ManagedModel>();
  const overrides = modelsOf({ models: metadata.overrides });
  for (const model of modelsOf(metadata)) {
    const id = typeof model.id === "string" ? model.id : "";
    if (!id) continue;
    const local = overrides.find((item) => item.id === id) ?? {};
    const fromCatalog = entriesOf(model.provenance).some(([, value]) => value === "catalog" || value === "official_patch");
    const sources = fromCatalog ? ["catalog"] : [];
    if (Object.keys(local).length) sources.push("local override");
    rows.set(id, { id, metadata: model, sources, override: local });
  }
  // 仅 success 携带真实模型列表；failed 的 models 可能是历史残留，不并入展示。
  if (discovery?.state === "success") {
    for (const model of modelsOf(discovery)) {
      const id = typeof model.id === "string" ? model.id : "";
      if (!id) continue;
      const row = rows.get(id) ?? { id, metadata: {}, sources: [], override: {} };
      row.sources.unshift("connection discovery");
      rows.set(id, row);
    }
  }
  return [...rows.values()].sort(
    (a, b) => Number(b.sources.includes("connection discovery")) - Number(a.sources.includes("connection discovery")) || a.id.localeCompare(b.id),
  );
}

/** 获取状态文案：unsupported / failed / unknown / success（含空列表）互不混淆。 */
export function discoveryLabel(result: Record<string, unknown> | null): string {
  if (!result) return "尚未获取模型（unknown）";
  const models = Array.isArray(result.models) ? result.models : [];
  const labels: Record<string, string> = {
    success: models.length ? `获取成功：${models.length} 个模型` : "获取成功：Connection 返回空列表",
    unsupported: "不支持模型发现（unsupported）",
    failed: "获取失败（failed）",
    unknown: "无法确认模型列表（unknown）",
  };
  const label = labels[typeof result.state === "string" ? result.state : ""] ?? labels.unknown;
  return `${label} · ${typeof result.category === "string" ? result.category : ""}`;
}

/** 搜索 + 状态过滤；disabled 只匹配本地显式禁用，active/beta/deprecated 按目录状态。 */
export function filterModels(rows: ManagedModel[], query: string, status: string): ManagedModel[] {
  const term = query.trim().toLowerCase();
  return rows.filter((row) => {
    const displayName = typeof row.metadata.display_name === "string" ? row.metadata.display_name : "";
    if (!`${row.id} ${displayName}`.toLowerCase().includes(term)) return false;
    if (status === "all") return true;
    if (status === "disabled") return row.metadata.enabled === false;
    if (status === "enabled") return row.metadata.enabled !== false;
    return row.metadata.status === status;
  });
}
