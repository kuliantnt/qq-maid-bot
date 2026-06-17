# Rust QQ 文本网关

`qq-maid-gateway-rs/` 是 Rust QQ 官方 C2C / 群 at 接入层，也是后续 QQ 接入开发的主线方向。旧 Python bot 接入层已经移除；新 QQ 接入能力优先放到本 gateway，业务能力优先放到 `qq-maid-llm/`。

```text
QQ Gateway C2C_MESSAGE_CREATE
  -> qq-maid-gateway-rs
  -> qq-maid-llm POST /v1/respond
  -> QQ OpenAPI /v2/users/{openid}/messages

QQ Gateway GROUP_AT_MESSAGE_CREATE
  -> qq-maid-gateway-rs
  -> qq-maid-llm POST /v1/respond
  -> QQ OpenAPI /v2/groups/{group_openid}/messages

qq-maid-llm RSS scheduler
  -> qq-maid-gateway-rs POST /internal/push
  -> QQ OpenAPI /v2/users 或 /v2/groups 消息发送
```

## 当前范围

- 处理 `C2C_MESSAGE_CREATE` 和 `GROUP_AT_MESSAGE_CREATE` 文本消息。
- `/ping` 会在 gateway 本地返回诊断信息，只对 `qq-maid-llm` 做短超时 `/healthz` 探测，不调用 `/v1/respond`。
- 文本回复使用 QQ C2C `msg_type: 0`、原消息 `msg_id` 和递增 `msg_seq`。
- 入站附件不会改 `/v1/respond` schema；图片等附件信息会追加到 `content` 末尾，例如 `[附件 image/jpeg: a.jpg https://example.test/a.jpg]`。
- Markdown 和图片保留独立 outbound 类型、payload 构造和发送入口；发送失败会 warn 并 fallback 到文本。第一版真机验收不以富媒体成功发送为前置条件。
- 本机内部 `/internal/push` 供 LLM RSS 调度主动推送，默认只监听 `127.0.0.1`，可通过共享 token 限制调用方。
- 不做频道、频道私信、Ark、Embed、Keyboard、多租户或旧接入层兼容。

## 配置

复制模板并填入真实配置：

```bash
cp .env.example .env
```

如需只给 gateway 保留局部配置，也兼容 `qq-maid-gateway-rs/config/.env`。

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

不要提交真实 `.env`、AppSecret、Access Token、openid、私聊内容或截图中的敏感信息。

## 日志

默认日志级别为 `info,qq_maid_gateway_rs=debug`，可写在仓库根 `.env`：

```env
RUST_LOG=info,qq_maid_gateway_rs=debug
```

临时排障可在启动命令前覆盖：

```bash
RUST_LOG=debug make run-gateway
```

默认日志会记录 gateway 连接、READY/RESUMED、重连、收到 C2C 事件、调用 `/v1/respond`、回发 QQ 消息和失败状态。日志中的 openid/user_id 会脱敏，不记录 QQ raw event envelope、Authorization header、AppSecret 或 token，也不默认打印消息正文。

确需查看解析后的消息正文时，可以临时开启：

```bash
QQ_MAID_GATEWAY_VERBOSE_LOG=true make run-gateway
```

也可以写入 `.env`：

```env
QQ_MAID_GATEWAY_VERBOSE_LOG=true
```

该开关只控制是否额外打印 `extracted_content` 字段，不改变 `RUST_LOG` 过滤级别。排障完成后应改回 `false`。

## 运行

先启动 Rust LLM 服务：

```bash
make run-llm
```

再启动 Rust C2C gateway：

```bash
make run
```

`make run` 当前等价于 `make run-gateway`。

## 检查

从仓库根目录执行：

```bash
make test-gateway
```

等价于：

```bash
cargo fmt -p qq-maid-gateway-rs -- --check
cargo test -p qq-maid-gateway-rs
cargo check -p qq-maid-gateway-rs
```

第一版真机验收只要求：

- 能获取 QQ Access Token。
- 能连接 QQ Gateway。
- 能收到 C2C 文本事件。
- 能调用 `qq-maid-llm` 的 `/v1/respond`。
- 能回发 C2C 文本。
- `/ping` 能直接返回 gateway 诊断信息。
- 重复 `message_id` 不重复回复。
- WebSocket 断开后能自动重连。
