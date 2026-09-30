import { Field, Input } from "../../components/ui/field.js";
import { Button } from "../../components/ui/button.js";
import type { ConfigFieldSnapshot } from "../../types.js";
import { configFieldLabel } from "./configuration-navigation.js";
import { configInputValue } from "./configuration-values.js";
import { TTS_PROVIDER_KEY, ttsNumberRange, ttsProviderOptions } from "./tts-config.js";

type PublicFieldRowProps = {
  field: ConfigFieldSnapshot;
  value: string | undefined;
  onChange: (value: string) => void;
  onRemove?: () => void;
  busy?: boolean;
};

/** 普通配置字段行：内置连接卡片与通用配置分节共用同一渲染与变更收集语义（含 TTS 受控下拉）。 */
export function PublicFieldRow({ field, value, onChange, onRemove, busy = false }: PublicFieldRowProps) {
  const current = value ?? configInputValue(field);
  const dirty = value !== undefined;
  const id = `config-${field.key}`;
  const range = ttsNumberRange(field.key);
  return (
    <div className="flex flex-col gap-1 border-b border-line-inner pb-3 last:border-b-0">
      <Field
        label={configFieldLabel(field.key)}
        id={id}
        hint={[
          field.applyMode === "restart" ? "重启后生效" : null,
          field.editable ? null : "只读",
          dirty ? "有未保存修改" : null,
          range ? `范围 ${range[0]} 到 ${range[1]} 的整数` : null,
        ].filter(Boolean).join(" · ") || undefined}
      >
        {(props) =>
          field.key === TTS_PROVIDER_KEY ? (
            // TTS Provider 是受控下拉：保留未知历史值，避免把自定义 Provider 静默改写。
            <select
              {...props}
              disabled={!field.editable}
              value={current === "" ? "disabled" : current}
              onChange={(event) => onChange(event.target.value)}
              className="rounded-console border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
            >
              {ttsProviderOptions(field.savedValue ?? field.effectiveValue).map(([optionValue, label]) => (
                <option key={optionValue} value={optionValue}>{label}</option>
              ))}
            </select>
          ) : field.valueType === "boolean" ? (
            <select
              {...props}
              disabled={!field.editable}
              value={current === "true" ? "true" : "false"}
              onChange={(event) => onChange(event.target.value)}
              className="rounded-console border border-line bg-input px-3 py-2 text-sm text-ink outline-none"
            >
              <option value="true">启用</option>
              <option value="false">关闭</option>
            </select>
          ) : (
            <Input
              {...props}
              type={field.valueType === "integer" ? "number" : "text"}
              disabled={!field.editable}
              value={current}
              onChange={(event) => onChange(event.target.value)}
            />
          )
        }
      </Field>
      {field.savedValue !== null && field.savedValue !== undefined && field.editable && onRemove ? (
        <Button variant="secondary" disabled={busy} onClick={onRemove} className="self-start px-2.5 py-1 text-xs">
          恢复未保存值
        </Button>
      ) : null}
    </div>
  );
}
