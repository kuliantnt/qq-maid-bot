import { useState } from "react";
import { getDefaultStore, useAtomValue } from "jotai";
import { CONSOLE_THEMES, CONSOLE_THEME_IDS, isConsoleThemePreset } from "../../theme.js";
import { themeController, themePresetAtom } from "../../stores/theme.js";
import { updatePreferences } from "../../stores/user-data.js";
import { Button } from "../../components/ui/button.js";

const COLOR_INPUT_PATTERN = /^#[0-9a-f]{6}$/i;

/**
 * 界面偏好：主题预设、恢复默认与自定义三色。
 *
 * 契约与旧版一致：预设切换只保存 customColors: []（服务端仅存自定义色，预设归
 * localStorage），服务端写入成功后才应用本地；保存中禁止并发提交。
 */
export function ThemePreferencesSection() {
  const preset = useAtomValue(themePresetAtom);
  const [status, setStatus] = useState("当前主题：" + CONSOLE_THEMES[preset].name);
  const [customColors, setCustomColors] = useState("");
  const [saveInFlight, setSaveInFlight] = useState(false);

  const savePreference = async (
    patch: Parameters<typeof updatePreferences>[0],
    success: string,
    failure: string,
    apply: () => void,
  ): Promise<void> => {
    if (saveInFlight) return;
    setSaveInFlight(true);
    setStatus("正在保存界面偏好……");
    try {
      await updatePreferences(patch);
      apply();
      setStatus(success);
    } catch (cause) {
      setStatus(cause instanceof Error ? `${failure}：${cause.message}` : failure);
    } finally {
      setSaveInFlight(false);
    }
  };

  const selectPreset = (nextPreset: string): void => {
    if (!isConsoleThemePreset(nextPreset)) return;
    void savePreference({ customColors: [] }, "主题已保存。", "主题保存失败", () => {
      const preference = themeController.select(nextPreset);
      getDefaultStore().set(themePresetAtom, preference.preset);
      setStatus(`当前主题：${CONSOLE_THEMES[preference.preset].name}`);
    });
  };

  const saveCustomColors = (): void => {
    const colors = customColors.split(",").map((value) => value.trim());
    if (colors.length !== 3 || colors.some((color) => !COLOR_INPUT_PATTERN.test(color))) {
      setStatus("请输入三个六位十六进制颜色。");
      return;
    }
    void savePreference({ customColors: colors }, "自定义颜色已保存。", "自定义颜色保存失败", () => {
      themeController.applyCustomColors(colors);
    });
  };

  return (
    <section aria-label="界面主题偏好" className="flex flex-col gap-3">
      <h3 className="m-0 text-base font-bold">主题</h3>
      <div role="radiogroup" aria-label="主题预设" className="flex flex-wrap gap-2">
        {CONSOLE_THEME_IDS.map((id) => {
          const theme = CONSOLE_THEMES[id];
          const selected = preset === id;
          return (
            <label
              key={id}
              className={`flex cursor-pointer items-center gap-2 border px-3 py-2 text-sm ${
                selected ? "border-accent bg-accent-soft text-accent" : "border-line text-ink hover:bg-accent-soft"
              }`}
            >
              <input
                type="radio"
                name="console-theme"
                value={id}
                checked={selected}
                onChange={() => selectPreset(id)}
                className="size-3.5 accent-[var(--console-accent)]"
              />
              <span className="font-semibold">{theme.name}</span>
              <span className="text-xs text-muted">{theme.description}</span>
              <span aria-label={`${theme.name} 的背景、卡片和强调色预览`} className="flex gap-0.5">
                {(["background", "card", "accent"] as const).map((role) => (
                  <span
                    key={role}
                    title={role === "background" ? "背景" : role === "card" ? "卡片" : "强调色"}
                    style={{ backgroundColor: theme[role] }}
                    className="inline-block size-3 border border-line"
                  />
                ))}
              </span>
            </label>
          );
        })}
      </div>
      <p aria-live="polite" role="status" className="m-0 text-xs text-muted">
        {status}
      </p>
      <div>
        <Button variant="secondary" disabled={saveInFlight} onClick={() => {
          void savePreference({ customColors: [] }, "已恢复默认主题。", "恢复默认主题失败", () => {
            const preference = themeController.reset();
            getDefaultStore().set(themePresetAtom, preference.preset);
            setStatus(`当前主题：${CONSOLE_THEMES[preference.preset].name}`);
          });
        }} className="px-2.5 py-1 text-xs">
          恢复默认
        </Button>
      </div>
      <div className="flex flex-col gap-1">
        <label htmlFor="custom-colors-input" className="text-sm font-semibold text-ink">
          自定义颜色（背景、主文字、强调色）
        </label>
        <div className="flex gap-2">
          <input
            id="custom-colors-input"
            type="text"
            value={customColors}
            placeholder="#0D1117, #E6EDF3, #3FB950"
            onChange={(event) => setCustomColors(event.target.value)}
            className="max-w-96 flex-1 border border-line bg-input px-3 py-2 text-sm text-ink outline-none placeholder:text-muted focus-visible:border-accent"
          />
          <Button variant="secondary" disabled={saveInFlight} onClick={saveCustomColors} className="px-2.5 py-1 text-xs">
            保存颜色
          </Button>
        </div>
      </div>
    </section>
  );
}
