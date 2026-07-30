import {
  CONSOLE_THEME_IDS,
  CONSOLE_THEMES,
  isConsoleThemePreset,
  type ConsoleThemePreset,
  type ThemeController,
} from "../theme.js";
import type { BackgroundController, BackgroundMode } from "../background.js";

export function renderThemeSelector(
  target: HTMLElement,
  controller: ThemeController,
  backgroundController: BackgroundController,
): void {
  target.replaceChildren();

  const fieldset = document.createElement("fieldset");
  fieldset.className = "console-theme-selector";
  const legend = document.createElement("legend");
  legend.textContent = "Color";
  fieldset.append(legend);

  const choices = document.createElement("div");
  choices.className = "console-theme-choices";
  const status = document.createElement("p");
  status.className = "field-meta";
  status.setAttribute("aria-live", "polite");
  status.id = "console-theme-selection";

  const sync = (preset: ConsoleThemePreset): void => {
    for (const input of choices.querySelectorAll<HTMLInputElement>("input[type=radio]")) {
      const selected = input.value === preset;
      input.checked = selected;
      input.closest("label")?.classList.toggle("console-theme-choice--selected", selected);
    }
    status.textContent = `当前主题：${CONSOLE_THEMES[preset].name}`;
  };

  for (const preset of CONSOLE_THEME_IDS) {
    const theme = CONSOLE_THEMES[preset];
    const label = document.createElement("label");
    label.className = "console-theme-choice";
    const input = document.createElement("input");
    input.type = "radio";
    input.name = "console-theme";
    input.value = preset;
    input.checked = controller.current().preset === preset;
    input.setAttribute("aria-describedby", "console-theme-selection");
    input.addEventListener("change", () => {
      if (!isConsoleThemePreset(input.value)) return;
      controller.select(input.value);
      sync(input.value);
    });

    const name = document.createElement("span");
    name.className = "console-theme-choice-name";
    name.textContent = theme.name;
    const preview = document.createElement("span");
    preview.className = "console-theme-preview";
    preview.setAttribute("aria-label", `预览：深色材质、浅色画布、对色 ${theme.name}`);
    for (const role of ["dark", "light", "contrast"] as const) {
      const swatch = document.createElement("span");
      swatch.className = `console-theme-swatch console-theme-swatch--${role}`;
      swatch.style.backgroundColor = theme[role];
      swatch.title = role === "dark" ? "深色材质" : role === "light" ? "浅色画布" : "对色";
      preview.append(swatch);
    }
    label.append(input, name, preview);
    choices.append(label);
  }

  const reset = document.createElement("button");
  reset.type = "button";
  reset.className = "secondary console-theme-reset";
  reset.textContent = "恢复默认";
  reset.addEventListener("click", () => {
    const preference = controller.reset();
    sync(preference.preset);
  });

  const backgroundFieldset = document.createElement("fieldset");
  backgroundFieldset.className = "console-background-selector";
  const backgroundLegend = document.createElement("legend");
  backgroundLegend.textContent = "Background";
  const backgroundChoices = document.createElement("div");
  backgroundChoices.className = "console-theme-choices console-background-choices";
  const backgroundStatus = document.createElement("p");
  backgroundStatus.className = "field-meta";
  backgroundStatus.setAttribute("aria-live", "polite");

  const syncBackground = (mode: BackgroundMode): void => {
    for (const input of backgroundChoices.querySelectorAll<HTMLInputElement>("input[type=radio]")) {
      const selected = input.value === mode;
      input.checked = selected;
      input.closest("label")?.classList.toggle("console-theme-choice--selected", selected);
    }
    backgroundStatus.textContent = backgroundController.isUnlocked()
      ? mode === "special" ? "当前背景：特殊九宫格" : "当前背景：普通背景"
      : "当前背景：普通背景；特殊背景需先解锁";
  };

  for (const option of [
    { mode: "default" as const, name: "普通背景", description: "默认单张背景图" },
    { mode: "special" as const, name: "特殊九宫格", description: "需控制台解锁" },
  ]) {
    const label = document.createElement("label");
    label.className = "console-theme-choice";
    const input = document.createElement("input");
    input.type = "radio";
    input.name = "console-background";
    input.value = option.mode;
    input.checked = backgroundController.current() === option.mode;
    input.disabled = option.mode === "special" && !backgroundController.isUnlocked();
    input.addEventListener("change", () => {
      const mode = input.value as BackgroundMode;
      backgroundController.select(mode);
      syncBackground(backgroundController.current());
    });
    const name = document.createElement("span");
    name.className = "console-theme-choice-name";
    name.textContent = option.name;
    const description = document.createElement("span");
    description.className = "field-meta";
    description.textContent = option.description;
    label.append(input, name, description);
    backgroundChoices.append(label);
  }

  backgroundFieldset.append(backgroundLegend, backgroundChoices, backgroundStatus);
  fieldset.append(choices, status, reset, backgroundFieldset);
  target.append(fieldset);
  sync(controller.current().preset);
  syncBackground(backgroundController.current());
}
