import { useState } from "react";
import { Input } from "../../components/ui/field.js";

/**
 * 模型候选路线 Chip 编辑器。
 *
 * 从旧版 views/configuration/model-route-editor.ts 平移：视觉上以独立 Chip 展示每个
 * provider:model 值（完整文本、删除、拖动/上移下移排序），候选列表作为受控状态由
 * Agent 草稿持有，保存契约仍是 set_model_route，值语义与旧版完全一致。
 */

/** 归一化候选列表：去首尾空白、过滤空值、按精确匹配去重，保持顺序。 */
export function normalizeCandidates(values: readonly string[]): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const raw of values) {
    const value = raw.trim();
    if (!value) continue;
    if (seen.has(value)) continue;
    seen.add(value);
    result.push(value);
  }
  return result;
}

/** 候选格式校验：必须包含冒号，且 provider 与 model 均非空。 */
export function isMalformedCandidate(value: string): boolean {
  const trimmed = value.trim();
  if (!trimmed) return true;
  const colon = trimmed.indexOf(":");
  if (colon <= 0 || colon === trimmed.length - 1) return true;
  return false;
}

export interface AddCandidateResult {
  list: string[];
  error: string | null;
}

/** 追加候选到末尾；空值、重复、非法格式返回明确错误。 */
export function addCandidate(list: readonly string[], candidate: string): AddCandidateResult {
  const value = candidate.trim();
  if (!value) return { list: [...list], error: "模型不能为空" };
  if (isMalformedCandidate(value)) return { list: [...list], error: "格式应为 provider:model" };
  if (list.some((item) => item === value)) return { list: [...list], error: "该模型已在路线中" };
  return { list: [...list, value], error: null };
}

/** 删除指定位置的候选。 */
export function removeCandidate(list: readonly string[], index: number): string[] {
  if (index < 0 || index >= list.length) return [...list];
  return [...list.slice(0, index), ...list.slice(index + 1)];
}

/** 将候选从 from 移动到 to（越界时收敛到边界），保持其余顺序。 */
export function moveCandidate(list: readonly string[], from: number, to: number): string[] {
  if (from < 0 || from >= list.length) return [...list];
  const target = Math.max(0, Math.min(to, list.length - 1));
  if (from === target) return [...list];
  const next = [...list];
  const moved = next.splice(from, 1)[0];
  if (moved === undefined) return [...list];
  next.splice(target, 0, moved);
  return next;
}

export interface ModelRouteEditorProps {
  label: string;
  candidates: readonly string[];
  disabled: boolean;
  onChange: (candidates: string[]) => void;
}

/** Chip 编辑器：排序即时生效（上移/下移/拖动），删除与添加同样即时；错误经 aria-live 播报。 */
export function ModelRouteEditor({ label, candidates, disabled, onChange }: ModelRouteEditorProps) {
  const [addValue, setAddValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [dragFrom, setDragFrom] = useState<number | null>(null);
  const [dragOverIndex, setDragOverIndex] = useState<number | null>(null);

  const tryAdd = (): void => {
    const result = addCandidate(candidates, addValue);
    if (result.error) {
      setError(result.error);
      return;
    }
    setError(null);
    setAddValue("");
    onChange(result.list);
  };

  return (
    <div className="flex flex-col gap-1.5">
      <p className="m-0 text-sm font-semibold text-ink">{label}</p>
      <div role="list" aria-label={`${label}候选模型`} className="flex flex-wrap gap-1.5">
        {candidates.map((candidate, index) => (
          <span
            key={candidate}
            role="listitem"
            aria-label={`候选 ${candidate}，拖动或使用上移/下移调整顺序`}
            draggable={!disabled}
            onDragStart={() => setDragFrom(index)}
            onDragEnd={() => {
              setDragFrom(null);
              setDragOverIndex(null);
            }}
            onDragOver={(event) => {
              event.preventDefault();
              setDragOverIndex(index);
            }}
            onDragLeave={() => setDragOverIndex((current) => (current === index ? null : current))}
            onDrop={(event) => {
              event.preventDefault();
              setDragOverIndex(null);
              if (dragFrom !== null && dragFrom !== index) {
                onChange(moveCandidate(candidates, dragFrom, index));
              }
              setDragFrom(null);
            }}
            className={`flex items-center gap-1 border border-line bg-glass-muted px-2 py-1 text-xs font-mono text-ink ${
              dragOverIndex === index && dragFrom !== null && dragFrom !== index ? "border-accent" : ""
            } ${disabled ? "opacity-70" : ""}`}
          >
            <span aria-hidden="true" className={disabled ? "hidden" : "text-muted"}>≡</span>
            <span>{candidate}</span>
            {disabled ? null : (
              <>
                <button
                  type="button"
                  aria-label={`上移 ${candidate}`}
                  onClick={() => onChange(moveCandidate(candidates, index, index - 1))}
                  className="px-0.5 text-muted hover:text-accent"
                >
                  ↑
                </button>
                <button
                  type="button"
                  aria-label={`下移 ${candidate}`}
                  onClick={() => onChange(moveCandidate(candidates, index, index + 1))}
                  className="px-0.5 text-muted hover:text-accent"
                >
                  ↓
                </button>
                <button
                  type="button"
                  aria-label={`删除 ${candidate}`}
                  onClick={() => onChange(removeCandidate(candidates, index))}
                  className="px-0.5 text-muted hover:text-error"
                >
                  ×
                </button>
              </>
            )}
          </span>
        ))}
        {candidates.length === 0 ? (
          <span className="text-xs text-muted">暂无候选模型</span>
        ) : null}
      </div>
      <div className="flex gap-2">
        <Input
          type="text"
          placeholder="provider:model"
          disabled={disabled}
          aria-label={`为 ${label} 添加候选模型`}
          value={addValue}
          onChange={(event) => setAddValue(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              tryAdd();
            }
          }}
          className="max-w-64 flex-1 py-1.5 text-sm"
        />
        <button
          type="button"
          disabled={disabled}
          onClick={tryAdd}
          className="border border-line bg-glass px-3 py-1.5 text-xs font-bold text-ink transition-colors hover:bg-accent-soft disabled:opacity-50"
        >
          添加
        </button>
      </div>
      <p role="status" aria-live="polite" className="m-0 min-h-4 text-xs text-error">
        {error}
      </p>
    </div>
  );
}
