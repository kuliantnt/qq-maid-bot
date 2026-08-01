import type { ConfigFieldSnapshot } from "../../types.js";
import { configFieldLabel } from "./navigation.js";
import { current } from "./state.js";
import { inputId, string } from "./fields.js";

export const TTS_PROVIDER_OPTIONS: ReadonlyArray<readonly [string, string]> = [
  ["disabled", "关闭"],
  ["qwen", "千问"],
];

export const TTS_NUMBER_RANGES: Readonly<Record<string, readonly [number, number]>> = {
  "delivery.tts.request_timeout_seconds": [1, 120],
  "delivery.tts.max_text_chars": [1, 600],
};

export function ttsProviderOptions(currentValue: unknown): Array<[string, string]> {
  const current = currentValue === null || currentValue === undefined ? "disabled" : String(currentValue);
  const options = TTS_PROVIDER_OPTIONS.map(([value, label]): [string, string] => [value, label]);
  if (!options.some(([value]) => value === current)) {
    options.push([current, `${current}（当前自定义值）`]);
  }
  return options;
}

export function ttsNumberRange(key: string): readonly [number, number] | null {
  return TTS_NUMBER_RANGES[key] ?? null;
}

/** TTS 范围字段必须先完整通过整数与边界校验，不能沿用普通整数的宽松 parseInt 语义。 */
export function parseTtsNumberValue(key: string, rawValue: string): number {
  const range = ttsNumberRange(key);
  if (!range) throw new Error(`${configFieldLabel(key)}没有可用的页面输入范围`);
  const value = rawValue.trim() === "" ? Number.NaN : Number(rawValue);
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < range[0] || value > range[1]) {
    throw new Error(`${configFieldLabel(key)}必须是 ${range[0]} 到 ${range[1]} 之间的整数`);
  }
  return value;
}

export function decorateTtsRow(row: HTMLElement, field: ConfigFieldSnapshot): void {
  row.dataset.configFieldKey = field.key;
  if (field.key.startsWith("delivery.tts.qwen_")) row.dataset.ttsQwenField = "true";
}

/** 关闭 Provider 只做视觉提示，字段始终保持可编辑且不会生成清除操作。 */
export function bindTtsProviderState(): void {
  const provider = document.getElementById(inputId("delivery.tts.provider"));
  if (!(provider instanceof HTMLSelectElement)) return;
  const refresh = (): void => {
    const disabled = provider.value === "disabled";
    for (const row of document.querySelectorAll<HTMLElement>("[data-tts-qwen-field='true']")) {
      row.classList.toggle("config-row-muted", disabled);
    }
  };
  provider.addEventListener("change", refresh);
  refresh();
}

