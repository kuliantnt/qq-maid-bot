import type { MemoryCategory, MemoryKind, MemoryTargetView, MemoryVisibility } from "../../types.js";

export const KIND_LABELS: Readonly<Record<MemoryKind, string>> = {
  personal: "个人记忆",
  group_profile: "群内用户画像",
  group: "群组记忆",
};

export const CATEGORY_LABELS: Readonly<Record<MemoryCategory, string>> = {
  note: "普通记录",
  preference: "偏好",
  identity: "身份",
  relation: "关系",
  instruction: "指令",
};

export const VISIBILITY_LABELS: Readonly<Record<MemoryVisibility, string>> = {
  private: "仅本人",
  context_only: "当前上下文",
  group_members: "群成员",
  public: "公开",
};

export const CATEGORY_OPTIONS: ReadonlyArray<readonly [MemoryCategory | "", string]> = [
  ["", "全部分类"],
  ["note", "普通记录"],
  ["preference", "偏好"],
  ["identity", "身份"],
  ["relation", "关系"],
  ["instruction", "指令"],
];

/** 各范围支持的可见性集合；创建表单随所选 target 联动。 */
export const VISIBILITY_OPTIONS: Readonly<Record<MemoryKind, ReadonlyArray<readonly [MemoryVisibility, string]>>> = {
  personal: [
    ["private", "仅本人"],
    ["context_only", "当前上下文"],
    ["public", "公开"],
  ],
  group_profile: [
    ["context_only", "当前上下文"],
    ["group_members", "群成员"],
    ["public", "公开"],
  ],
  group: [
    ["group_members", "群成员"],
    ["public", "公开"],
  ],
};

/** 管理 API 只返回 opaque ref；页面不尝试反解析原始账号、群组或用户 ID。 */
export function memoryTargetLabel(target: MemoryTargetView): string {
  return [
    KIND_LABELS[target.scope],
    target.platform,
    `账号 ${compactRef(target.accountRef)}`,
    target.groupRef ? `群组 ${compactRef(target.groupRef)}` : null,
    target.subjectRef ? `用户 ${compactRef(target.subjectRef)}` : null,
  ]
    .filter((value): value is string => value !== null)
    .join(" · ");
}

export function canClearTarget(target: MemoryTargetView): boolean {
  return target.capabilities.canClearTarget;
}

export function canDisableGroupProfile(target: MemoryTargetView): boolean {
  return target.scope === "group_profile" && target.capabilities.canDisableGroupProfile;
}

export function canCreateMemory(target: MemoryTargetView): boolean {
  return target.scope !== "group_profile" || target.capabilities.canDisableGroupProfile;
}

export function compactRef(value: string): string {
  return value.length <= 32 ? value : `${value.slice(0, 22)}…${value.slice(-8)}`;
}

export function uniqueRefs(refs: readonly (string | null)[]): string[] {
  return [...new Set(refs.filter((value): value is string => value !== null))];
}
