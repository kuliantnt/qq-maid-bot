/**
 * 控制台 API 路由集中配置。
 *
 * 所有后端接口路径统一在此定义，api.ts 只引用常量；
 * 修改或新增接口路径时只需改动本文件。
 */

export const AUTH_ROUTES = {
  bootstrap: "/api/v1/console/auth/bootstrap",
  preauth: "/api/v1/console/auth/preauth",
  initialize: "/api/v1/console/auth/initialize",
  login: "/api/v1/console/auth/login",
  logout: "/api/v1/console/auth/logout",
  passwordResetBootstrap: "/api/v1/console/auth/password-reset/bootstrap",
  passwordReset: "/api/v1/console/auth/password-reset",
  session: "/api/v1/console/session",
} as const;

export const CONFIGURATION_ROUTES = {
  get: "/api/v1/console/configuration",
  runtime: "/api/v1/console/configuration/runtime",
  secrets: "/api/v1/console/configuration/secrets",
  agent: "/api/v1/console/configuration/agent",
  validate: "/api/v1/console/configuration/validate",
  testConnection: "/api/v1/console/configuration/test-connection",
} as const;

export const TODO_ROUTES = {
  create: "/api/v1/console/todo/create",
  list: "/api/v1/console/todo/list",
  targets: "/api/v1/console/todo/targets",
  get: "/api/v1/console/todo/get",
  update: "/api/v1/console/todo/update",
  delete: "/api/v1/console/todo/delete",
} as const;

export const MEMORY_ROUTES = {
  list: "/api/v1/console/memories/list",
  targets: "/api/v1/console/memories/targets",
  get: "/api/v1/console/memories/get",
  create: "/api/v1/console/memories/create",
  update: "/api/v1/console/memories/update",
  archive: "/api/v1/console/memories/archive",
  restore: "/api/v1/console/memories/restore",
  prepare: "/api/v1/console/memories/operations/prepare",
  commit: "/api/v1/console/memories/operations/commit",
} as const;

export const USER_DATA_ROUTES = {
  preferencesGet: "/api/v1/console/user-preferences/get",
  preferencesUpdate: "/api/v1/console/user-preferences/update",
  filesList: "/api/v1/console/files/list",
  filesUpload: "/api/v1/console/files/upload",
  filesDelete: "/api/v1/console/files/delete",
  filesGet: (fileId: string): string => `/api/v1/console/files/get/${fileId}`,
} as const;

export const KNOWLEDGE_ROUTES = {
  capabilities: "/api/v1/console/knowledge/files/capabilities",
  list: "/api/v1/console/knowledge/files/list",
  upload: "/api/v1/console/knowledge/files/upload",
  delete: "/api/v1/console/knowledge/files/delete",
  retry: "/api/v1/console/knowledge/files/retry",
  get: (fileId: string): string => `/api/v1/console/knowledge/files/get/${fileId}`,
} as const;

export const STATUS_ROUTE = "/api/v1/console/status";
export const RESTART_ROUTE = "/api/v1/console/restart";
export const MARKDOWN_RENDER_ROUTE = "/api/v1/markdown/render";
