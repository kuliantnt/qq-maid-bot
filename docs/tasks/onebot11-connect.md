# 任务：为 qq-maid-bot 一步到位接入 OneBot 11 反向 WebSocket，并建立多平台 Gateway 边界

## 背景

项目当前由以下 Rust workspace 成员组成：

- qq-maid-common
- qq-maid-gateway-rs
- qq-maid-core

现有 `qq-maid-gateway-rs` 主要面向 QQ 官方机器人 Gateway，包含：

- QQ 官方 Gateway WebSocket 连接
- Identify / Resume / Heartbeat
- QQ 官方 API 鉴权与消息回发
- `/v1/respond` 调用
- Markdown / 纯文本 / 图片出站选择
- 主动推送服务
- 消息去重
- `/ping` 运行状态

现在需要增加 OneBot 11 接入，主要兼容 NapCatQQ 和 Lagrange.OneBot。

本次不是用 OneBot 11 替换 QQ 官方机器人，而是让二者能够独立启用或同时运行。不要把 OneBot 协议处理直接堆入现有官方 Gateway 主循环，应建立清晰的平台适配边界。

## 目标

完成后应达到：

1. QQ 官方 Gateway 行为保持兼容。
2. 可以仅启用 QQ 官方平台。
3. 可以仅启用 OneBot 11 平台，不要求配置 QQ 官方 AppID/AppSecret。
4. 可以同时启用两个平台。
5. NapCatQQ 或 Lagrange.OneBot 可通过反向 WebSocket 连接本项目。
6. OneBot 私聊消息可以进入现有 `/v1/respond` 链路并正常回复。
7. OneBot 群聊可以按触发规则进入现有回复链路。
8. Todo、RSS 等主动推送能够向 OneBot 私聊或群聊目标发送。
9. OneBot 断线、非法事件、未知消息段或 API 请求失败不能拖垮其他平台。
10. 架构上预留未来增加 OneBot 11 正向 WebSocket transport 的空间，但本次不必实现。

## 实现原则

1. 先阅读仓库根目录及各 crate 下的：
   - AGENTS.md
   - README.md
   - Cargo.toml
   - 配置示例
   - 部署脚本
   - 现有测试
2. 搜索并梳理现有调用链：
   - gateway 启动入口
   - QQ 官方事件解析
   - RespondClient
   - RespondRequest / RespondResponse
   - render_respond_response
   - OutboundMessage
   - QqApiClient
   - push server
   - todo / RSS 主动推送
   - session key / conversation key
   - ping runtime status
3. 以仓库实际结构为准，不要仅根据本任务书中的建议文件名猜测。
4. 优先复用已有 Respond、渲染、媒体、去重和日志设施。
5. 采用最小而清晰的改动，不进行与任务无关的大规模重构。

## 总体架构要求

将平台相关部分拆分为独立适配器，形成类似结构：

- official QQ adapter
  - 官方 Gateway transport
  - 官方事件转换
  - 官方出站 sender
- OneBot 11 adapter
  - Reverse WebSocket server
  - OneBot 事件转换
  - OneBot API sender
- shared pipeline
  - 统一入站消息模型
  - Respond 调用
  - 统一回复编排
  - 渲染和 fallback
  - 主动推送目标路由

不要求机械套用某种 trait 设计；请根据仓库现状选择简单、可测试、可维护的结构。

不要为了抽象而抽象，但必须避免：

- OneBot JSON 类型散落到 Respond/LLM 业务层
- QQ 官方 OpenID 类型散落到 OneBot 适配器
- 两个平台复制完整 Respond 业务流程
- 一个超大的 match 同时处理两套协议

## 统一入站消息模型

新增或整理内部平台无关的入站消息结构，至少表达：

- platform：qq_official / onebot11
- account_id：当前机器人账号标识
- conversation scope：private / group
- conversation target id
- sender id
- sender display name，可选
- message id
- timestamp
- text content
- reply reference，可选
- attachments / media，可选
- mentioned_bot
- raw event，仅限诊断或扩展使用

说明：

- 当前 gateway -> LLM 的 `/v1/respond` 已使用 `platform`、`scope_key`、`user_id`、`group_id`、`message_id`、`timestamp` 等公共字段。
- 优先复用现有 `RespondRequest` 可表达的共性；如新增 gateway 内部中间模型，应限定在 gateway 内部，不要无故扩大 `/v1/respond` 公开 schema。

要求：

1. 平台 ID 使用字符串或不会丢精度的类型保存。
2. 不要把 OneBot 的 QQ 号强制转换到可能丢精度的前端 number 语义。
3. 文本提取应基于消息段数组。
4. 未支持的消息段不能导致整条事件解析失败。
5. 可将未支持段转换为可读占位，例如：
   - `[图片]`
   - `[语音]`
   - `[文件：文件名]`
   - `[表情]`
6. 图片段如有可访问 URL，应纳入现有图片输入能力；如果当前 Respond 链路暂不支持，则至少保留扩展数据且不破坏文本回复。

## 会话键要求

会话和持久化归属标识在整体语义上必须区分平台、机器人账号和作用域，避免跨平台、多账号和群私聊碰撞。

当前仓库已同时存在 `platform` 字段与 `scope_key`；实现时优先考虑复用这两类现有字段的组合语义，不要求必须把全部信息机械硬编码进单个字符串 key。

语义应类似：

- `onebot11:{self_id}:private:{user_id}`
- `onebot11:{self_id}:group:{group_id}`
- `qq-official:{account}:c2c:{openid}`
- `qq-official:{account}:group:{group_openid}`

实际编码格式可结合仓库现有 `scope_key`、`platform`、`user_id`、`group_id` 和相关持久化结构实现。

必须检查：

- session
- memory
- todo owner / scope
- pending 操作
- snapshot
- RSS / Todo 推送目标
- rate limit
- dedupe

特别注意：

- `session` / `rss` 当前主要依赖 `scope_key` 隔离；
- `todo` 当前 owner 归属优先使用 `user_id`，其次才 fallback 到 `scope_key`；
- `memory` 当前没有现成的 `scope_key` / `platform` 持久化维度，不能只改 session 就认为跨平台隔离已完成。

不要在没有迁移策略的情况下破坏现有 QQ 官方用户数据键。当前已经落库的 `private:{openid}` / `group:{group_openid}` 等 `scope_key` 需要继续可读；若需要调整持久化格式，应保持旧数据可读取，或提供明确迁移。

## OneBot 11 Reverse WebSocket

实现 OneBot 11 反向 WebSocket 服务端。

建议默认路径：

`/onebot/v11/ws`

配置至少包括：

- `QQ_MAID_ONEBOT11_ENABLED`
- `QQ_MAID_ONEBOT11_HOST`
- `QQ_MAID_ONEBOT11_PORT`
- `QQ_MAID_ONEBOT11_PATH`
- `QQ_MAID_ONEBOT11_ACCESS_TOKEN`
- `QQ_MAID_ONEBOT11_MAX_MESSAGE_BYTES`
- `QQ_MAID_ONEBOT11_REQUEST_TIMEOUT_SECONDS`

具体命名可根据现有配置风格调整，但配置示例和 README 必须同步。

当前官方 QQ 凭据主变量名为 `QQ_BOT_APP_ID` / `QQ_BOT_APP_SECRET`，`QQ_APPID` / `QQ_SECRET` 仅作兼容；新增平台开关时不要把旧变量名重新写回主文档。

鉴权要求：

1. 配置 Token 后必须验证连接请求。
2. 兼容常见的 `Authorization: Bearer <token>`。
3. 如 NapCat/Lagrange 实际还使用其他标准位置，可在不降低安全性的前提下兼容。
4. 日志不能输出完整 Token。
5. 默认监听地址优先使用 `127.0.0.1`，避免无意暴露公网。
6. 如果用户配置监听公网但未设置 Token，应输出明显警告；是否拒绝启动由项目现有安全策略决定。

连接要求：

1. 支持多个连接。
2. 根据事件中的 `self_id` 或生命周期事件注册机器人账号。
3. 同一账号重复连接时要有明确策略，避免主动推送随机发给旧连接。
4. 连接关闭后清理该连接关联的 pending API 请求。
5. OneBot 连接失败或异常不得终止 QQ 官方 Gateway。
6. 主服务关闭时正常停止监听任务。

## OneBot 事件处理

第一版处理：

### message

支持：

- private
- group

读取字段时应对实现差异保持适度宽容，包括数字或字符串形式的 ID。

### message_sent

默认忽略，避免机器人回复再次进入处理链形成循环。

如需用于观测，只记录 debug 日志，不进入 Respond。

### meta_event

支持或识别：

- lifecycle
- heartbeat

用于更新连接和账号健康状态。

### notice / request

第一版不要求实现业务操作。

要求：

- 能解析基础 envelope
- 记录结构化 debug 日志
- 不崩溃
- 不误当消息送入 LLM

### 未知 post_type

忽略并记录限量日志，不关闭连接。

## OneBot 消息段

入站第一版至少支持：

- text
- at
- reply
- image
- face

出站第一版至少支持：

- text
- reply
- at
- image

使用 OneBot 11 消息段数组作为协议层核心格式，不要在内部依赖 CQ 码字符串重新解析。

未知消息段必须兼容，不要使用封闭 enum 导致整个消息反序列化失败。可以使用：

- 已知类型结构
- Unknown / raw Value fallback

## 群聊触发规则

默认规则：

1. 私聊消息响应。
2. 群聊只有满足以下任一条件时响应：
   - 明确 @ 当前机器人
   - 回复当前机器人发送的消息
   - 内容以项目已有命令前缀开头，例如 `/todo`、`/help`
3. 忽略当前机器人自己发送的消息。
4. 支持配置指定群无需 @ 即可响应。
5. 支持配置群白名单或黑名单，至少保留一个清晰扩展点。
6. @机器人后传给 LLM 的文本应去除仅用于触发的 @ 段。
7. 不要误删正文中对其他成员的 @。
8. 空文本且只有图片时，按现有图片输入能力处理；无法处理时返回合理提示或静默，不触发空 prompt。

配置形式可以是逗号分隔 ID、JSON 或项目已有配置方式，以可维护性为准。

## Respond 和渲染复用

OneBot 消息必须复用现有：

- RespondClient
- RespondRequest / RespondResponse
- 命令处理
- Todo
- Memory
- Weather
- RSS
- Markdown / text 双通道
- 现有图片出站类型与失败 fallback 约束

OneBot 不支持 QQ 官方 Markdown payload，因此：

1. 默认发送 `RespondResponse.text` 或现有纯文本 fallback。
2. 不要把 QQ 官方 Markdown payload 直接发到 OneBot。
3. 如果后续明确补齐 OneBot 图片出站能力，可以发送生成后的图片；当前仓库不要假设已经存在稳定可复用的“Markdown 转图片”主链路。
4. 图片发送失败时降级发送纯文本。
5. Gateway 只负责选择和发送，不在平台适配器内重新实现 Markdown 转纯文本。

## OneBot API 请求与 echo

实现通用 OneBot API 请求机制：

请求格式：

- action
- params
- echo

响应按 echo 与请求关联。

至少支持业务所需：

- `send_private_msg`
- `send_group_msg`

也可统一使用 `send_msg`，但需确认 NapCatQQ 与 Lagrange 的兼容性，并保留明确的目标类型。

要求：

1. echo 唯一。
2. pending map 并发安全。
3. 请求有超时。
4. 连接断开时所有 pending 请求返回明确错误。
5. 检查响应：
   - status
   - retcode
   - wording / message
   - data
6. 不得把 OneBot 返回失败伪装成发送成功。
7. 日志记录 action、target、retcode 和耗时，但不要泄露消息全文或 Token。
8. 如果收到无法匹配 echo 的响应，只记录 debug/warn，不关闭连接。

## 主动推送改造

检查现有 internal push server、Todo 定时提醒和 RSS 推送目标。

当前基线是：

- gateway 暴露本机 `/internal/push`
- LLM 侧 `runtime/rss/push.rs` 发送的请求体字段为 `target_type`、`target_id`、`message_type`、`text`、`fallback_text`
- 如配置 token，当前使用请求头 `X-QQ-Maid-Push-Token`
- gateway 侧目前只把 `private` / `group` 目标路由到 QQ 官方发送实现

在此基础上，将主动推送目标表示改造成平台无关目标，至少包含：

- platform
- account_id，可选或必填，按多账号策略确定
- target_type：private / group
- target_id
- 可选 reply/message metadata

要求：

1. 保持现有 QQ 官方推送 API 兼容；如需修改公开请求结构，应提供向后兼容解析。
2. 新增 OneBot 目标后，可向指定 OneBot 私聊或群聊发送；旧请求未指定 `platform` 时继续按现有 QQ 官方行为处理。
3. OneBot 账号未连接时返回明确错误，不伪造成功。
4. 多个 OneBot 账号连接时，根据 account_id 精确路由。
5. 保留现有 `X-QQ-Maid-Push-Token` 鉴权兼容；如需新增其他鉴权写法，应作为兼容扩展而不是静默替换。
6. 更新相关 README 和请求示例。

## 配置系统改造

当前 QQ 官方 credentials 不应继续无条件必填。

调整为：

- 只有启用 QQ 官方平台时，`QQ_BOT_APP_ID` / `QQ_BOT_APP_SECRET` 才必填。
- 只有启用 OneBot 11 时，OneBot 监听配置才生效。
- 至少启用一个平台，否则启动时报明确配置错误。
- 两个平台可以同时启用。

建议增加明确的平台启用配置，例如：

- `QQ_MAID_OFFICIAL_ENABLED`
- `QQ_MAID_ONEBOT11_ENABLED`

需考虑现有用户兼容：

- 未设置 `QQ_MAID_OFFICIAL_ENABLED` 且存在 `QQ_BOT_APP_ID` / `QQ_BOT_APP_SECRET`（或兼容旧变量 `QQ_APPID` / `QQ_SECRET`）时，保持原有默认行为。
- 不要导致现有部署升级后突然无法启动。

启动日志应显示：

- official enabled
- onebot11 enabled
- OneBot listen addr/path
- Token 是否已配置，只输出布尔值
- 当前连接账号数

## 去重与防循环

OneBot 去重 key 至少包含：

- platform
- self_id
- message_type
- group_id 或 user_id
- message_id

要求：

1. 复用现有去重设施或提取成平台无关形式。
2. `message` 和 `message_sent` 不得互相造成循环。
3. 重连后短时间重复上报不能重复调用 LLM。
4. 不要仅使用 message_id，避免多账号碰撞。

## 运行状态与 /ping

扩展现有 GatewayRuntimeStatus 或新增平台状态聚合。

`/ping` 或诊断信息至少能展示：

- QQ 官方 Gateway 状态
- OneBot 11 监听状态
- OneBot 当前连接数
- 已连接 self_id
- 最近 OneBot 心跳时间
- 最近收到消息时间
- 最近发送成功时间
- 最近发送失败摘要
- pending API 请求数

保持输出清晰，不要把全部底层字段无脑打印给用户。

## 并发与故障隔离

1. QQ 官方 Gateway 和 OneBot server 应作为独立长期任务运行。
2. 任一平台临时失败时，另一平台继续工作。
3. OneBot 单连接解析失败不应终止整个监听器。
4. 非法 JSON 可关闭该连接或忽略该帧，但必须有明确日志。
5. 限制单帧大小，防止异常客户端占用过多内存。
6. 避免持有锁跨 await。
7. 不要为每条消息创建无限制后台任务；使用现有并发策略或增加合理限流。
8. Respond 调用超时和发送失败应复用现有用户可见错误策略。

## 建议排查范围

请通过搜索确认实际位置，可能包括：

- `qq-maid-gateway-rs/src/app/`
- `qq-maid-gateway-rs/src/config/`
- `qq-maid-gateway-rs/src/gateway/`
- `qq-maid-gateway-rs/src/api/`
- `qq-maid-gateway-rs/src/respond/`
- `qq-maid-gateway-rs/src/render/`
- `qq-maid-gateway-rs/src/media/`
- `qq-maid-gateway-rs/src/gateway/push*`
- `qq-maid-common`
- `qq-maid-core` 的 respond request、session 和 target 相关类型
- runtime 配置模板
- README / DEVELOPMENT
- systemd / Docker / Makefile / 发布打包脚本

文件路径以仓库实际结构为准。

## 建议模块边界

仅作参考，不要求机械照搬：

- `platform/mod.rs`
- `platform/types.rs`
- `platform/official/`
- `platform/onebot11/`
  - `mod.rs`
  - `config.rs`
  - `event.rs`
  - `segment.rs`
  - `server.rs`
  - `connection.rs`
  - `client.rs`
  - `sender.rs`
- `dispatch.rs`
- `outbound.rs`

如果现有模块结构更适合渐进改造，可保留现有 `gateway` 路径，并新增 `gateway/onebot11`，但需要保持官方和 OneBot 职责清晰。

## 禁止事项

- 不要删除或替换 QQ 官方 Gateway。
- 不要把 OneBot 逻辑直接堆进现有官方 WebSocket 主循环。
- 不要在 LLM 层判断 OneBot 协议字段。
- 不要复制一整套 Todo、Weather、Memory、Help 或 RSS 业务。
- 不要依赖 CQ 码作为内部核心消息格式。
- 不要把所有 OneBot ID 强制保存为窄整数。
- 不要吞掉 OneBot API 错误。
- 不要伪造发送成功。
- 不要在日志中输出完整 Access Token。
- 不要因未知事件或未知消息段 panic。
- 不要在本次顺便实现群管理、踢人、禁言、加好友审批等无关功能。
- 不要进行与接入无关的大规模格式化或重构。
- 不要伪造测试、构建或联调结果。

## 验收标准

### 配置

1. 原有 QQ 官方配置不修改即可继续启动。
2. 仅配置 OneBot 11 时可启动，不要求 AppID/AppSecret。
3. 两个平台同时开启时都能运行。
4. 两个平台均关闭时给出明确错误。
5. Token、监听地址等配置错误有可理解提示。

### 连接

1. NapCatQQ 可成功连接反向 WebSocket。
2. Lagrange.OneBot 可成功连接反向 WebSocket。
3. 配置错误 Token 时拒绝连接。
4. OneBot 重连后能恢复收发。
5. OneBot 断线不影响 QQ 官方 Gateway。

### 私聊

1. 私聊文本进入现有 Respond 链路。
2. 回复通过 `send_private_msg` 或等价 API 发回原用户。
3. 回复失败时日志和运行状态明确记录失败。
4. 机器人自身消息不会循环触发。

### 群聊

1. @机器人时响应。
2. 回复机器人消息时响应。
3. `/todo` 等命令按现有命令规则响应。
4. 普通未触发群消息默认不响应。
5. 配置为无需 @ 的群可以响应。
6. 回复发送到正确群。
7. 多个群的 session 不串话。

### 消息段

1. text 正常提取。
2. at 当前机器人可用于触发且不会污染正文。
3. reply 信息可保留并用于回复。
4. image 不导致解析失败。
5. 未知段不导致连接或进程退出。
6. 出站可组合 reply + text。
7. 图片发送失败可降级纯文本。

### 主动推送

1. 旧 QQ 官方推送请求继续工作。
2. 可向 OneBot 私聊主动推送。
3. 可向 OneBot 群聊主动推送。
4. 指定 account_id 时路由到正确机器人账号。
5. 账号离线时返回明确失败。

### 健康状态

1. 可观察 OneBot 监听状态。
2. 可观察连接数和账号。
3. 可观察最近心跳和收发时间。
4. `/ping` 不再只显示 QQ 官方 Gateway 状态。

## 测试要求

补充单元测试，至少覆盖：

1. OneBot 配置解析及兼容默认值。
2. 官方 credentials 条件必填。
3. OneBot 私聊事件解析。
4. OneBot 群聊事件解析。
5. 字符串和数字 ID 兼容。
6. text/at/reply/image/unknown 消息段。
7. 群聊触发规则。
8. 自身消息过滤。
9. 去重 key 不跨账号碰撞。
10. API echo 请求响应关联。
11. API 超时。
12. 连接断开时 pending 请求失败。
13. OneBot API 非零 retcode 传播。
14. 主动推送目标解析和旧格式兼容。
15. Markdown/图片失败后纯文本 fallback。

增加最小集成测试或测试 WebSocket：

- 启动本地 OneBot reverse WS server
- 模拟客户端连接
- 发送 lifecycle / heartbeat
- 发送 private message
- 验证 server 发出带 echo 的 send API 请求
- 返回成功响应
- 验证整个流程完成

运行项目已有命令时，优先使用仓库根目录 `Makefile` 封装而不是自行改写流程。至少按影响范围执行：

- 修改 gateway：`make test-gateway`
- 修改 common：`make test-common`；若影响 LLM 或 gateway，再补对应模块测试
- 涉及 gateway / LLM 跨模块改动或提交前：`make test`
- 涉及诊断入口、启动方式、配置或对外连接：`make diagnose`，并按需做本地启动验证
- 如确有必要补充单独的 `cargo fmt` / `cargo test` / `cargo check` 命令，可作为辅助，但不要跳过项目既有 make 校验入口

如果某项无法运行，必须说明具体原因，不得写成通过。

## 文档要求

更新：

1. README 中的 OneBot 11 功能说明。
2. 配置模板。
3. NapCatQQ 反向 WebSocket 配置示例。
4. Lagrange.OneBot ReverseWebSocket 配置示例。
5. 私聊和群聊触发规则。
6. 主动推送目标格式。
7. 安全提示：
   - 默认监听 localhost
   - 公网监听必须使用 Token 和反向代理 TLS
8. 当前支持和暂不支持的 OneBot 功能列表。
9. 故障排查：
   - 连接不上
   - Token 错误
   - 群里不回复
   - 机器人消息循环
   - 主动推送提示账号未连接

## 完成后输出

完成后请提供：

1. 原架构和问题分析。
2. 最终平台适配架构。
3. 修改文件列表。
4. 配置项列表及默认值。
5. NapCatQQ 配置示例。
6. Lagrange.OneBot 配置示例。
7. OneBot 入站和出站支持范围。
8. 群聊触发规则。
9. 主动推送格式和兼容策略。
10. 执行的测试、构建和静态检查。
11. 每项测试的真实结果。
12. 未完成能力、已知兼容差异和后续建议。
