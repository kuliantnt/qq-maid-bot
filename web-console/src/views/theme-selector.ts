import {
  CONSOLE_THEME_IDS,
  CONSOLE_THEMES,
  isConsoleThemePreset,
  type ConsoleThemePreset,
  type ThemeController,
} from "../theme.js";
import type { BackgroundController, BackgroundMode } from "../background.js";
import type { UserDataController } from "./configuration.js";

export function renderThemeSelector(
  target: HTMLElement,
  controller: ThemeController,
  backgroundController: BackgroundController,
  userData: UserDataController | null = null,
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
  let saveInFlight = false;

  const savePreference = async (
    patch: Parameters<NonNullable<UserDataController["updatePreferences"]>>[0],
    success: string,
    failure: string,
    apply: () => void,
  ): Promise<void> => {
    if (!userData || saveInFlight) return;
    saveInFlight = true;
    status.textContent = "正在保存界面偏好……";
    try {
      await userData.updatePreferences(patch);
      apply();
      status.textContent = success;
    } catch (cause) {
      status.textContent = cause instanceof Error ? `${failure}：${cause.message}` : failure;
    } finally {
      saveInFlight = false;
    }
  };

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
       const nextPreset = input.value;
       void savePreference({ customColors: [] }, "主题已保存。", "主题保存失败", () => {
         controller.select(nextPreset);
         sync(nextPreset);
       });
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
    void savePreference({ customColors: [] }, "已恢复默认主题。", "恢复默认主题失败", () => {
      const preference = controller.reset();
      sync(preference.preset);
    });
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
       void savePreference({ activeBackgroundFileId: null }, "背景已保存。", "背景保存失败", () => {
         backgroundController.select(mode);
         syncBackground(backgroundController.current());
       });
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
  if (userData) {
    const custom = document.createElement("div");
    custom.className = "console-custom-theme-controls";
    const label = document.createElement("label");
    label.textContent = "自定义颜色（深色、浅色、强调色）";
    const input = document.createElement("input");
    input.type = "text";
    input.value = userData.preferences.customColors.join(", ");
    input.placeholder = "#07130F, #E9F4E7, #78E3AD";
    const save = document.createElement("button");
    save.type = "button";
    save.className = "secondary";
    save.textContent = "保存颜色";
    const customStatus = document.createElement("p");
    customStatus.className = "field-meta";
    customStatus.setAttribute("aria-live", "polite");
    save.addEventListener("click", () => {
      const colors = input.value.split(",").map((value) => value.trim());
      if (colors.length !== 3 || colors.some((color) => !/^#[0-9a-f]{6}$/i.test(color))) {
        customStatus.textContent = "请输入三个六位十六进制颜色。";
        return;
      }
      void savePreference({ customColors: colors }, "自定义颜色已保存。", "自定义颜色保存失败", () => {
        controller.applyCustomColors(colors);
        customStatus.textContent = "自定义颜色已保存。";
      });
    });
    custom.append(label, input, save, customStatus);
    fieldset.append(custom);

    if (userData.uploadFile && userData.deleteFile) {
      const filesSection = document.createElement("div");
      filesSection.className = "console-custom-background-controls";
      const fileLabel = document.createElement("label");
      fileLabel.textContent = "自定义背景图片";
      const fileInput = document.createElement("input");
      fileInput.type = "file";
      fileInput.accept = "image/*";
      const fileStatus = document.createElement("p");
      fileStatus.className = "field-meta";
      fileStatus.setAttribute("aria-live", "polite");
      const fileList = document.createElement("div");
      fileList.className = "console-background-file-list";
      for (const file of userData.files) {
        const row = document.createElement("div");
        row.className = "console-background-file-row";
        const name = document.createElement("span");
        name.textContent = file.filename;
        const activate = document.createElement("button");
        activate.type = "button";
        activate.className = "secondary";
        activate.textContent = "使用";
        activate.addEventListener("click", () => {
          void savePreference({
            backgroundFileIds: [...new Set([...userData.preferences.backgroundFileIds, file.fileId])],
            activeBackgroundFileId: file.fileId,
          }, "背景已保存。", "背景保存失败", () => {
            void backgroundController.selectFile(file, true).catch(() => {
              backgroundStatus.textContent = "背景读取失败，已保留原背景。";
              void userData.updatePreferences({ activeBackgroundFileId: null });
            });
          });
        });
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "danger";
        remove.textContent = "删除";
        remove.addEventListener("click", () => {
          void userData.deleteFile?.(file).then(() => {
            backgroundController.deleteFile(file.fileId);
            row.remove();
            fileStatus.textContent = "背景文件已删除。";
          });
        });
        row.append(name, activate, remove);
        fileList.append(row);
      }
      fileInput.addEventListener("change", () => {
        const file = fileInput.files?.[0];
        if (!file) return;
        void userData.uploadFile?.(file).then((uploaded) => {
          fileStatus.textContent = `${uploaded.filename} 已上传，请刷新配置后选择。`;
          fileInput.value = "";
          const name = document.createElement("span");
          name.textContent = uploaded.filename;
          const activate = document.createElement("button");
          activate.type = "button";
          activate.className = "secondary";
          activate.textContent = "使用";
          activate.addEventListener("click", () => {
            void savePreference({
              backgroundFileIds: [...new Set([...userData.preferences.backgroundFileIds, uploaded.fileId])],
              activeBackgroundFileId: uploaded.fileId,
            }, "背景已保存。", "背景保存失败", () => { void backgroundController.selectFile(uploaded, true); });
          });
          const row = document.createElement("div");
          row.className = "console-background-file-row";
          row.append(name, activate);
          fileList.append(row);
        });
      });
      filesSection.append(fileLabel, fileInput, fileList, fileStatus);
      backgroundFieldset.append(filesSection);
    }
  }
}
