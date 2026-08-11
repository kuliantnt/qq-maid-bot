import { MEMORY_ROUTES } from "./api-routes.js";
import {
  ConsoleApiError,
  array,
  finiteNumber,
  mutatingJson,
  nullableString,
  record,
  requiredBoolean,
  requiredFiniteNumber,
  requiredNonNegativeInteger,
  requiredPositiveInteger,
  requiredString,
} from "./api.js";
import type {
  MemoryCategory,
  MemoryConfirmation,
  MemoryCreateInput,
  MemoryItem,
  MemoryKind,
  MemoryListParams,
  MemoryOperation,
  MemoryOperationCapabilities,
  MemoryPage,
  MemorySourceType,
  MemoryStatus,
  MemoryTargetPage,
  MemoryTargetView,
  MemoryUpdateInput,
  MemoryVersionedInput,
  MemoryVisibility,
} from "./types.js";

export async function listMemories(params: MemoryListParams): Promise<MemoryPage> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.list, "POST", {
    page: params.page,
    page_size: params.pageSize,
    ...(params.scope === "all" ? {} : { scope: params.scope }),
    ...(params.status === "all" ? {} : { status: params.status }),
    ...(params.category === "all" ? {} : { category: params.category }),
    ...(params.visibility === "all" ? {} : { visibility: params.visibility }),
    ...(params.pinned === "all" ? {} : { pinned: params.pinned === "true" }),
    ...(params.keyword ? { keyword: params.keyword } : {}),
    ...(params.platform ? { platform: params.platform } : {}),
    ...(params.accountRef ? { account_ref: params.accountRef } : {}),
    ...(params.groupRef ? { group_ref: params.groupRef } : {}),
    ...(params.subjectRef ? { subject_ref: params.subjectRef } : {}),
  }));
  return parseMemoryPage(payload.data);
}

export async function listMemoryTargets(page = 1, pageSize = 100): Promise<MemoryTargetPage> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.targets, "POST", {
    page,
    page_size: pageSize,
  }));
  return parseMemoryTargetPage(payload.data);
}

export async function getMemory(targetRef: string, memoryRef: string): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.get, "POST", {
    target_ref: targetRef,
    memory_ref: memoryRef,
  }));
  return ensureMemoryTarget(parseMemoryItem(payload.data), targetRef);
}

export async function createMemory(input: MemoryCreateInput): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.create, "POST", {
    target_ref: input.targetRef,
    content: input.content,
    category: input.category,
    visibility: input.visibility,
    pinned: input.pinned === true,
    ...(input.attributeKey === undefined ? {} : { attribute_key: input.attributeKey }),
  }));
  return parseMemoryMutation(payload.data, input.targetRef);
}

export async function updateMemory(input: MemoryUpdateInput): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.update, "POST", {
    target_ref: input.targetRef,
    memory_ref: input.memoryRef,
    expected_version: input.expectedVersion,
    patch: {
      ...(input.patch.content === undefined ? {} : { content: input.patch.content }),
      ...(input.patch.category === undefined ? {} : { category: input.patch.category }),
      ...(input.patch.visibility === undefined ? {} : { visibility: input.patch.visibility }),
      ...(input.patch.pinned === undefined ? {} : { pinned: input.patch.pinned }),
      ...(input.patch.attributeKey === undefined ? {} : { attribute_key: input.patch.attributeKey }),
    },
  }));
  return parseMemoryMutation(payload.data, input.targetRef);
}

export async function archiveMemory(input: MemoryVersionedInput): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.archive, "POST", {
    target_ref: input.targetRef,
    memory_ref: input.memoryRef,
    expected_version: input.expectedVersion,
  }));
  return parseMemoryMutation(payload.data, input.targetRef);
}

export async function restoreMemory(input: MemoryVersionedInput): Promise<MemoryItem> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.restore, "POST", {
    target_ref: input.targetRef,
    memory_ref: input.memoryRef,
    expected_version: input.expectedVersion,
  }));
  return parseMemoryMutation(payload.data, input.targetRef);
}

export async function prepareMemoryOperation(input: {
  readonly operation: MemoryOperation;
  readonly targetRef: string;
  readonly memoryRef?: string;
  readonly expectedVersion?: number;
}): Promise<MemoryConfirmation> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.prepare, "POST", {
    operation: input.operation,
    target_ref: input.targetRef,
    ...(input.memoryRef === undefined ? {} : { memory_ref: input.memoryRef }),
    ...(input.expectedVersion === undefined ? {} : { expected_version: input.expectedVersion }),
  }));
  const confirmation = parseMemoryConfirmation(payload.data);
  if (confirmation.operation !== input.operation || confirmation.target.targetRef !== input.targetRef) {
    throw new ConsoleApiError("Memory 确认接口返回了不匹配的操作范围", "invalid_response");
  }
  return confirmation;
}

export async function commitMemoryOperation(input: {
  readonly operation: MemoryOperation;
  readonly targetRef: string;
  readonly confirmationToken: string;
}): Promise<{
  affectedCount: number;
  capabilities: MemoryOperationCapabilities;
  target: MemoryTargetView;
  deleted?: true;
  memoryRef?: string;
}> {
  const payload = record(await mutatingJson(MEMORY_ROUTES.commit, "POST", {
    operation: input.operation,
    target_ref: input.targetRef,
    confirmation_token: input.confirmationToken,
  }));
  const data = record(payload.data);
  const target = parseMemoryTarget(data.target);
  if (data.operation !== input.operation || target.targetRef !== input.targetRef) {
    throw new ConsoleApiError("Memory 提交接口返回了不匹配的操作范围", "invalid_response");
  }
  const deleted = data.deleted === undefined ? undefined : requiredBoolean(data.deleted, "deleted");
  const memoryRef = data.memory_ref === undefined ? undefined : requiredString(data.memory_ref, "memory_ref");
  if (input.operation === "delete_memory" && (deleted !== true || memoryRef === undefined)) {
    throw new ConsoleApiError("Memory 删除接口返回了无法确认的结果", "invalid_response");
  }
  return {
    affectedCount: requiredNonNegativeInteger(data.affected_count, "affected_count"),
    capabilities: parseOperationCapabilities(data.capabilities),
    target,
    ...(deleted === true ? { deleted: true } : {}),
    ...(memoryRef === undefined ? {} : { memoryRef }),
  };
}

function parseMemoryPage(value: unknown): MemoryPage {
  const data = record(value);
  return {
    items: array(data.items).map(parseMemoryItem),
    page: finiteNumber(data.page) ?? 1,
    pageSize: finiteNumber(data.page_size) ?? 20,
    total: finiteNumber(data.total) ?? 0,
    totalPages: finiteNumber(data.total_pages) ?? 0,
  };
}

function parseMemoryTargetPage(value: unknown): MemoryTargetPage {
  const data = record(value);
  return {
    items: array(data.items).map(parseMemoryTarget),
    page: finiteNumber(data.page) ?? 1,
    pageSize: finiteNumber(data.page_size) ?? 100,
    total: finiteNumber(data.total) ?? 0,
    totalPages: finiteNumber(data.total_pages) ?? 0,
  };
}

function parseMemoryTarget(value: unknown): MemoryTargetView {
  const target = record(value);
  const scope = memoryKindValue(target.scope);
  if (scope === null) {
    throw new ConsoleApiError("Memory 目标接口返回了无效范围", "invalid_response");
  }
  return {
    targetRef: requiredString(target.target_ref, "target_ref"),
    scope,
    platform: requiredString(target.platform, "platform"),
    accountRef: requiredString(target.account_ref, "account_ref"),
    groupRef: nullableString(target.group_ref),
    subjectRef: nullableString(target.subject_ref),
    capabilities: parseOperationCapabilities(target.capabilities),
  };
}

function parseMemoryItem(value: unknown): MemoryItem {
  const item = record(value);
  const kind = memoryKindValue(item.kind);
  const status = memoryStatusValue(item.status);
  const category = memoryCategoryValue(item.category);
  const visibility = memoryVisibilityValue(item.visibility);
  const sourceType = memorySourceTypeValue(item.source_type);
  if (kind === null || status === null || category === null || visibility === null || sourceType === null) {
    throw new ConsoleApiError("Memory 管理接口返回了无效范围或状态", "invalid_response");
  }
  return {
    memoryRef: requiredString(item.memory_ref, "memory_ref"),
    target: parseMemoryTarget(item.target),
    version: requiredPositiveInteger(item.version, "version"),
    content: requiredString(item.content, "content"),
    kind,
    category,
    visibility,
    status,
    pinned: requiredBoolean(item.pinned, "pinned"),
    createdAt: requiredString(item.created_at, "created_at"),
    updatedAt: nullableString(item.updated_at),
    lastConfirmedAt: nullableString(item.last_confirmed_at),
    sourceType,
    capabilities: parseMemoryCapabilities(item.capabilities),
  };
}

function parseMemoryMutation(value: unknown, targetRef: string): MemoryItem {
  return ensureMemoryTarget(parseMemoryItem(record(value).memory), targetRef);
}

function ensureMemoryTarget(item: MemoryItem, targetRef: string): MemoryItem {
  if (item.target.targetRef !== targetRef) {
    throw new ConsoleApiError("Memory 接口返回了不匹配的操作范围", "invalid_response");
  }
  return item;
}

function parseMemoryConfirmation(value: unknown): MemoryConfirmation {
  const data = record(value);
  const operation = data.operation;
  if (operation !== "clear_target" && operation !== "disable_group_profile" && operation !== "delete_memory") {
    throw new ConsoleApiError("Memory 确认操作无效", "invalid_response");
  }
  return {
    confirmationToken: requiredString(data.confirmation_token, "confirmation_token"),
    operation,
    target: parseMemoryTarget(data.target),
    affectedCount: requiredNonNegativeInteger(data.affected_count, "affected_count"),
    expiresAt: requiredFiniteNumber(data.expires_at, "expires_at"),
  };
}

function parseMemoryCapabilities(value: unknown): MemoryItem["capabilities"] {
  const data = record(value);
  return {
    canUpdate: requiredBoolean(data.can_update, "capabilities.can_update"),
    canArchive: requiredBoolean(data.can_archive, "capabilities.can_archive"),
    canRestore: requiredBoolean(data.can_restore, "capabilities.can_restore"),
    canDelete: requiredBoolean(data.can_delete, "capabilities.can_delete"),
  };
}

function parseOperationCapabilities(value: unknown): MemoryOperationCapabilities {
  const data = record(value);
  const canClearTarget = requiredBoolean(data.can_clear_target, "capabilities.can_clear_target");
  const canDisableGroupProfile = requiredBoolean(data.can_disable_group_profile, "capabilities.can_disable_group_profile");
  return { canClearTarget, canDisableGroupProfile };
}

function memoryKindValue(value: unknown): MemoryKind | null {
  return value === "personal" || value === "group_profile" || value === "group" ? value : null;
}

function memoryStatusValue(value: unknown): MemoryStatus | null {
  return value === "active" || value === "archived" ? value : null;
}

function memoryCategoryValue(value: unknown): MemoryCategory | null {
  return value === "note" || value === "preference" || value === "identity" || value === "relation" || value === "instruction" ? value : null;
}

function memoryVisibilityValue(value: unknown): MemoryVisibility | null {
  return value === "private" || value === "context_only" || value === "group_members" || value === "public" ? value : null;
}

function memorySourceTypeValue(value: unknown): MemorySourceType | null {
  return value === "user_confirmed" || value === "manual_import" || value === "system_derived" || value === "legacy" ? value : null;
}
