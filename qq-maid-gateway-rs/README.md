# Rust 平台入口与 QQ 文本网关

`qq-maid-gateway-rs/` 是 Rust 平台入口层，当前主入口是 QQ 官方 C2C / 群 at / 普通群文本接入，并包含可选微信服务号文本回调和 OneBot 11 反向 WebSocket 聊天入口。旧 Python bot 接入层已经移除；新平台接入能力优先放到本 gateway 的 adapter / sender 边界，业务能力优先放到 `qq-maid-core/`。

## 多平台入口边界

```mermaid
flowchart LR
    subgraph platform_in["平台入口"]
        qq["QQ 官方 Gateway"]
        onebot["OneBot 11 反向 WebSocket<br/>文本 / 引用 / 媒体入站"]
        wechat["微信回调入口"]
    end

    subgraph adapters["平台 adapter"]
        qq_adapter["qq_official adapter"]
        onebot_adapter["onebot text / at / reply / media adapter"]
        wechat_adapter["wechat_service adapter"]
    end

    subgraph inbound["统一入站模型"]
        inbound_msg["InboundMessage"]
        actor["Actor"]
        conversation["Conversation"]
        core_request["CoreRequest"]
    end

    subgraph core["Core 边界"]
        core_service["CoreService::respond"]
        business_keys["scope_key / owner_key"]
        state["Session / Pending / Memory / Todo"]
        llm["LLM / Tool Loop"]
        outbound_msg["OutboundMessage"]
    end

    subgraph outbound["出站投递"]
        reply_target["ReplyTarget"]
        delivery_target["DeliveryTarget / raw_target_id"]
        capability["ReplyCapability"]
        qq_sender["QQ sender"]
        onebot_sender["OneBot text sender"]
        wechat_sender["微信 sender"]
    end

    qq --> qq_adapter --> inbound_msg
    onebot --> onebot_adapter --> inbound_msg
    wechat --> wechat_adapter --> inbound_msg
    inbound_msg --> actor --> core_request
    inbound_msg --> conversation --> core_request
    core_request --> core_service
    core_request --> business_keys --> state
    core_service --> llm
    core_service --> outbound_msg
    outbound_msg --> reply_target --> delivery_target --> capability
    capability --> qq_sender
    capability --> onebot_sender
    capability --> wechat_sender
```

这张图里的命名边界有固定含义：

- `InboundMessage` 是 Gateway 内部的平台无关入站模型，包含 `Actor` 与 `Conversation`。QQ、OneBot、微信等协议字段只能在各自 adapter 内解析。
- `CoreRequest` 是 Gateway 调用 Core 的稳定契约。Core 可以看到平台枚举、Actor 和 Conversation，但不理解 QQ `msg_seq`、stream id、微信 XML 字段或 OneBot CQ 片段。
- OneBot 11 已实现默认关闭的单账号反向 WebSocket server、鉴权、生命周期/心跳、连接替换、API request/response 共用连接上下文、文本/结构化 at/reply/图片/文件入站 adapter、Core/Command 编排、文本 sender、主动推送精确路由和脱敏状态。reply 以平台 `message_id` 写入独立 scope 的进程内 ref_index，安全 http/https 图片可进入既有多模态链路；客户端本机路径、`file://` 和 base64 只保留不可读摘要。Core stream 只在可信 `Completed` 后发送一条最终文本，不向 OneBot 平台发送 status/delta，也不实现富媒体发送。
- `scope_key` / `owner_key` 是业务隔离键，用于 Session、Pending、Memory、Todo 等状态归属，不是发送地址。
- `ReplyTarget` / `DeliveryTarget` 保存真实投递目标，必须保留平台和 `raw_target_id`。发送逻辑只能使用投递目标调用 sender，不能从 `scope_key` 或 `owner_key` 反解析平台 ID。
- RSS、Notification、Todo 提醒和 Push 这类主动投递也必须携带原始 delivery target；后续多平台收敛时不要把目标统一替换成 namespaced 字符串。

## 当前范围

- 处理 `C2C_MESSAGE_CREATE`、`GROUP_AT_MESSAGE_CREATE` 和平台实际推送到 Gateway 的普通 `GROUP_MESSAGE_CREATE` 文本消息；普通群消息默认采用 `mention` 模式，仅响应命令、@ 和回复机器人消息，可按配置关闭或改为提示词触发模式。QQ 官方通道只能处理平台实际推送的事件，关键词不会让平台额外推送不可见的普通非 @ 消息。
- `/ping` 会在 gateway 本地返回诊断信息，直接读取 Core 进程内健康快照；`/ping check` 会调用 `CoreService::upstream_check()` 执行一次不写会话的最小上游检查。
- 文本回复使用 QQ C2C `msg_type: 0`、原消息 `msg_id` 和递增 `msg_seq`。
- 入站附件不会改 Core 稳定请求模型；图片等附件信息会追加到文本末尾，例如 `[附件 image/jpeg: a.jpg https://example.test/a.jpg]`。
- Markdown 和图片保留独立 outbound 类型、payload 构造和发送入口；发送失败会 warn 并 fallback 到文本。C2C 流式回复当前固定使用 Markdown 流式载荷，首帧成功后不再补发普通全文。
- Core 的统一通知 Worker 和 Todo 每日提醒通过进程内 `PushSink` 主动推送；RSS 只生产 Notification Outbox 任务，不再维护独立发送链路。
- 微信服务号入口默认关闭；启用后处理 GET URL 验证、POST 明文 `text` XML、同步文本 XML 快路径，以及超出同步安全预算后的客服文本补发。客服补发按需获取 `access_token`，Markdown 会降级为 text。
- OneBot 11 入口默认关闭，支持 OneBot-only 启动，也可与 QQ 官方、微信入口并存。当前只接受反向 WebSocket + Array 消息格式；启用后建立单账号连接，安全适配私聊及明确 at 当前机器人或引用机器人出站消息的群聊，按 segment 顺序映射文本、图片、文件和未知段，并通过同一连接发送私聊/群聊纯文本 action。ref_index 仅在进程内保存，重启后引用旧消息会安全 miss；安全远程图片可进入图片理解，文件和不可读媒体只生成摘要、不解析正文。入站按权威 Core scope 使用统一会话调度配置，同 scope 串行、不同 scope 并发，并具备有界队列、活动 scope 上限、idle 回收和 shutdown cancel。首次账号会锁定进程内 `self_id`，同账号新连接替换旧连接，不同账号会被拒绝；连接异常不会结束监听器或其它入口，未完成发送会返回可重试错误。出站还不支持图片、文件、Markdown、平台原生引用、at、流式输出或其他富媒体消息段。
- 不做频道、频道私信、Ark、Embed、Keyboard、多租户或旧接入层兼容。
- 微信服务号暂不做加密 XML、模板消息、图片语音视频、菜单事件、主动推送或流式输出；客服消息只实现慢请求文本补发。

## 开发边界

- QQ 平台字段解析、intent、白名单、消息去重和发送分支优先放在本目录维护。
- 普通聊天、查询、天气、翻译、session、todo、memory、RSS 指令、业务 Tool 和 prompt 组装放在 `qq-maid-core/`。
- gateway 调用 Core 时只走 `CoreService` 进程内接口，不要重新引入旧 `/query`、HTTP `/memory`、`/v1/chat` 或任何 localhost respond 调用路径。
- 主动推送只通过 `PushSink` 进程内边界进入 Gateway，不要恢复本机 push HTTP、push token 或 push 端口。
- 发送分支只接收 `ReplyTarget` / `DeliveryTarget` 里的平台原始目标；不要让 Core、LLM、Tool Loop 或业务 store 根据 `scope_key` 推断 QQ、OneBot 或微信发送参数。

## 源码边界

当前 Gateway 主链路按以下边界维护：

- `src/gateway/mod.rs`：运行域装配和顶层编排，只负责初始化共享状态、绑定进程内 push sink、维护重连循环，并把 WebSocket 协议处理委托给下层模块。
- `src/gateway/protocol.rs`：QQ Gateway WebSocket 协议层，负责 gateway 地址获取、HELLO/IDENTIFY/RESUME、心跳、READY/RESUMED、`INVALID_SESSION` 和 envelope 分发。
- `src/gateway/event.rs`：QQ 平台 payload 到 `C2cMessage` / `GroupMessage` 的解析与兼容字段处理。
- `src/gateway/cache.rs`：gateway 内部短时缓存，只保存 reply 回填和机器人 outbound message id 等可丢弃状态，不承载业务语义。
- `src/gateway/c2c.rs`：C2C 私聊消息处理管道，负责 Signal Layer 回填、本地 `/ping`、Core 调用和普通回复发送。
- `src/gateway/stream.rs`：C2C Markdown 流式发送状态机，负责分片、终包、QQ stream id/index 续接和普通回复 fallback 边界。
- `src/gateway/group.rs`：群消息处理管道，负责群消息到 Core 的调用、群回复发送和群 at 回复前缀。
- `src/gateway/group_filter.rs`：群消息过滤、触发策略和群/成员冷却判定。
- `src/gateway/outbound.rs`：QQ 出站发送包装和 runtime 发送状态记录，保持“真实发送结果再记录状态”的约束。
- `src/respond.rs`：gateway 到 CoreService 的进程内桥接层，负责 CoreRequest 映射、错误脱敏，以及 reply block / 附件备注拼接。
- `src/gateway/push.rs`：进程内主动推送实现。
- `src/gateway/wechat_service.rs`：微信服务号文本回调 HTTP 入口，负责签名校验、明文 XML 解析、Core 调用、同步 XML 回复、慢请求去重和客服文本补发。
- `src/gateway/platform/wechat_service.rs`：微信服务号平台字段到统一 `InboundMessage` / `CoreRequest` 的映射，以及 XML 解析和渲染 helper。
- `src/gateway/platform/onebot11/mod.rs`：OneBot 11 私聊、群聊、结构化 at/reply 以及有序文本/图片/文件 segment 到统一 `InboundMessage` 的安全映射和触发过滤。
- `src/gateway/onebot11/dispatch.rs`：去重后的 OneBot 入站引用索引、Core 调用、非流式最终回复收口、结构化 output 文本降级、sender 调用和出站 message_id/可见实体快照回填。
- `src/gateway/onebot11/protocol.rs`：OneBot 11 事件、消息段、action / response、`echo`、生命周期、心跳和无精度损失 ID 类型。
- `src/gateway/onebot11/connection.rs`：单账号活动连接、同账号替换策略和 API `echo` 关联上下文。
- `src/gateway/onebot11/sender.rs`：`send_private_msg` / `send_group_msg` 文本 segment、真实响应校验和平台消息 ID 提取。
- `src/gateway/onebot11/server.rs`：反向 WebSocket 监听、路径与 Bearer 鉴权、连接事件循环、超时和优雅退出。

维护时应尽量保持这些边界，不要把 WebSocket 协议细节、Core 业务调用和 QQ 发送状态记录重新堆回同一个超长文件。

## 配置

从仓库根目录复制模板并填入真实配置：

```bash
cp runtime/config/.env.example runtime/config/.env
```

默认配置入口位于运行目录，优先读取 `runtime/config/.env`，其次读取 `runtime/.env`；临时排障可用 `GATEWAY_ENV_FILE` 指向单独配置文件。

主要变量：

```env
QQ_BOT_ENABLED=true
QQ_BOT_APP_ID=你的QQ机器人AppID
QQ_BOT_APP_SECRET=你的QQ机器人AppSecret
QQ_BOT_SANDBOX=false
QQ_BOT_API_BASE=https://api.sgroup.qq.com
QQ_BOT_TOKEN_REFRESH_MARGIN_SECONDS=60
QQ_MAID_ENABLE_MARKDOWN=true
QQ_MAID_ENABLE_IMAGE=false
QQ_MAID_C2C_VISIBLE_PROGRESS_STATUS_ENABLED=true
QQ_MAID_GATEWAY_VERBOSE_LOG=false
QQ_MAID_GROUP_MESSAGE_MODE=mention
QQ_MAID_GROUP_ACTIVE_KEYWORDS=小女仆
QQ_MAID_BOT_MENTION_IDS=
RUST_LOG=info,qq_maid_gateway_rs=debug
```

`QQ_BOT_APP_ID` 与 `QQ_BOT_APP_SECRET` 必须成对配置；两项均缺失表示 QQ 官方 Bot 未绑定。此时不会创建 Token、API client、Gateway 或重连任务，微信服务号或 OneBot 11 仍可独立运行。凭证存在时可用 `QQ_BOT_ENABLED=false` 暂时禁用；旧配置未设置该开关时仍默认启用。配置由启动时读取，`qbot config bot --unbind`、`--disable` 和重新绑定都需重启生效。

兼容旧变量名：

```env
QQ_APPID=你的QQ机器人AppID
QQ_SECRET=你的QQ机器人AppSecret
```

普通群消息由 `QQ_MAID_GROUP_MESSAGE_MODE` 控制，默认 `mention` 保持有限触发；`off` 完全关闭普通群消息，`command` 只处理 `/` 或全角 `／` 开头的命令，`mention` 额外处理平台 @ 标记和回复机器人消息，`active` 只处理包含 `QQ_MAID_GROUP_ACTIVE_KEYWORDS` 指定提示词的普通群消息，提示词默认 `小女仆`，多个用英文逗号分隔。第一个有效关键词同时作为程序生成状态提示和兜底文案中的机器人主称呼，其余关键词仍作为 active 模式别名；仅当新变量完全未设置时，旧变量 `QQ_MAID_STATUS_DISPLAY_NAME` 才作为主称呼回退，且不会加入 active 关键词。旧变量 `QQ_MAID_ENABLE_GROUP_MESSAGES` 仅在未设置新变量时兼容，`false` 映射为 `off`，`true` 映射为 `active`，未设置时默认 `mention`。这些策略只对 QQ 官方已经推送到 Gateway 的群事件生效；如果平台没有推送普通非 @ 群消息，Gateway 无法通过关键词提前收到或登记该消息。群聊不会开放通用 Harness、文件处理或代码执行；Tool Calling 由 Core 的 `TOOL_CALLING_GROUP_ENABLED` 控制且默认关闭。gateway 只负责把群聊目标传给 Core，由 Core 按既有命令和普通聊天边界处理。

普通群事件是否 @ 当前机器人只信任官方结构化 `mentions[].is_you == true`；旧的 AppID、openid、member_openid、CQ 文本和 `<@...>` 文本不再作为触发依据。`QQ_MAID_BOT_MENTION_IDS` 仅保留为旧配置兼容，不应再用于修正普通群 @ 判定。不要把真实 ID 写入公开文档或提交到仓库。

普通群消息会过滤自己发送的消息、可识别的其它机器人消息、空内容/无附件消息和重复 `message_id`，并使用群级与群成员级内存冷却避免刷屏；但发送给 Core 的 `scope_key` 仍保持群会话维度，actor 仅表示群内发言人，避免同一个用户的私聊与群聊 session / pending / visible snapshot / ref_index 串用。只有 QQ 官方实际推送且 payload 带 `current_msg_idx / msg_idx` 的群消息，才能提前登记到运行期 ref_index；平台未推送或缺字段时，后续引用只能依赖当前引用事件 payload 兜底或已有索引。

`QQ_MAID_C2C_VISIBLE_PROGRESS_STATUS_ENABLED` 控制私聊 Tool Loop 的可见进度文本，默认开启，只在 Core 输出策略为 `progress_then_complete` / `progress_then_stream` 时发送一次受控短提示。它不是 QQ 原生 typing 状态；原生 typing 由 `QQ_MAID_AGENT_TYPING_ENABLED` / `QQ_MAID_AGENT_TYPING_DELAY_MS` 单独控制。

OneBot 11 入口最小配置：

```env
ONEBOT11_ENABLED=false
ONEBOT11_BIND_HOST=127.0.0.1
ONEBOT11_BIND_PORT=8789
ONEBOT11_WEBSOCKET_PATH=/onebot/v11/ws
ONEBOT11_ACCESS_TOKEN=
ONEBOT11_REQUEST_TIMEOUT_MS=10000
ONEBOT11_MAX_MESSAGE_BYTES=1048576
```

启用时 `ONEBOT11_ACCESS_TOKEN` 必填，客户端需携带 `Authorization: Bearer <token>`；推荐保持回环地址监听。`X-Self-ID` 可以在握手时上报，也可由首个合法事件上报。`/ping all` 和控制台只显示 token 是否配置、监听/连接状态、脱敏 `self_id`、最近心跳与断开摘要，不输出完整 QQ 号、token、媒体 URL 或消息正文。当前支持私聊、明确 at 当前机器人或引用机器人出站消息的群聊、图片/文件入站摘要、Core 命令/聊天和文本发送；ref_index 仅保存在进程内，重启后的历史引用会安全 miss；平台流式和图片/文件出站仍不支持。

微信服务号最小配置：

```env
WECHAT_SERVICE_ENABLED=false
WECHAT_SERVICE_TOKEN=
WECHAT_SERVICE_APP_ID=
WECHAT_SERVICE_APP_SECRET=
WECHAT_SERVICE_BIND_HOST=127.0.0.1
WECHAT_SERVICE_BIND_PORT=8788
WECHAT_SERVICE_CALLBACK_PATH=/wechat/service
WECHAT_SERVICE_REPLY_TIMEOUT_MS=4000
```

生产环境建议保持本机监听 `127.0.0.1`，由 Nginx、Caddy 或 Cloudflare Tunnel 把公网 HTTPS `https://你的域名/wechat/service` 转发到 `http://127.0.0.1:8788/wechat/service`。微信公众平台服务器配置中 URL 填公网 HTTPS 地址，Token 填 `WECHAT_SERVICE_TOKEN`，消息加解密方式选择明文模式，`EncodingAESKey` 当前未使用。详细配置和排障步骤见 [runtime/README.md](../runtime/README.md#微信服务号文本回调配置)。

`/ping all` 的调试详情会展示微信入口安全摘要，包括启用状态、监听地址和端口、callback path、`token` / `app_id` / `app_secret` 是否已配置、`access_token` 是否按需获取、客服消息是否可用、同步回复预算、当前支持模式和暂不支持能力。secret 类字段只显示 `configured` / `missing` 等摘要，不输出真实值；未启用时显示 `disabled`，不表示 QQ Gateway 异常。

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

默认日志会记录 gateway 连接、READY/RESUMED、重连、收到 C2C 事件、调用进程内 CoreService、回发 QQ 消息和失败状态。日志中的 openid/user_id 会脱敏，不记录 QQ raw event envelope、Authorization header、AppSecret 或 token，也不默认打印消息正文。

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
- 能通过进程内 `CoreService` 调用 `qq-maid-core`。
- 能回发 C2C 文本。
- `/ping` 能直接返回 gateway 诊断信息。
- `/ping check` 能主动验证 LLM 鉴权、模型、参数和响应解析，且不写入聊天历史。
- 重复 `message_id` 不重复回复。
- WebSocket 断开后能自动重连。
