# Memory WebUI 身份授权与 API 边界

> 状态：当前边界，更新于 2026-07-30。Memory WebUI 和 Memory 管理 API 尚未实现；部署管理员认证、配置管理和 Todo 管理 API 已落地。

本文定义后续 Memory WebUI 的接入边界，不表示仓库已开放 Memory HTTP 路由。历史 v1 设计及当时的威胁分析保留在 [设计归档](./archive/memory-webui-auth-api-v1.md)。

## 当前已落地基线

| 能力 | 当前状态 | 实现边界 |
| --- | --- | --- |
| 部署管理员认证 | 已实现 | Bootstrap / PreAuth、初始化、登录、登出和密码重置由 `qq-maid-core/src/http/management/` 提供 |
| Web 会话与 CSRF | 已实现 | 服务端 Session、HttpOnly Cookie、同源校验、CSRF 和管理请求限流由通用认证层负责 |
| 配置与重启 | 已实现 | 控制台可读写 runtime / agent / secret 配置，并执行校验、连接测试和受控重启 |
| Todo 管理 API | 已实现 | 复用管理员 Session、同源和 CSRF；业务归属仍保留真实平台 owner / scope / delivery target |
| Memory 领域门面 | 已实现 | `qq-maid-core/src/runtime/tools/memory/` 维护 personal、group profile 和 group memory 的权限与持久化不变量 |
| Memory 管理 API / 页面 | 未实现 | 当前路由树不注册 Memory HTTP 路由，前端也没有 Memory 资源页 |
| 平台用户自助管理 | 未实现 | 尚无“浏览器会话 → 已验证平台主体”的账号绑定与群角色复验链路 |

`WEB_CONSOLE_ENABLED=false` 时，除 `/healthz` 外的控制台页面和管理路由均不注册。控制台已不是“只读页面”，但仍只应部署在 loopback 或受控内网；经反向代理时必须按配置中心文档保留原始 Host / 协议并限定可信代理 IP。

## 不可绕过的业务边界

- HTTP Handler 只负责请求解析、管理员认证、同源 / CSRF 校验、调用领域门面和安全 DTO 转换。
- Memory 的作用域、可见性、生命周期、确认来源、群画像 opt-out 和权限校验必须留在 `runtime/tools/memory/`。
- 不得从 HTTP Handler 直接暴露 `MemoryStore` CRUD，也不得用浏览器提交的 `scope_key`、`user_id`、`group_id`、昵称或角色构造授权事实。
- 部署管理员是 HTTP 调用者，不是 Memory 业务 actor。如需跨主体管理，必须增加显式 capability、安全目标引用和审计，不能把管理员 ID 写成 memory owner。
- 个人记忆、群内个人画像和群公共记忆必须分别授权；群管理员身份不能扩张为读取成员个人记忆或群内画像的权限。
- `legacy_unassigned` 不允许被普通管理流程自动认领或批量暴露。

## 后续交付顺序

### 1. 部署管理员 Memory API

第一阶段只允许现有部署管理员使用，并复用现有通用认证与资源 API 契约：

1. 管理员 Session、同源、CSRF、限流和 `x-request-id` 不另起一套实现。
2. 列表、详情、创建、编辑、归档、恢复和清空通过 Memory 领域门面实现，保留真实范围与生命周期。
3. 目标使用服务端签发的不透明引用，不向前端暴露 raw 平台 ID、稳定 scope key 或数据库内部细节。
4. 破坏性或批量操作采用服务端 prepare / commit 确认，确认令牌绑定主体、会话、目标、操作和过期时间。
5. 读取他人记忆、来源详情、批量导出和物理删除默认不进入首版。

### 2. 平台用户与群管理员自助

该阶段不能复用部署管理员账号充当最终用户身份。开放前至少需要：

- 通过可验证的平台消息完成一次性账号绑定，不接受浏览器自报 ID。
- 按 `platform + account_id + subject` 隔离身份，不因文本 ID 相同自动合并跨平台身份。
- 群公共记忆写操作使用实时或短 TTL 的 `owner/admin` 结构化事实复验；`member/unknown` 失败关闭。
- 账号解绑、群角色变化或 grant 过期后，相关会话与 capability 及时失效。

## API 与安全约定

后续 Memory API 优先复用 [管理 API 约定](../development/management-api.md) 的统一响应、错误、分页、`x-request-id` 和认证上下文。如 Memory 因作用域、审计或并发更新需要更强语义，应在通用契约上显式扩展，不得降低现有 Todo API 的安全基线。

- 错误响应不返回 SQLite 文本、绝对路径、内部 scope、raw 平台 ID 或认证细节。
- 日志和审计不记录 Memory 正文、来源原文、cookie、CSRF token、Bootstrap token 或确认令牌。
- 前端不把管理员会话、CSRF、平台 ID 或完整记忆写入 `localStorage`。
- 不得以隐藏按钮、CORS allowlist、来源 IP 或“仅内网”替代服务端认证和资源授权。

## 实现前验收清单

- 确认路由树只在显式启用的控制台内注册 Memory API，禁用时返回 404。
- 验证未登录、缺少 CSRF、跨源、越权作用域、失效目标引用和并发修改均失败关闭。
- 对 personal、group profile、group 和 `legacy_unassigned` 分别覆盖列表、详情、写入和破坏性操作的权限矩阵。
- 确认所有成功结果来自真实领域与持久化返回，不用前端文案或模型文案伪造成功。
- 对响应、日志、审计和前端状态执行敏感信息检查。
