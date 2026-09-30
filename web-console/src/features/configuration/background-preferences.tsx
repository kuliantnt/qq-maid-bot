import { useState } from "react";
import { useAtomValue } from "jotai";
import { showToast } from "../../stores/toast.js";
import { backgroundController } from "../../stores/background.js";
import { deleteFile, updatePreferences, uploadFile, userDataAtom } from "../../stores/user-data.js";
import { Button } from "../../components/ui/button.js";
import type { BackgroundMode } from "../../background.js";
import { activateBackgroundFile } from "./background-activation.js";
import type { UserFile } from "../../types.js";

const BACKGROUND_MODE_OPTIONS: ReadonlyArray<{ mode: BackgroundMode; name: string; description: string }> = [
  { mode: "default", name: "无背景", description: "不显示背景，仅使用主题底色" },
  { mode: "special", name: "特殊九宫格", description: "需控制台解锁" },
];

/**
 * 背景偏好：模式选择与自定义背景文件管理。
 *
 * 模式切换先写服务端（activeBackgroundFileId 同一次清空）再应用本地；
 * 自定义背景激活统一走 activateBackgroundFile 的回滚流程。
 */
export function BackgroundPreferencesSection() {
  const userData = useAtomValue(userDataAtom);
  const [status, setStatus] = useState("");
  const [saveInFlight, setSaveInFlight] = useState(false);

  if (!userData) {
    return (
      <section aria-label="背景偏好" className="flex flex-col gap-2">
        <h3 className="m-0 text-base font-bold">背景</h3>
        <p role="status" className="m-0 text-xs text-muted">界面偏好尚未加载完成。</p>
      </section>
    );
  }

  const { preferences, files } = userData;
  const selection = backgroundController.selection();
  const unlocked = backgroundController.isUnlocked();
  const lastError = backgroundController.lastError();
  const statusText = lastError
    || (status || (selection.activeFileId
      ? "当前背景：自定义图片"
      : selection.mode === "special"
        ? unlocked ? "当前背景：特殊九宫格" : "当前背景：特殊九宫格（未解锁）"
        : "当前背景：无背景"));

  const savePreference = async (patch: Parameters<typeof updatePreferences>[0], success: string, apply: () => void): Promise<void> => {
    if (saveInFlight) return;
    setSaveInFlight(true);
    setStatus("正在保存界面偏好……");
    try {
      await updatePreferences(patch);
      apply();
      setStatus(success);
    } catch (cause) {
      setStatus(cause instanceof Error ? `背景保存失败：${cause.message}` : "背景保存失败");
    } finally {
      setSaveInFlight(false);
    }
  };

  const selectMode = (mode: BackgroundMode): void => {
    void savePreference({ backgroundMode: mode, activeBackgroundFileId: null }, "背景已保存。", () => {
      backgroundController.select(mode);
    });
  };
  const activateFile = (file: Parameters<typeof activateBackgroundFile>[1], forceRefresh: boolean): void => {
    if (saveInFlight) return;
    setSaveInFlight(true);
    void activateBackgroundFile({
      preferences,
      updatePreferences,
      controller: backgroundController,
      setStatus,
    }, file, forceRefresh).finally(() => setSaveInFlight(false));
  };

  const removeFile = (file: UserFile): void => {
    void deleteFile(file)
      .then(() => setStatus("背景文件已删除。"))
      .catch((cause) => showToast("error", cause instanceof Error ? cause.message : "背景文件删除失败"));
  };

  const uploadAndActivate = (input: HTMLInputElement): void => {
    const file = input.files?.[0];
    if (!file) return;
    void uploadFile(file)
      .then((uploaded) => {
        setStatus(`${uploaded.filename} 已上传。`);
        input.value = "";
        activateFile(uploaded, true);
      })
      .catch((cause) => showToast("error", cause instanceof Error ? cause.message : "背景文件上传失败"));
  };

  return (
    <section aria-label="背景偏好" className="flex flex-col gap-3">
      <h3 className="m-0 text-base font-bold">背景</h3>
      <div role="radiogroup" aria-label="背景模式" className="flex flex-wrap gap-2">
        {BACKGROUND_MODE_OPTIONS.map((option) => {
          const selected = selection.mode === option.mode;
          const disabled = option.mode === "special" && !unlocked;
          return (
            <label
              key={option.mode}
              className={`flex items-center gap-2 border px-3 py-2 text-sm ${
                selected ? "border-accent bg-accent-soft text-accent" : "border-line text-ink hover:bg-accent-soft"
              } ${disabled ? "cursor-not-allowed opacity-50" : "cursor-pointer"}`}
            >
              <input
                type="radio"
                name="console-background"
                value={option.mode}
                checked={selected}
                disabled={disabled}
                onChange={() => selectMode(option.mode)}
                className="size-3.5 accent-[var(--console-accent)]"
              />
              <span className="font-semibold">{option.name}</span>
              <span className="text-xs text-muted">{option.description}</span>
            </label>
          );
        })}
      </div>
      <p aria-live="polite" role="status" className={`m-0 text-xs leading-relaxed ${lastError ? "font-semibold text-warning" : "text-muted"}`}>
        {statusText}
      </p>

      {files.length > 0 ? (
        <div className="flex flex-col gap-1.5">
          {files.map((file) => (
            <div key={file.fileId} className="flex items-center gap-2 text-sm">
              <span className="min-w-0 flex-1 truncate text-ink">{file.filename}</span>
              <Button variant="secondary" disabled={saveInFlight} onClick={() => activateFile(file, true)} className="px-2.5 py-1 text-xs">
                使用
              </Button>
              <Button variant="danger" disabled={saveInFlight} onClick={() => removeFile(file)} className="px-2.5 py-1 text-xs">
                删除
              </Button>
            </div>
          ))}
        </div>
      ) : null}

      <div className="flex flex-col gap-1">
        <label htmlFor="background-file-input" className="text-sm font-semibold text-ink">
          自定义背景图片
        </label>
        <input
          id="background-file-input"
          type="file"
          accept="image/*"
          disabled={saveInFlight}
          onChange={(event) => {
            if (event.target instanceof HTMLInputElement) uploadAndActivate(event.target);
          }}
          className="text-sm text-muted"
        />
      </div>
    </section>
  );
}
