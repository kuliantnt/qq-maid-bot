# API Contracts

## Client boundary

组件不能直接调用 `fetch`。API 调用集中在 `src/api.ts` 或按领域拆分的 API module，响应在边界解析成 `src/types.ts` 的类型。请求使用 `credentials: "same-origin"`。改变状态的请求携带 `X-CSRF-Token`，CSRF token 不进入 localStorage。

## Current endpoints

| Method | Path | 用途 | 认证 |
|---|---|---|---|
| GET | `/api/v1/console/session` | 刷新管理员会话和 CSRF | admin session |
| GET | `/api/v1/console/auth/bootstrap` | 读取初始化状态 | 匿名、同源、限流 |
| POST | `/api/v1/console/auth/preauth` | 创建 pre-auth 和 CSRF | 同源、限流 |
| POST | `/api/v1/console/auth/initialize` | 创建首位管理员 | pre-auth + CSRF |
| POST | `/api/v1/console/auth/login` | 管理员登录 | pre-auth + CSRF |
| POST | `/api/v1/console/auth/password-reset/bootstrap` | 申请重置流程 | pre-auth + CSRF |
| POST | `/api/v1/console/auth/password-reset` | 重置密码 | pre-auth + CSRF |
| POST | `/api/v1/console/auth/logout` | 注销 | admin + CSRF |
| GET | `/api/v1/console/status` | 运行、平台、能力、存储摘要 | 只读 |
| GET | `/api/v1/console/configuration` | 读取配置快照和工具摘要 | admin |
| PATCH | `/api/v1/console/configuration/runtime` | 保存普通配置 | admin + CSRF |
| PATCH | `/api/v1/console/configuration/secrets` | 替换或清除 secret | admin + CSRF |
| PATCH | `/api/v1/console/configuration/agent` | 保存 Agent 策略 | admin + CSRF |
| POST | `/api/v1/console/configuration/validate` | 本地配置预检 | admin + CSRF |
| POST | `/api/v1/console/restart` | 提交受控重启 | admin + CSRF |
| POST | `/api/v1/markdown/render` | 服务端 Markdown 安全预览 | 只读 |

`status` 页面数据是安全摘要，不包含 token、secret、API key、cookie、authorization 或绝对敏感路径。配置 snapshot 对 secret 只返回配置状态，不返回原文。配置写入使用 revision，发生冲突时必须刷新并由用户重新确认，不得覆盖未知修改。

配置交互的完整状态协议见 `docs/INTERACTION_CONTRACTS.md`。特别约束：runtime 和 agent 使用 expected revision；secret 每项 replace/clear 使用自己的 expected revision；空 secret 不产生请求；409 或 `config_conflict` 不得自动重试；成功必须以服务端返回的 snapshot 为准。

## Errors

客户端统一使用 `ConsoleApiError`，保留 `status`、`code` 和安全的 `message`。页面必须提供 loading、empty 和 error 状态。不能把原始响应、secret、内部路径或 raw ID直接显示给用户。

`validate` 只做本地预检，不保存也不联网。`restart` 只表示受控重启请求已接受，页面必须在服务恢复后刷新 status，不能把 accepted 当作 completed。

## Future slots

Memory、日志、消息调试和附件管理目前没有可消费的后端 endpoint。它们只能作为页面 registry 的未来元数据，不得注册 fetch、静态假数据或假的成功状态。Memory 还需要身份绑定、权限、版本、审计和脱敏 DTO，见仓库 `docs/design/memory-webui-auth-api.md`。

## Adding an API consumer

1. 确认 Rust endpoint 已存在并有稳定 DTO。
2. 在 `types.ts` 定义响应类型。
3. 在 API module 中使用现有 transport 和 parser。
4. 页面 controller 调用 API，组件只接收已解析数据。
5. 增加成功、空结果、网络失败和权限失败测试。
6. 如果新增静态 JS/CSS，更新 `qq-maid-core/src/http/console_routes.rs` 的 `CONSOLE_ASSETS`。
