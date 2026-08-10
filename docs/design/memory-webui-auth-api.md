# Memory WebUI 身份授权与 API 边界

> 状态：Issue #476 的部署管理员 Memory 管理 API 与原生 TypeScript WebUI 均已实现，更新于 2026-08-09。

本文记录 Issue #476 第一阶段的真实后端边界。历史 v1 设计及当时的威胁分析保留在[设计归档](./archive/memory-webui-auth-api-v1.md)。路由、DTO 和错误细节以 `qq-maid-core/src/http/api/memory/` 及[管理 API 约定](../development/management-api.md)为准。

## 已落地基线

| 能力 | 当前状态 | 实现边界 |
| --- | --- | --- |
| 部署管理员认证 | 已实现 | Bootstrap / PreAuth、初始化、登录、登出和密码重置由现有管理认证提供 |
| Web Session、Origin、CSRF、限流 | 已实现 | Memory API 复用现有服务端 Session、同源校验、CSRF 和管理请求限流 |
| 统一响应与请求 ID | 已实现 | 复用 `/api/v1/console/` 的成功/错误 envelope、分页和 `x-request-id` |
| 管理审计 | 已实现 | Memory 操作写入既有 `console_audit_events`，只增加安全元数据列 |
| Memory 领域门面 | 已实现 | `runtime/tools/memory/management/` 编排目标发现、DTO 所需结果、revision 和确认协议 |
| Memory 管理 API | 已实现 | 只在 `WEB_CONSOLE_ENABLED=true` 时注册，路径统一为 `/api/v1/console/memories/*` |
| 原生 TypeScript WebUI | 已实现 | `web-console/` 提供受控 Memory 列表、筛选、创建、编辑、归档/恢复、永久删除和范围确认操作，`dist/` 由构建生成 |
| 平台用户/群管理员自助 | 未实现 | 没有把部署管理员身份转换为平台用户或群角色的绑定链路 |

## 部署管理员边界

第一阶段的管理能力是实例级部署管理员 capability。管理员是 HTTP 调用者和审计 actor，不是 Memory owner；所有管理员看到同一份实例级可管理范围，但不会因此成为任何平台用户、群组、机器人账号或 scope。

Memory 创建使用 `ManualImport` 来源，`created_by_user_id` 保持为空；编辑只保留原记录的 owner/source 元数据，不允许管理员通过请求伪造这些字段。管理审计单独记录管理员 actor，不把管理员 ID 写入 Memory owner。

支持的范围只有：

- `personal`；
- `group_profile`；
- `group`。

`legacy_unassigned` 不进入 target discovery、list、detail 或任何写操作。未知、缺失或不能可信解析的旧 scope 统一 fail closed，不通过错误差异、ID 探测或搜索泄露其存在。

## Opaque target/reference

target discovery 从已存在且可可信解析的 v3 Memory 记录中恢复稳定的 platform/account/group/subject 关系，然后生成带版本前缀的 opaque reference：

- `memory_target:v1:<digest>`；
- `memory_account:v1:<digest>`；
- `memory_group:v1:<digest>`；
- `memory_subject:v1:<digest>`；
- `memory:v1:<digest>`。

摘要只返回 scope 名称、平台名、opaque ref 和目标级 capabilities，不返回 scope key、account/group/user ID、owner ID、关系 subject raw ID 或内部 row key。`can_disable_group_profile` 由服务端持久化的群画像 preference 计算，客户端重新登录或刷新 target 列表后仍能得到真实状态。服务器每次使用 target ref 都重新回查当前候选目标；客户端持有旧 ref 不会跳过当前合法性检查。memory ref 同时绑定 target ref 和记录 ID，因此 target mismatch、未知 ID、目标外 ID 和 legacy probing 都统一安全失败。

## API 与生命周期

第一阶段真实路由为：

| 路径 | 领域动作 |
| --- | --- |
| `POST /api/v1/console/memories/targets` | 分页发现可管理 target，返回目标级 capabilities，并支持 scope/platform/account_ref/group_ref/subject_ref 筛选 |
| `POST /api/v1/console/memories/list` | 按 target、结构化字段和正文 keyword 分页查询 |
| `POST /api/v1/console/memories/get` | 按 target_ref + memory_ref 读取安全详情 |
| `POST /api/v1/console/memories/create` | 对重新回查成功的 target 创建人工导入 |
| `POST /api/v1/console/memories/update` | 携带 expected_version 编辑，保留旧记录并写入新记录 |
| `POST /api/v1/console/memories/archive` | 携带 expected_version 原子归档 |
| `POST /api/v1/console/memories/restore` | 携带 expected_version 原子恢复 |
| `POST /api/v1/console/memories/operations/prepare` | 准备 `clear_target` / `disable_group_profile` / `delete_memory`；单条删除同时携带 opaque `memory_ref` 与 `expected_version` |
| `POST /api/v1/console/memories/operations/commit` | 提交一次性确认 |

编辑沿用当前领域的历史语义：旧记录变为 archived，新记录获得新 ID；不会原地覆盖正文，也不能修改 target、owner、source 或创建者。永久删除通过 `delete_memory` 的服务端 prepare/commit 双阶段确认，只接受 active Memory；prepare 将 opaque `memory_ref` 与完整 revision 快照绑定到 session-bound 一次性 token，commit 在事务内再次校验后物理删除。服务端只返回 opaque memory_ref，不暴露内部记录 ID。

`clear_target` 的语义是事务内把目标范围当前 active Memory 全部归档，历史保留且可恢复；它不是 DELETE。`disable_group_profile` 复用群画像 opt-out 生命周期：在同一事务写入 profile preference=false，并归档当前 active 画像。第一阶段没有重新启用画像的管理路由；历史 archived 记录不会被自动恢复。

## revision 与并发

`memory_management_schema_v5_revision` 为旧记录和新记录增加 `revision INTEGER NOT NULL DEFAULT 1`。revision 由服务端维护，不接受客户端指定。更新、归档、恢复、永久删除和批量操作都在 SQLite `IMMEDIATE` transaction 中比较完整记录或 `(id, revision)` 快照；相同 expected version 的并发请求最多一个提交，另一个返回 `conflict`，事务失败不会留下部分结果。永久删除在 CAS 成功后物理移除 active 记录，不能恢复。

## prepare / commit

`clear_target` 和 `disable_group_profile` 必须先 prepare。确认条目绑定部署管理员 ID、当前管理 Session 摘要、operation、target、active `(id, revision)` 快照、画像开关快照和 5 分钟 TTL。响应只返回随机 confirmation token 原文一次；服务端只保存 token 的 SHA-256 摘要及上述绑定信息，不保存 token 原文。

commit 会重新校验当前 Session、管理员、Origin、CSRF、TTL、token、operation、target 和当前 target 合法性，再由领域 storage 事务比较快照。成功前 token 在互斥锁内消费，因此不能 replay；跨 actor、跨 Session、错误 operation、目标变化、revision 变化或过期都会 fail closed。CSRF 只由每次 HTTP 请求的现有认证流程校验，不写入 confirmation 状态，因此合法的 CSRF 轮换不会无故使确认失效。确认状态是进程内最小状态；进程重启会丢弃未提交确认并安全失败，不新增第二套数据库确认表。

## 搜索、分页与 DTO

Memory keyword 首版使用 SQLite 参数化 `LIKE`，只匹配 `content`。keyword trim 后为空视为未设置，长度上限为 256 个字符；反斜杠、`%`、`_` 会按字面子串转义，并使用明确的 `ESCAPE` 规则。COUNT 和 page query 共用完全相同的 WHERE 条件，不读取后在内存分页，也不搜索 source_text、source_ref、scope 或 raw identity。

成功的 list/get/mutation DTO 可以返回管理界面需要的正文、kind/category、visibility、status、pinned、时间、safe source type、version 和 capabilities。任何错误、not_found、conflict、permission、日志、审计和 confirmation 状态都不包含正文、source detail、raw identity、scope key 或 token。

## 后续平台用户自助管理

第二阶段或后续平台用户能力不能复用部署管理员账号充当最终用户身份。开放前至少需要可验证的平台消息绑定、`platform + account + subject` 隔离、实时群角色复验和 grant 失效处理；这些能力不属于本阶段。

`WEB_CONSOLE_ENABLED=false` 时 Memory 路由不注册并返回 404；setup-required 或 Memory domain 未装配时不匿名降级，已认证但服务缺失返回 `memory_unavailable`。旧 `/memory`、`/query`、`/v1/chat` 路由仍不存在。
