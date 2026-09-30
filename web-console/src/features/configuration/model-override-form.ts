/** 本地模型 override 表单的纯状态逻辑；只产出 models.json 已有的可选元数据。
 * 留空 / “继承”不复制目录默认值；没有目录信息时保持未知，能力声明仅用于展示。 */

export const MODALITY_OPTIONS = ["text", "image", "audio", "video", "pdf"] as const;

export const CAPABILITY_KEYS = ["reasoning", "tool_calling", "vision"] as const;
export type CapabilityKey = (typeof CAPABILITY_KEYS)[number];

export const CAPABILITY_LABELS: Record<CapabilityKey, string> = {
  reasoning: "推理",
  tool_calling: "工具调用",
  vision: "视觉",
};

export interface ModelOverrideDraft {
  id: string;
  display_name: string;
  context_window: string;
  max_output_tokens: string;
  status: string;
  /** "" 表示继承（启用）；"true" / "false" 为显式本地启停。 */
  enabled: string;
  inputPrice: string;
  outputPrice: string;
  modalitiesInherit: boolean;
  inputModalities: string[];
  outputModalities: string[];
  claims: Record<CapabilityKey, string>;
}

export function emptyModelOverrideDraft(): ModelOverrideDraft {
  return {
    id: "",
    display_name: "",
    context_window: "",
    max_output_tokens: "",
    status: "",
    enabled: "",
    inputPrice: "",
    outputPrice: "",
    modalitiesInherit: true,
    inputModalities: [],
    outputModalities: [],
    claims: { reasoning: "", tool_calling: "", vision: "" },
  };
}

function recordOf(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

/** 从既有 override 条目填充草稿；undefined/null 字段一律回到“继承”状态。 */
export function draftFromOverride(modelId: string, value: Record<string, unknown>): ModelOverrideDraft {
  const price = recordOf(value.price);
  const modalities = recordOf(value.modalities);
  const capabilities = recordOf(value.capabilities);
  const claims = Object.fromEntries(
    CAPABILITY_KEYS.map((key) => {
      const advertised = recordOf(capabilities[key]).advertised;
      return [key, advertised == null ? "" : String(advertised)];
    }),
  ) as Record<CapabilityKey, string>;
  const inputModalities = Array.isArray(modalities.input) ? modalities.input.filter((item): item is string => typeof item === "string") : [];
  const outputModalities = Array.isArray(modalities.output) ? modalities.output.filter((item): item is string => typeof item === "string") : [];
  return {
    id: modelId,
    display_name: typeof value.display_name === "string" ? value.display_name : "",
    context_window: value.context_window == null ? "" : String(value.context_window),
    max_output_tokens: value.max_output_tokens == null ? "" : String(value.max_output_tokens),
    status: typeof value.status === "string" ? value.status : "",
    enabled: value.enabled == null ? "" : String(value.enabled),
    inputPrice: price.input_per_million_usd == null ? "" : String(price.input_per_million_usd),
    outputPrice: price.output_per_million_usd == null ? "" : String(price.output_per_million_usd),
    modalitiesInherit: value.modalities == null,
    inputModalities,
    outputModalities,
    claims,
  };
}

function numeric(value: string, tokens: boolean): number {
  const number = Number(value);
  if (!Number.isFinite(number) || number < (tokens ? 1 : 0) || (tokens && !Number.isSafeInteger(number))) {
    throw new Error(tokens ? "token 数必须是正整数" : "价格必须是非负数字");
  }
  return number;
}

/** 草稿 → update_model_override 条目；ID 必填，留空字段不产出键（保持继承语义）。 */
export function buildModelOverride(draft: ModelOverrideDraft): Record<string, unknown> {
  if (!draft.id.trim()) throw new Error("请填写模型 ID");
  const result: Record<string, unknown> = { id: draft.id.trim() };
  if (draft.display_name.trim()) result.display_name = draft.display_name.trim();
  if (draft.context_window) result.context_window = numeric(draft.context_window, true);
  if (draft.max_output_tokens) result.max_output_tokens = numeric(draft.max_output_tokens, true);
  if (draft.status) result.status = draft.status;
  if (draft.enabled) result.enabled = draft.enabled === "true";
  if (draft.inputPrice || draft.outputPrice) {
    result.price = {
      input_per_million_usd: draft.inputPrice ? numeric(draft.inputPrice, false) : null,
      output_per_million_usd: draft.outputPrice ? numeric(draft.outputPrice, false) : null,
    };
  }
  if (!draft.modalitiesInherit) {
    result.modalities = {
      input: draft.inputModalities.filter((item) => (MODALITY_OPTIONS as readonly string[]).includes(item)),
      output: draft.outputModalities.filter((item) => (MODALITY_OPTIONS as readonly string[]).includes(item)),
    };
  }
  const capabilities: Record<string, unknown> = {};
  for (const key of CAPABILITY_KEYS) {
    if (draft.claims[key]) capabilities[key] = { advertised: draft.claims[key] === "true" };
  }
  if (Object.keys(capabilities).length) result.capabilities = capabilities;
  return result;
}
