# 管理 API 约定

本文定义 Web 控制台资源管理接口的公共约定。当前首个资源是 Todo；后续 Memory、知识库等接口复用同一套鉴权、分页、响应和错误基础设施，但必须保留各自的领域 Service、权限和校验逻辑。

## 路由与鉴权

资源管理路由仅在 `WEB_CONSOLE_ENABLED=true` 时注册，统一使用 `/api/v1/console/` 前缀和 `POST`。调用者必须先通过现有部署管理员登录接口取得服务端 Session cookie，并在请求中同时携带登录会话签发的 `x-csrf-token`。浏览器请求还必须通过现有同源校验。

业务请求体不得提供或覆盖 `user_id`、`creator_id`、`owner_id`、`operator_id`、`account_id` 或 `scope_key`。服务端从管理员 Session 生成稳定 API subject，领域 Service 再根据该 subject 判断资源权限。

当前仓库没有“部署管理员与 QQ / OneBot / 微信用户”的可信身份绑定，因此 Todo 管理 API 使用独立的管理员管理作用域，不读取或修改聊天入口 Todo，也不允许通过请求体选择聊天用户。未来若要跨入口管理用户数据，必须先增加显式授权/绑定模型，不能把调用者提交的用户 ID 当作授权事实。

客户端可以传入不超过 128 字符且仅包含字母、数字、`-`、`_`、`.`、`:` 的 `x-request-id`；缺失或非法时由服务端生成 UUID。进入 API Handler 后的成功和错误响应都会回传该 Header，并在 JSON 中包含 `request_id`。

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
| 参数或字段校验失败 | 422 | `validation_error` |
| 未登录或会话失效 | 401 | `unauthenticated` |
| Origin、CSRF 或领域权限拒绝 | 403 | 对应安全错误码 / `permission_denied` |
| 资源不存在或按隐藏策略不可见 | 404 | `not_found` |
| 状态冲突 | 409 | `conflict` |
| 领域服务未装配 | 503 | `<domain>_unavailable` |
| 数据库、通知或其他内部失败 | 500 | `internal_error` |

领域错误先在对应 API adapter 映射为上述 API 错误；领域 Service 不依赖 HTTP 状态码。

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

分页数据结构位于成功响应的 `data`：

```json
{
  "items": [],
  "page": 1,
  "page_size": 20,
  "total": 0,
  "total_pages": 0
}
```

`total=0` 时 `total_pages=0`；其余情况按向上取整计算且不使用可能溢出的 `total + page_size - 1`。

## Todo API

五个接口均为 `POST`：

| 路径 | 用途 |
| --- | --- |
| `/api/v1/console/todo/create` | 创建 Todo |
| `/api/v1/console/todo/list` | 分页和组合筛选 |
| `/api/v1/console/todo/get` | 查询单项 |
| `/api/v1/console/todo/update` | 部分更新 / 状态转换 |
| `/api/v1/console/todo/delete` | 按现有物理删除语义删除 |

Todo ID 在响应中使用十进制字符串，输入兼容正整数 JSON number 或十进制字符串。响应 DTO 不包含内部 `user_id`、`scope_key` 和自然语言原文。

### 创建

```json
{
  "title": "准备周报",
  "detail": "整理本周进度",
  "due_date": "2099-08-01",
  "due_at": null,
  "recurrence_kind": "none",
  "recurrence_interval_days": 0,
  "recurrence_interval": 0,
  "recurrence_unit": "day"
}
```

仅 `title` 必填且不得为空。日期、时间和重复规则继续由 Todo 领域草稿归一与校验处理；管理 API 不执行 Todo 自然语言时间推断。当前管理身份没有平台投递目标，因此请求 DTO 不开放 `reminder_at` 写入；返回 DTO 保留该字段以兼容 Todo 完整模型。

### 列表与现有筛选

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
  "recurring": false
}
```

筛选直接映射到 Todo 现有 `TodoQuery`：

- `status`：`pending`、`completed`、`all`；默认 `all`。
- `due_date`：按单日筛选，与日期范围互斥。
- `date_start` / `date_end`：必须同时传入；`completed` 状态筛完成时间，其余状态筛计划时间。
- `time_filter`：`overdue` 或 `no_due_date`，与普通日期条件互斥；未显式传 `status` 的 `overdue` 自动使用 `pending`，显式组合其他状态会报参数错误。
- `keyword`：复用标题、详情和原文的多词 AND 模糊匹配。
- `recurring`：`true` 仅周期项，`false` 仅一次性项，缺失或 `null` 不限制。

`COUNT(*)` 与当前页查询使用同一个 Todo owner/scope 和同一组筛选条件；分页由 SQLite `LIMIT/OFFSET` 执行，不会先全量加载。排序复用 Todo 现有稳定状态/时间/ID 排序规则。

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
  "status": "completed"
}
```

- 未传字段保持原值。
- `detail`、`due_date`、`due_at` 传 `null` 表示显式清空；`title=null` 非法。
- ID、owner 和 scope 不在更新 DTO 中，不能修改。
- `status` 使用 Todo 现有完成/恢复规则；周期 Todo 的“完成”仍按现有语义推进下一周期。
- 已完成 Todo 的普通字段不能直接修改，必须在同一请求或前一请求中恢复为 `pending`。
- 删除复用 Todo 当前物理删除及提醒取消链路；不存在和不可见资源统一返回 404。

## 后续资源模块

Memory 或知识库 API 可以直接复用 `http/api/common/` 中的：

- `ApiRequestContext` / 管理员身份认证；
- `PaginationRequest` / `ValidatedPagination` / `PagedResponse<T>`；
- 成功与错误 envelope、JSON rejection 和请求 ID；
- 管理员认证错误到 HTTP/API 错误的映射。

新增资源应在 `http/api/<domain>/` 定义自己的 DTO、Handler 和领域错误映射，并调用 `runtime/tools/<domain>/` 的明确门面。不得增加通用 CRUD Service、万能 Patch Map 或任意表 Repository。
