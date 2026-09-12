import type { ConfigFieldSnapshot, ConfigurationSnapshot } from "../../types.js";
import { configFieldLabel } from "./configuration-navigation.js";
import { parseTtsNumberValue, ttsNumberRange } from "./tts-config.js";

/** 页面输入值 ↔ 后端 ConfigValueType 的双向投影（与旧 fields.ts 语义一致）。 */

export function configInputValue(field: ConfigFieldSnapshot): string {
  const baseline = field.savedValue ?? field.effectiveValue;
  if (baseline === null || baseline === undefined) return "";
  if (field.valueType === "string_list") {
    return Array.isArray(baseline) ? baseline.join(", ") : String(baseline);
  }
  return String(baseline);
}

/** 空字符串对未配置的可选字段表示“未修改”，不参与变更收集。 */
export function isEmptyInputValue(value: unknown): boolean {
  return value === "" || value === null || value === undefined;
}

/** 把输入框值投影为后端类型；不合法值抛错由保存流程展示。 */
export function parseConfigInputValue(field: ConfigFieldSnapshot, raw: string): unknown {
  switch (field.valueType) {
    case "boolean":
      return raw === "true" || raw === "on" || raw === "1";
    case "integer": {
      // TTS 等有后端边界约束的字段必须先通过范围校验，错误信息与旧版一致。
      if (ttsNumberRange(field.key)) return parseTtsNumberValue(field.key, raw);
      const parsed = Number(raw);
      if (!Number.isInteger(parsed)) throw new Error(`${configFieldLabel(field.key)} 需要是整数`);
      return parsed;
    }
    case "string_list": {
      const items = raw.split(/[,，]/).map((item) => item.trim()).filter((item) => item !== "");
      return items;
    }
    default:
      return raw;
  }
}

/** 摘要徽章的共享计算：预检状态与待重启数量。 */
export function configurationSummary(snapshot: ConfigurationSnapshot): {
  invalidCount: number;
  pendingCount: number;
} {
  const invalidCount = snapshot.fields.filter((field) => !field.valid).length;
  const pendingCount =
    snapshot.fields.filter((field) => field.pendingRestart).length + (snapshot.agent?.pendingRestart ? 1 : 0);
  return { invalidCount, pendingCount };
}
