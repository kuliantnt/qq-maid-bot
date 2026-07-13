# 通过 OneBot 11 反向 WebSocket 接入 NapCat

本文说明如何让 [NapCat](https://napneko.github.io/) 以**反向 WebSocket 客户端**身份连到本项目的 OneBot 11 监听端，让小女仆通过 NapCat 走 QQ 私聊和群聊。

本文只覆盖“接入与配置”。OneBot 11 入口的实现边界、平台 ID 与业务隔离键设计见：

- 架构与开发边界：[../DEVELOPMENT.md](../DEVELOPMENT.md)
- Gateway 职责：[`qq-maid-gateway-rs/README.md`](../../qq-maid-gateway-rs/README.md)
- 配置项权威清单：[`runtime/config/.env.example`](../../runtime/config/.env.example)

## 1. 一期能力边界

在动手前先明确当前 OneBot 11 入口**只**提供的能力，避免配置预期之外的功能：

| 维度 | 支持 |
| --- | --- |
| 连接方式 | NapCat / Lagrange.OneBot 等以反向 WebSocket 客户端连接；一期不提供正向 WS 服务端 |
| 账号数量 | 单账号。首个上报的 `self_id` 会锁定本进程，重连换账号会被拒绝 |
| 私聊 | ✅ 文本进入 Core，文本回复通过 `send_private_msg` 发回 |
| 群聊 | ✅ 仅当消息明确 `@` 当前机器人时进入 Core；回复通过 `send_group_msg` 发到原群 |
| 出站内容 | 仅 `text` 消息段。一期**不**发送回复引用、`@`、图片等出站段 |
| 入站消息段 | 一期只解析文本与 `at` 触发语义；`reply`/`image`/`face` 等段不解析为正文，但不会让整条事件解析失败 |
| 平台流式 | 不向 OneBot 平台流式发送；Core 内部的流式回复合并为普通文本一次性发出 |
| 主动推送 | ✅ 可向 OneBot 私聊 / 群聊目标推送 Todo 每日提醒、RSS 等 |

## 2. 前置条件

- 已经按 [runtime/README.md](../../runtime/README.md) 完成 release 构建，`runtime/qq-maid-bot` 可执行。
- 已经在 `runtime/config/.env` 配置好至少一个模型 Provider API Key（如 `OPENAI_API_KEY`）和路由模型，否则就算连上 OneBot 也无法生成回复。
- 部署好 NapCat 并完成 QQ 登录。NapCat 配置参见其官方文档；本项目不负责登录 NapCat。
- 准备一个独立的随机字符串作为 `ONEBOT11_ACCESS_TOKEN`，NapCat 端必须填同一值。

> 如果想让 NapCat 跨主机连到机器人，**不要**直接把监听地址改成 `0.0.0.0`。请仍然监听回环或受控内网，通过 SSH 隧道、WireGuard 或受控反向代理转发；同时 `ONEBOT11_ACCESS_TOKEN` 必填。详见第 8 节安全提示。

## 3. 服务端配置（本项目）

在 `runtime/config/.env` 中开启 OneBot 11 入口。完整变量以 [`runtime/config/.env.example`](../../runtime/config/.env.example) 为准，最常用项：

```env
ONEBOT11_ENABLED=true
ONEBOT11_BIND_HOST=127.0.0.1
ONEBOT11_BIND_PORT=8789
ONEBOT11_WEBSOCKET_PATH=/onebot/v11/ws
ONEBOT11_ACCESS_TOKEN=请改成一个独立随机值
ONEBOT11_REQUEST_TIMEOUT_MS=10000
ONEBOT11_MAX_MESSAGE_BYTES=1048576
```

关键约束（来自配置解析与监听器实现）：

- `ONEBOT11_ENABLED=true` 时 `ONEBOT11_ACCESS_TOKEN` **必填**，留空启动时会 `bail!` 报错，监听器不会起来。
- `ONEBOT11_REQUEST_TIMEOUT_MS` 取值范围 100..=120000 毫秒，默认 10000。它同时约束首个 `self_id` 上报窗口与 OneBot API 请求超时。
- `ONEBOT11_MAX_MESSAGE_BYTES` 取值范围 1 KiB..=16 MiB，默认 1 MiB。超过该大小的单帧会被关闭连接。
- 默认 `BIND_HOST=127.0.0.1`，**只允许本机直连**。改为公网或 `0.0.0.0` 前请先读第 8 节。

只想用 OneBot 入口、不接 QQ 官方的话，可以把 `QQ_BOT_APP_ID` / `QQ_BOT_APP_SECRET` 留空、`QQ_BOT_ENABLED=false`；QQ 官方 Token / Gateway 任务不会启动。

完整启动与诊断流程见 [runtime/README.md](../../runtime/README.md)。最小验证：

```bash
cd runtime
./botctl.sh start
curl -s http://127.0.0.1:8787/ping | head
```

启动日志会输出类似：

```text
OneBot 11 reverse WebSocket listening local_addr=127.0.0.1:8789 path=/onebot/v11/ws
```

`/ping` 也会显示 OneBot 监听状态、是否已连接、脱敏 `self_id`、最近心跳和最近收发时间。

## 4. NapCat 端配置

NapCat 的反向 WebSocket 客户端配置项在不同版本 UI / 配置文件里命名略有差异，下面给出字段语义映射；具体所在页面以你安装的 NapCat 版本为准。

在 NapCat 的网络配置中新增一条**反向 WebSocket 客户端**（Reverse WebSocket Client / Universal Reverse WebSocket Client），按下表填写：

| NapCat 字段 | 取值 | 说明 |
| --- | --- | --- |
| 服务器地址 / Host | `127.0.0.1` | 与本项目同一主机时填回环；跨主机时填能访问到本项目监听端口的主机 |
| 端口 / Port | `8789` | 与 `ONEBOT11_BIND_PORT` 一致 |
| 路径 / Path | `/onebot/v11/ws` | 与 `ONEBOT11_WEBSOCKET_PATH` 一致 |
| 上报 URL / 完整地址 | `ws://127.0.0.1:8789/onebot/v11/ws` | 若 NapCat 只让填一个 URL，按此拼接 |
| Token / Access Token | 与 `ONEBOT11_ACCESS_TOKEN` **逐字相同** | NapCat 会以 `Authorization: Bearer <token>` 发起握手 |
| 消息格式 | **数组 / Array** | 必须使用消息段数组；选 CQ 码字符串会被本项目忽略 |
| SelfID / 自身 ID | 通常自动取登录 QQ 号 | 如可手填，填机器人 QQ 号 |

启用后 NapCat 会主动连到本项目。连接成功的判定：

- 启动日志中出现 `OneBot 11 client connected`，`/ping` 中 OneBot 状态变为已连接并展示脱敏 `self_id`。
- NapCat 端给出连接成功提示，并按其配置间隔发送 heartbeat。

## 5. 连接与鉴权流程

握手与生命周期由 `qq-maid-gateway-rs/src/gateway/onebot11/server.rs` 处理，要点如下：

1. NapCat 发起 WS 握手。本项目校验请求头 `Authorization: Bearer <ONEBOT11_ACCESS_TOKEN>`，不匹配返回 401 并关闭。日志只记录“被拒绝的未授权连接”，**不**打印任何 Token 片段。
2. 可选 `X-Self-ID` 请求头。若提供，本项目先校验它是非空十进制 ID；NapCat 一般会自动带上。
3. 注册账号：首个上报的 `self_id`（来自 `X-Self-ID` 或第一条事件的 `self_id` 字段）会被锁定到本连接上下文。
   - 同一账号重连会**替换**旧连接，旧连接以 `replaced by newer connection` 关闭，避免主动推送发到断开的连接。
   - 已经锁定账号后，**不同** `self_id` 的连接会被直接拒绝，断开摘要为 `different self_id is not allowed`。多账号私服请见第 7 节。
4. 上报窗口：如果连接既没带 `X-Self-ID` 也没发任何事件，本项目会在 `ONEBOT11_REQUEST_TIMEOUT_MS` 后关闭连接，摘要 `self_id report timed out`。NapCat 通常会立刻发 lifecycle / heartbeat，无需手动处理。
5. 心跳监督：收到 heartbeat 后按其 `interval` 的 2 倍（不小于 `request_timeout`）设置心跳截止。心跳超时连接被关闭，摘要 `heartbeat timed out`。重连由 NapCat 端负责。
6. 关闭清理：连接断开后，绑定到该连接的 pending OneBot API 请求会立即返回明确失败，不会无限挂起。

## 6. 触发规则与回复

- **私聊**：文本进入 Core 复用 `/v1/respond` 链路，回复通过 `send_private_msg` 发回原 user_id。机器人自己发的消息（`user_id == self_id`）和 `message_sent` 事件都被忽略，避免形成回声循环。
- **群聊**：默认只在消息里出现指向本机器人 `self_id` 的 `at` 段时进入 Core。回复通过 `send_group_msg` 发到原 group_id。
  - 当前一期**没有**“命令前缀免 @”或“指定群免 @”的开关；不 @ 机器人时 `/todo` 等命令也不会触发。这条与 QQ 官方群聊不同，配置时请注意。
  - `at` 段只用于触发，不会污染送进 Core 的正文；正文中对其他成员的 `@` 保持原样。
- 去重 key 至少包含 `platform / self_id / message_type / group_id 或 user_id / message_id`，重连后短时间重复上报不会重复调用 LLM，跨账号也不会碰撞。
- 出站消息体始终使用消息段数组（`[{"type":"text","data":{"text":...}}]`），不依赖 CQ 码；OneBot API 返回非 `status=ok` 或非零 `retcode` 都会被如实记为发送失败，不会伪装成功。
- 当前 OneBot 入口不发送 QQ 官方 Markdown payload，图片也不出站；Core 若生成了 Markdown，只会以纯文本 fallback 形式发送。

## 7. 多账号、跨主机、与其它平台共存

- **同机多 NapCat 账号**：一期单账号锁定，第二个 `self_id` 会被拒绝。需要多账号时当前需要为每个账号跑一个本项目进程，各自监听不同端口。
- **跨主机**：保持 `ONEBOT11_BIND_HOST=127.0.0.1`，通过 SSH 隧道或受控网络转发。例如：

  ```bash
  # 在 NapCat 所在主机执行，把本地 8789 转发到运行 bot 的主机
  ssh -L 8789:127.0.0.1:8789 user@bot-host -N
  ```

  然后 NapCat 的反向 WS 地址填 `ws://127.0.0.1:8789/onebot/v11/ws`。
- **与 QQ 官方入口共存**：OneBot 与 QQ 官方 Gateway 是独立任务，两者互不阻塞；OneBot 连接异常不会拖垮官方 Gateway，反之亦然。可同时开启 `QQ_BOT_ENABLED=true` 与 `ONEBOT11_ENABLED=true`。
- **会话隔离**：OneBot 的 session / memory / todo 等业务键目前按 `platform=onebot11`、`scope_key=private:{user_id}` / `group:{group_id}`、`owner_key` 等现有维度隔离，与 QQ 官方 `private:{openid}` / `group:{group_openid}` 不会串话。隔离键设计见 [../design/scope-identity-boundary.md](../design/scope-identity-boundary.md)。

## 8. 安全提示

- 默认监听 `127.0.0.1`，不建议直接改 `0.0.0.0` 暴露公网。如确需公网访问，应由反向代理负责 TLS，并在代理层限制来源 IP。
- `ONEBOT11_ACCESS_TOKEN` 必须填，且为独立随机值，不要与 `WECHAT_SERVICE_TOKEN`、QQ `AppSecret` 等复用。
- All logs 默认脱敏：日志只输出脱敏 `self_id`、是否监听 / 连接、最近心跳和固定断开摘要，**不**打印完整 Token、完整 ID、消息正文或 API response envelope。
- `ONEBOT11_MAX_MESSAGE_BYTES` 同时控制 Axum 的单帧与单消息上限；异常客户端无法用超大帧打满内存，会被关闭连接。

## 9. 故障排查

| 现象 | 排查方向 |
| --- | --- |
| NapCat 一直连不上，本项目日志显示 `rejected unauthorized OneBot 11 WebSocket connection` | 两边 `ONEBOT11_ACCESS_TOKEN` 不一致。注意 NapCat 端是否需要去掉前缀 `Bearer `，多数实现会自动补齐 |
| NapCat 連上但 `/ping` 一直未连接 | 检查 `ONEBOT11_ENABLED=true`、`ONEBOT11_ACCESS_TOKEN` 非空；启动日志若没有 `OneBot 11 reverse WebSocket listening`，说明监听器没起来，看 stderr 是否报 `access token is required when enabled` |
| 连接很快被关闭，摘要 `self_id report timed out` | NapCat 未发送任何事件，可确认登录态是否正常；或调高 `ONEBOT11_REQUEST_TIMEOUT_MS` |
| 连上后日志显示 `different self_id is not allowed` | 本进程已经绑定到另一个 `self_id`，需要重启本项目进程，或确认是否多个 NapCat 账号连到同一端口 |
| 群里 @ 了机器人但不回复 | 确认 NapCat 发的 `at` 段 `qq` 字段就是本项目锁定的 `self_id`；确认群消息没有被本项目群过滤忽略（日志 `group_not_triggered`）；确认模型 Provider 配置可用、`/ping` Core 健康 |
| 机器人不响应 `/todo` 等命令 | 当前群聊一期要求先 @ 机器人；私聊则可直接发命令 |
| 私聊能收到消息但无回复 | 多为模型调用失败。查 `/ping` 与运行日志的 Core / LLM 报错；常见是 `OPENAI_API_KEY` 缺失或 `LLM_MODEL` 指向未配置 provider 的模型 |
| 收到机器人自己消息被无限回复 | 不会发生；`message_sent` 与 `user_id == self_id` 都在 adapter 阶段被过滤。如果出现，请优先确认 NapCat 是否正常上报 `post_type=message_sent` |
| 主动推送提示账号未连接 | RSS / Todo 推送目标里的 OneBot `account_id` 与当前连接的 `self_id` 不一致，或 NapCat 已断开。`/ping` 会显示最近断开摘要 |

## 10. 主动推送

Todo 每日提醒、RSS 等主动推送现已支持 OneBot 目标。向本项目内部 push 接口提交时，目标需带平台信息，使其路由到 OneBot 发送链路，而不是 QQ 官方：

- 旧 QQ 官方推送请求未带 `platform` 仍按旧逻辑处理，保持兼容；
- 带 `platform=onebot11` 的请求按 `account_id` 精确路由到对应连接；
- 该账号未连接时返回明确失败，不会伪造发送成功；
- 出站内容与聊天回复一致，一期只发文本。

具体请求字段格式以推送链路实现为准；如需新增具备平台信息的推送来源，请联系维护者确认字段契约，不要自行猜测写入。

## 11. 后续计划

以下能力**不在**本接入文档覆盖范围，由后续任务按需补齐：

- 正向 WebSocket 服务端、HTTP 上报服务端等其它 OneBot transport
- 出站 `reply` / `at` / `image` 消息段
- 入站 `reply` / `image` / `face` 段的内容提取与多模态接入
- “指定群免 @”、“命令前缀免 @”等群聊触发扩展
- 多账号在同进程内并行接入
- 群成员角色等 Notice / Request 业务处理

需要其中任意一项时，请先阅读 [../tasks/onebot11-connect.md](../tasks/onebot11-connect.md) 的边界约束再开工。