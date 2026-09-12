import { configFieldLabel } from "./configuration-navigation.js";

/**
 * TTS 字段的专用输入约束。
 *
 * 从旧版 views/configuration/tts.ts 平移：Provider 是受控下拉而不是自由文本，
 * 数字字段必须完整通过整数与边界校验，不能沿用普通整数的宽松语义。
 */

export const TTS_PROVIDER_OPTIONS: ReadonlyArray<readonly [string, string]> = [
  ["disabled", "关闭"],
  ["qwen", "千问"],
];

export const TTS_NUMBER_RANGES: Readonly<Record<string, readonly [number, number]>> = {
  "delivery.tts.request_timeout_seconds": [1, 120],
  "delivery.tts.max_text_chars": [1, 600],
};

export const TTS_PROVIDER_KEY = "delivery.tts.provider";

/** Provider 下拉选项：保留未知历史值，避免保存时把自定义 Provider 静默改写。 */
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
