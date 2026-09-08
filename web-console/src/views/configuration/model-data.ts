import { array, record, string } from "./fields.js";

export interface ManagedModel { id: string; metadata: Record<string, unknown>; sources: string[]; override: Record<string, unknown> }
export function mergeModels(discovery: Record<string, unknown> | null, metadata: Record<string, unknown>): ManagedModel[] {
  const rows = new Map<string, ManagedModel>();
  for (const value of array(metadata.models)) {
    const model = record(value), id = string(model.id);
    const local = array(metadata.overrides).map(record).find(item => item.id === id) ?? {};
    const sources = Object.values(record(model.provenance)).some(value => value === "catalog" || value === "official_patch") ? ["catalog"] : [];
    if (Object.keys(local).length) sources.push("local override");
    rows.set(id, { id, metadata: model, sources, override: local });
  }
  if (discovery?.state === "success") for (const value of array(discovery.models)) {
    const id = string(record(value).id);
    const row = rows.get(id) ?? { id, metadata: {}, sources: [], override: {} };
    row.sources.unshift("connection discovery"); rows.set(id, row);
  }
  return [...rows.values()].sort((a, b) => Number(b.sources.includes("connection discovery")) - Number(a.sources.includes("connection discovery")) || a.id.localeCompare(b.id));
}
export function discoveryLabel(result: Record<string, unknown> | null): string {
  if (!result) return "尚未获取模型（unknown）";
  const labels: Record<string, string> = { success: array(result.models).length ? `获取成功：${array(result.models).length} 个模型` : "获取成功：Connection 返回空列表", unsupported: "不支持模型发现（unsupported）", failed: "获取失败（failed）", unknown: "无法确认模型列表（unknown）" };
  return `${labels[string(result.state)] ?? labels.unknown} · ${result.category ?? ""}`;
}
export function filterModels(rows: ManagedModel[], query: string, status: string): ManagedModel[] {
  const term = query.trim().toLowerCase();
  return rows.filter(row => `${row.id} ${string(row.metadata.display_name)}`.toLowerCase().includes(term)
    && (status === "all" || (status === "disabled" ? row.metadata.enabled === false : status === "enabled" ? row.metadata.enabled !== false : row.metadata.status === status)));
}
