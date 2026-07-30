# 管理 API 约定

> 当前状态：本文记录已落地的部署管理员资源 API 通用约定，以及 Todo 资源的现行契约。路由和 DTO 仍以 `qq-maid-core/src/http/api/` 源码为准。

本文定义 Web 控制台资源管理接口的公共约定。当前首个资源是 Todo；后续 Memory、知识库等接口复用鉴权、分页、响应和错误基础设施，但必须保留各自的领域权限与数据归属模型。

## 路由与公共鉴权

资源管理路由仅在 `WEB_CONSOLE_ENABLED=true` 时注册，统一使用 `/api/v1/console/` 前缀和 `POST`。调用者必须先通过部署管理员登录取得服务端 Session cookie，并携带同一会话签发的 `x-csrf-token`；浏览器请求还必须通过现有同源校验和管理请求限流。

`AuthenticatedApiActor` 表示本次操作的管理员调用者，只用于认证、CSRF、Origin、限流、审计和诊断。它不是 Todo owner，不能用于构造 `owner_key` 或 `scope_key`。所有已认证部署管理员当前都具有同一份全局 Todo 管理权限。

客户端可以传入不超过 128 字符且仅包含字母、数字、`-`、`_`、`.`、`:` 的 `x-request-id`；缺失或非法时由服务端生成 UUID。进入 API Handler 后的成功和错误响应都会回传该 Header，并在 JSON 中包含 `request_id`。

## Todo 的真实归属

管理 API 与 QQ、OneBot、微信聊天入口操作同一张 `todos` 表、同一批记录。Todo 继续保存聊天入口生成的真实 `owner_key`、`user_id` 和 conversation `scope_key`，平台、账号、私聊/群聊目标也继续遵循现有稳定 scope 语义。

管理 API 不创建 `console_admin:*` owner，也不使用 `management:console_admin:*` scope。普通聊天 Tool、Slash 命令和 Todo Store 的 owner-scoped 查询入口保持原有校验，不能调用管理专用全局查询。

## 统一响应

成功响应：

```json
{
  "ok": true,
  "data": {},
  "request_id": "2b46a6ac-6b68-4743-b3f1-f980f28a11e0"
}
```

错误响应：

```json
{
  "ok": false,
  "error": {
    "code": "validation_error",
    "message": "page must be greater than or equal to 1"
  },
  "request_id": "2b46a6ac-6b68-4743-b3f1-f980f28a11e0"
}
```

公共错误映射：

| 场景 | HTTP 状态 | `error.code` |
| --- | ---: | --- |
| JSON 无法解析或缺少必填字段 | 400 | `invalid_json` |
| ID、目标引用、筛选或字段校验失败 | 422 | `validation_error` |
| 未登录或会话失效 | 401 | `unauthenticated` |
| Origin、CSRF 或领域权限拒绝 | 403 | 对应安全错误码 / `permission_denied` |
| Todo 不存在 | 404 | `not_found` |
| 状态或并发版本冲突 | 409 | `conflict` |
| 领域服务未装配 | 503 | `<domain>_unavailable` |
| 数据库、Outbox 或其他内部失败 | 500 | `internal_error` |

## 公共分页

列表请求在顶层复用以下字段：

```json
{
  "page": 1,
  "page_size": 20
}
```

- `page` 默认 `1`，从 `1` 开始。
- `page_size` 默认 `20`，范围为 `1..=100`。
- 页码乘法和数据库 offset 在公共分页层做溢出校验。
- 页码超出总页数时返回空 `items`，仍返回真实 `total` 和 `total_pages`。
- `total=0` 时 `total_pages=0`；其余情况按向上取整计算。

SQLite 使用同一组筛选分别执行 `COUNT(*)` 与带 `LIMIT/OFFSET` 的当前页查询，不会全量加载后在内存切片。排序沿用 Todo 的状态/计划时间/完成时间顺序，并以内部 Todo ID 作为最终稳定次级排序。

## Todo API

六个接口均为 `POST`：

| 路径 | 用途 |
| --- | --- |
| `/api/v1/console/todo/create` | 向经过验证的真实平台目标创建 Todo |
| `/api/v1/console/todo/list` | 全局分页和组合筛选 |
| `/api/v1/console/todo/targets` | 分页发现可创建 Todo 的真实会话目标 |
| `/api/v1/console/todo/get` | 全局按 ID 查询单项 |
| `/api/v1/console/todo/update` | 全局部分更新 / 状态转换 |
| `/api/v1/console/todo/delete` | 按现有物理删除语义删除 |

Todo ID 在响应中使用十进制字符串。输入兼容正整数 JSON number 或不带前导零的正整数十进制字符串；`0`、`"0"`、`"000"`、负数、非数字及超出 SQLite `INTEGER` 正数范围的值返回 422，不伪装成 404。

### 目标信息与 `target_ref`

列表、详情、创建和更新响应都包含整理后的 `target`：

```json
{
  "target": {
    "target_ref": "todo_target:v1:...",
    "platform": "qq_official",
    "scope_type": "private",
    "user_id": "...",
    "group_id": null,
    "account_id": "...",
    "reminder_supported": true
  }
}
```

- `target_ref` 是服务端基于真实 owner/scope 生成的版本化稳定引用；客户端应把它视为不透明字符串，不应自行拼接或解析内部 key。
- 创建时服务端会把引用回查到已有 Todo 目标或已知 Session 目标，并再次校验 owner、conversation scope、平台、账号和目标类型。请求体不能直接提交 `owner_key`、`scope_key`、`group_id` 或 `account_id` 来绕过校验。
- 对无法完整解析的旧 scope，单项会降级为 `scope_type="unknown"`、`target_ref=null`，并返回受控 `diagnostic`；单条异常记录不会使整个列表失败。
- `/api/v1/console/todo/targets` 从已有 Todo 与 Session 的服务端可信身份字段中分页发现目标，因此即使会话从未创建过 Todo，控制台也能先取得 `target_ref` 再创建第一条记录。无法完整恢复真实 owner、成员或 conversation scope 的旧记录不会进入创建目标列表。
- 目标发现支持 `platform`、`account_id`、`scope_type`、`user_id` 和 `group_id` 精确筛选；响应只包含 `target_ref`、平台/账号、作用域类型、用户/群和 `reminder_supported`，不返回 owner/scope 内部 key。

### 创建

```json
{
  "target_ref": "todo_target:v1:...",
  "title": "准备周报",
  "detail": "整理本周进度",
  "due_date": "2099-08-01",
  "due_at": null,
  "reminder_at": "2099-08-01T09:00:00+08:00",
  "time_precision": "date_time",
  "recurrence_kind": "none",
  "recurrence_interval_days": 0,
  "recurrence_interval": 0,
  "recurrence_unit": "day"
}
```

`target_ref` 和 `title` 必填。管理 API 不执行自然语言时间推断；日期、时间、重复规则和敏感文本处理继续复用 Todo 领域的草稿归一与校验。创建请求只要包含非空 `reminder_at`，就必须同时满足“晚于当前时间”和目标平台支持主动提醒，否则返回 422。

创建 Todo 与创建对应 reminder Outbox 记录在同一个 SQLite 事务中提交。目标无效、提醒时间无效、Outbox 写入失败时不会留下 Todo。

### 全局列表与筛选

```json
{
  "page": 1,
  "page_size": 20,
  "status": "pending",
  "due_date": null,
  "date_start": "2099-08-01",
  "date_end": "2099-08-31",
  "time_filter": null,
  "keyword": "周报 项目",
  "recurring": false,
  "platform": "onebot",
  "account_id": "bot-1",
  "scope_type": "group",
  "user_id": null,
  "target_ref": null
}
```

Todo 筛选：

- `status`：`pending`、`completed`、`all`；默认 `all`。
- `due_date`：按单日筛选，与日期范围互斥。
- `date_start` / `date_end`：必须同时传入；`completed` 状态筛完成时间，其余状态筛计划时间。
- `time_filter`：`overdue` 或 `no_due_date`，与普通日期条件互斥。
- `keyword`：标题、详情和原文的多词 AND 模糊匹配。
- `recurring`：`true` 仅周期项，`false` 仅一次性项，缺失或 `null` 不限制。

管理目标筛选：

- `platform`：使用稳定 scope 中的实际平台名，例如 `qq_official`、`onebot`、`wechat_service`；QQ 历史 scope 会归入 `qq_official`。
- `account_id`：按稳定平台账号维度筛选。
- `scope_type`：`private` 或 `group`。
- `user_id`：按 Todo 实际归属成员精确筛选。
- `target_ref`：按服务端已验证的完整 owner/scope 精确筛选。

这些筛选只存在于管理专用全局查询；普通聊天入口仍必须传入当前真实 owner/scope。

### 单项、更新和删除

单项和删除请求：

```json
{
  "id": "123"
}
```

更新使用 Todo 专属部分更新 DTO：

```json
{
  "id": "123",
  "title": "新的标题",
  "detail": null,
  "due_date": null,
  "reminder_at": "2099-08-02T09:00:00+08:00",
  "status": "completed"
}
```

- 管理 Service 先按 ID 全局读取记录，再使用记录自身的真实 owner/scope 执行写入。
- 未传字段保持原值；`detail`、`due_date`、`due_at`、`reminder_at` 传 `null` 表示显式清空，`title=null` 非法。
- 更新时只有本次请求显式设置的非空 `reminder_at` 才重新校验未来时间和平台能力；显式清空会取消旧未终结 Outbox。未修改提醒字段时允许保留历史值，已经过去的提醒不会重新创建 Outbox，也不会阻止标题、详情或日期更新。
- 完成 Todo 会取消旧未终结提醒，不因保留的历史 `reminder_at` 已过期而失败；恢复为 `pending` 时沿用领域 Outbox 语义，过期值保留但不重新排程。
- 已完成 Todo 的普通字段不能直接修改；同一请求显式恢复为 `pending` 后可以修改。
- 一次性 Todo 完成后进入 `completed`；周期 Todo 沿用聊天侧“完成本次并推进下一周期”的规则。
- 删除继续使用物理删除语义，并在同一事务内取消尚未终结的 reminder Outbox。

组合更新在一个 SQLite 事务中完成全局重读、并发快照校验、字段归一、恢复、字段写入、完成/周期推进、旧提醒取消和新提醒写入。能够提前完成的字段、时间和周期计算在写入前完成；任一步失败都会回滚 Todo 和 Outbox，不使用失败后手动改回的补偿逻辑。

## 提醒与平台限制

管理 API 不直接调用平台发送接口，也不新增 Web 专用调度器。带提醒的 Todo 继续走：

```text
真实 TodoOwner / conversation scope
→ Todo 与 Notification Outbox 同库事务
→ Notification Worker
→ 现有 Push Sink
→ QQ 官方 / OneBot 11 目标
```

- QQ 官方私聊、群聊和 OneBot 11 私聊、群聊使用现有主动推送能力；群成员 Todo 继续携带真实 owner 成员 mention。
- 微信服务号当前不在统一主动 Push Sink 中。微信 Todo 可以通过管理 API 查询、修改或创建，但只要最终 pending Todo 带 `reminder_at`，请求会在写库前返回 422，不创建不可投递提醒。
- 更新提醒时间会取消旧 pending/retry/sending/failed 任务并写入新的去重事件；已发送历史保留。
- 完成一次性 Todo 或删除 Todo 会取消旧提醒；完成周期 Todo 会推进时间并写入下一周期提醒。

## 后续资源模块

Memory 或知识库 API 可以直接复用 `http/api/common/` 中的：

- `ApiRequestContext` / 管理员身份认证；
- `PaginationRequest` / `ValidatedPagination` / `PagedResponse<T>`；
- 成功与错误 envelope、JSON rejection 和请求 ID；
- 管理员认证错误到 HTTP/API 错误的映射。

它们不能照搬 Todo 的 owner 模型。新增资源应在 `http/api/<domain>/` 定义自己的 DTO、Handler、领域 Service、权限和错误映射，并调用 `runtime/tools/<domain>/` 的明确门面；不得增加通用 CRUD Service、万能 Patch Map 或任意表 Repository。
