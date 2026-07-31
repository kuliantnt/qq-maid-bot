# 控制台用户偏好与通用文件 API

> 本文是独立前端与后端约定的接口契约。公共认证、请求 ID、响应包络、分页和错误语义见[管理 API 约定](./management-api.md)，真实行为仍以 `qq-maid-core/src/http/api/user_data/` 为准。

## 公共调用要求

六个接口一律使用 `POST`，仅在 `WEB_CONSOLE_ENABLED=true` 时注册。请求必须携带当前部署管理员的 Session cookie 和同一会话签发的 `x-csrf-token`，并通过现有 Origin 校验与管理请求限流。独立前端应通过同源部署或反向代理访问这些路径；配置普通跨域 allowlist 不会放宽管理 API 的凭据边界。

请求和响应中都没有 `user_id` 或 `admin_id`。后端只使用认证会话得到的当前管理员 ID，不能查询或修改其他用户的偏好与文件。除文件内容读取外，成功结果统一位于：

```json
{
  "ok": true,
  "data": {},
  "request_id": "2b46a6ac-6b68-4743-b3f1-f980f28a11e0"
}
```

## 用户偏好

### 读取

`POST /api/v1/console/user-preferences/get`

请求体必须是空 JSON 对象：

```json
{}
```

没有偏好记录时，`data` 直接返回完整默认值：

```json
{
  "custom_colors": [],
  "background_file_ids": [],
  "active_background_file_id": null,
  "kuliantnt": false
}
```

### 部分更新

`POST /api/v1/console/user-preferences/update`

请求只改变出现的字段。例如：

```json
{
  "kuliantnt": true
}
```

也可以整体替换颜色和背景列表，并同时切换当前背景：

```json
{
  "custom_colors": ["#FF6699", "#8B5CF6"],
  "background_file_ids": [
    "2d637334-11ea-48ea-88ba-1ac31e9a5651",
    "a919885c-1208-4b18-a76f-d54764789b9a"
  ],
  "active_background_file_id": "2d637334-11ea-48ea-88ba-1ac31e9a5651"
}
```

- `custom_colors` 最多 32 项，每项最多 64 个字符；仅做字符串、数量和长度限制，不解析颜色格式，保存时保持原顺序。
- `background_file_ids` 最多 64 项，不能重复；每项必须是当前用户已经上传的服务端规范 UUID，保存时保持原顺序。
- `active_background_file_id` 非空时必须在最终的 `background_file_ids` 中；传 `null` 表示恢复默认背景。
- 整体替换背景列表且移除了原当前背景、又未显式提交新的当前背景时，后端自动把当前背景清空。
- `kuliantnt` 是普通布尔字段。
- 列表字段和 `kuliantnt` 不能传 `null`；省略表示不修改。未知字段返回 400。

成功时 `data` 返回更新后的完整偏好。

## 通用文件

文件接口不包含背景专用规则，可由头像、Logo 等后续场景复用。单文件上限为 10 MiB，暂不限制文件格式。

### 上传

`POST /api/v1/console/files/upload`

请求使用 `multipart/form-data`，必须且只能包含一个名为 `file` 的文件字段。服务端忽略任何客户端路径语义，原始文件名只保存为元数据。

约 11 MiB 的请求 Body Limit（10 MiB 文件加 multipart 开销）只挂在该上传路由。偏好读取、偏好
更新、文件列表和文件删除等 JSON 路由保持 64 KiB 上限，不继承上传额度。

成功响应的 `data`：

```json
{
  "file_id": "2d637334-11ea-48ea-88ba-1ac31e9a5651",
  "filename": "background.webp",
  "content_type": "image/webp",
  "size": 123456,
  "created_at": "2026-07-31T12:00:00Z",
  "url": "/api/v1/console/files/get/2d637334-11ea-48ea-88ba-1ac31e9a5651"
}
```

`file_id` 与实际磁盘文件名都由服务端分别生成；磁盘文件不使用原始文件名。缺少文件 Content-Type 时保存为 `application/octet-stream`。

### 列表

`POST /api/v1/console/files/list`

请求复用公共分页，`{}` 表示第一页默认大小：

```json
{
  "page": 1,
  "page_size": 20
}
```

响应 `data.items` 中每项与上传响应字段一致，并包含 `created_at` 和读取 `url`。只返回当前用户上传的文件，按创建时间倒序排列。

### 读取内容

`POST /api/v1/console/files/get/{file_id}`

请求没有 JSON body，但仍必须携带 Session cookie 和 `x-csrf-token`。成功时直接返回文件字节，不使用 JSON 成功包络，并设置：

- 原上传元数据中的 `Content-Type`；
- 与文件字节一致的 `Content-Length`；
- `Cache-Control: private, no-store`；
- 公共 `x-request-id` 和控制台安全响应头。

该端点复用管理员 Session、同源与 CSRF 校验，并继续按管理员 ID 检查文件归属；它使用独立的
只读认证路径，不消耗每分钟 60 次的配置修改、文件删除、Todo 等管理动作额度。接口不公开文件
系统路径，也没有取消访问控制。

由于接口统一使用 `POST`，`url` 不能直接作为 `<img src>`。前端应先以带凭据和 CSRF 的 `POST` 获取 Blob，再创建页面生命周期内的 object URL，例如：

```javascript
const response = await fetch(file.url, {
  method: "POST",
  credentials: "same-origin",
  headers: { "x-csrf-token": csrfToken },
});
const objectUrl = URL.createObjectURL(await response.blob());
```

文件 ID 非规范 UUID 返回 422；记录不存在或属于其他用户统一返回 404，不泄露资源是否属于其他用户。后端只按数据库中经过验证的服务端文件名读取，不接受文件名或任意路径。

### 删除

`POST /api/v1/console/files/delete`

请求：

```json
{
  "file_id": "2d637334-11ea-48ea-88ba-1ac31e9a5651"
}
```

成功响应的 `data`：

```json
{
  "file_id": "2d637334-11ea-48ea-88ba-1ac31e9a5651",
  "deleted": true
}
```

删除时先把磁盘文件原子改名为不可访问的暂存名，再在同一个 SQLite 事务内从当前用户的 `background_file_ids` 移除该 ID、按需清空 `active_background_file_id` 并删除文件记录。事务失败时尝试恢复磁盘文件；事务提交后清理暂存文件，清理失败会记录明确警告，不会伪报磁盘清理已经完成。

## 持久化位置

偏好与文件元数据写入 `APP_DB_FILE`：

- `console_user_preferences`：每个 `console_admins.id` 最多一行；
- `console_user_files`：按管理员 ID 保存文件 ID、原始文件名、Content-Type、大小、服务端文件名和创建时间。

文件内容保存在 `APP_DB_FILE` 父目录下的 `console-files/`。例如默认数据库是 `data/storage/app.db` 时，文件目录是 `data/storage/console-files/`。文件 ID 与磁盘文件名均使用服务端生成的 UUID；磁盘文件名追加 `.blob`，不保存 Base64，也不允许客户端指定服务器路径。
