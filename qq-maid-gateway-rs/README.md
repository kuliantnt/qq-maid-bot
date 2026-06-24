# Rust QQ 文本网关

`qq-maid-gateway-rs/` 是 Rust QQ 官方 C2C / 群 at 接入层，也是后续 QQ 接入开发的主线方向。旧 Python bot 接入层已经移除；新 QQ 接入能力优先放到本 gateway，业务能力优先放到 `qq-maid-core/`。

```text
QQ Gateway C2C_MESSAGE_CREATE
  -> qq-maid-gateway-rs
  -> qq-maid-core POST /v1/respond
  -> QQ OpenAPI /v2/users/{openid}/messages

QQ Gateway GROUP_AT_MESSAGE_CREATE
  -> qq-maid-gateway-rs
  -> qq-maid-core POST /v1/respond
  -> QQ OpenAPI /v2/groups/{group_openid}/messages

qq-maid-core RSS scheduler
  -> qq-maid-gateway-rs POST /internal/push
  -> QQ OpenAPI /v2/users 或 /v2/groups 消息发送
```

## 当前范围

- 处理 `C2C_MESSAGE_CREATE` 和 `GROUP_AT_MESSAGE_CREATE` 文本消息；普通 `GROUP_MESSAGE_CREATE` 默认关闭，可按模式启用。
- `/ping` 会在 gateway 本地返回诊断信息，只做短超时 `/healthz` 探测，不产生模型请求；`/ping check` 会通过 `/v1/respond` 执行一次不写会话的最小上游检查。
- 文本回复使用 QQ C2C `msg_type: 0`、原消息 `msg_id` 和递增 `msg_seq`。
- 入站附件不会改 `/v1/respond` schema；图片等附件信息会追加到 `content` 末尾，例如 `[附件 image/jpeg: a.jpg https://example.test/a.jpg]`。
- Markdown 和图片保留独立 outbound 类型、payload 构造和发送入口；发送失败会 warn 并 fallback 到文本。第一版真机验收不以富媒体成功发送为前置条件。
- 本机内部 `/internal/push` 供 Core RSS 调度主动推送，默认只监听 `127.0.0.1`，可通过共享 token 限制调用方。
- 不做频道、频道私信、Ark、Embed、Keyboard、多租户或旧接入层兼容。

## 开发边界

- QQ 平台字段解析、intent、白名单、消息去重和发送分支优先放在本目录维护。
- 普通聊天、查询、天气、翻译、session、todo、memory、RSS 指令和 prompt 组装放在 `qq-maid-core/`。
- gateway 调用 Core 时只走 `QQ_MAID_RESPOND_URL` 指向的 `/v1/respond`，不要重新引入旧 `/query`、HTTP `/memory` 或 `/v1/chat` 调用路径。
- `/internal/push` 是给 Core RSS 调度使用的本机内部出口，生产环境应保持本机监听，并按需配置 `QQ_MAID_PUSH_TOKEN` / `RSS_PUSH_TOKEN` 共享 token。

## 配置

从仓库根目录复制模板并填入真实配置：

```bash
cp runtime/.env.example runtime/config/.env
```

默认配置入口位于运行目录，优先读取 `runtime/config/.env`，其次读取 `runtime/.env`；临时排障可用 `GATEWAY_ENV_FILE` 指向单独配置文件。

主要变量：

```env
QQ_BOT_APP_ID=你的QQ机器人AppID
QQ_BOT_APP_SECRET=你的QQ机器人AppSecret
QQ_BOT_SANDBOX=false
QQ_BOT_API_BASE=https://api.sgroup.qq.com
QQ_BOT_TOKEN_REFRESH_MARGIN_SECONDS=60
QQ_MAID_RESPOND_URL=http://127.0.0.1:8787/v1/respond
QQ_MAID_ENABLE_MARKDOWN=true
QQ_MAID_ENABLE_IMAGE=false
QQ_MAID_GATEWAY_VERBOSE_LOG=false
QQ_MAID_GROUP_MESSAGE_MODE=off
QQ_MAID_PUSH_ENABLED=true
QQ_MAID_PUSH_HOST=127.0.0.1
QQ_MAID_PUSH_PORT=8788
QQ_MAID_PUSH_TOKEN=
RUST_LOG=info,qq_maid_gateway_rs=debug
```

兼容旧变量名：

```env
QQ_APPID=你的QQ机器人AppID
QQ_SECRET=你的QQ机器人AppSecret
```

普通群消息由 `QQ_MAID_GROUP_MESSAGE_MODE` 控制，默认 `off` 保持安静；`command` 只处理 `/` 或全角 `／` 开头的命令，`mention` 额外处理平台 @ 标记和回复机器人消息，`active` 会处理通过过滤后的普通群消息。旧变量 `QQ_MAID_ENABLE_GROUP_MESSAGES` 仅在未设置新变量时兼容，`false` 映射为 `off`，`true` 映射为 `active`。

普通群消息会过滤自己发送的消息、可识别的其它机器人消息、空内容/无附件消息和重复 `message_id`，并使用群级与群成员级内存冷却避免刷屏；但发送给 Core 的 `scope_key` 仍保持 `group:<group_openid>`，避免把 RSS、会话等按当前 QQ 目标建模的能力意外拆成成员分片。

不要提交真实配置文件、AppSecret、Access Token、openid、私聊内容或截图中的敏感信息。

## 日志

默认日志级别为 `info,qq_maid_gateway_rs=debug`，可写在运行目录配置：

```env
RUST_LOG=info,qq_maid_gateway_rs=debug
```

临时排障可在启动命令前覆盖：

```bash
RUST_LOG=debug make run
```

默认日志会记录 gateway 连接、READY/RESUMED、重连、收到 C2C 事件、调用 `/v1/respond`、回发 QQ 消息和失败状态。日志中的 openid/user_id 会脱敏，不记录 QQ raw event envelope、Authorization header、AppSecret 或 token，也不默认打印消息正文。

确需查看解析后的消息正文时，可以临时开启：

```bash
QQ_MAID_GATEWAY_VERBOSE_LOG=true make run
```

也可以写入 `runtime/config/.env`：

```env
QQ_MAID_GATEWAY_VERBOSE_LOG=true
```

该开关只控制是否额外打印 `extracted_content` 字段，不改变 `RUST_LOG` 过滤级别。排障完成后应改回 `false`。

## 运行

统一程序会先启动 Core HTTP，再启动 Rust C2C gateway。前台调试时直接运行：

```bash
make run
```

部署后的控制脚本、真实 `.env` 位置、日志目录和运行产物说明见 [runtime/README.md](../runtime/README.md)。

## 检查

从仓库根目录执行：

```bash
make test-gateway
```

该命令会先检查 `qq-maid-common/`，再检查 gateway。gateway 自身检查等价于：

```bash
cargo fmt -p qq-maid-gateway-rs -- --check
cargo test -p qq-maid-gateway-rs
cargo check -p qq-maid-gateway-rs
```

第一版真机验收只要求：

- 能获取 QQ Access Token。
- 能连接 QQ Gateway。
- 能收到 C2C 文本事件。
- 能调用 `qq-maid-core` 的 `/v1/respond`。
- 能回发 C2C 文本。
- `/ping` 能直接返回 gateway 诊断信息。
- `/ping check` 能主动验证 LLM 鉴权、模型、参数和响应解析，且不写入聊天历史。
- 重复 `message_id` 不重复回复。
- WebSocket 断开后能自动重连。
