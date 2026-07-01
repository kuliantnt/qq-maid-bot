# openclaw-qqbot QQ API 接入实现分析报告

## 一、分析目标

本文分析腾讯官方仓库 `tencent-connect/openclaw-qqbot` 的 QQ Bot 接入实现，重点关注以下内容：

1. QQ Bot 鉴权和 API 请求如何封装。
2. 私聊、群聊和频道消息如何接收与发送。
3. Markdown、富媒体、按钮和交互事件如何实现。
4. C2C 私聊流式消息使用了哪些接口字段。
5. OpenClaw 的工具调用与 QQ 消息层如何衔接。
6. 哪些实现思路适合迁移到现有 Rust QQ Bot 项目。

本报告仅根据仓库当前代码实现进行总结，不把代码中的实验性行为视为 QQ 官方协议的完整规范。实际实现仍应以 QQ 开放平台最新接口文档和线上回包为准。

---

## 二、总体结论

`openclaw-qqbot` 并没有简单依赖一个完整的 QQ Bot SDK，而是自行实现了一套相对完整的 QQ Bot 接入层，包括：

* Access Token 获取与缓存。
* QQ OpenAPI HTTP 请求封装。
* Gateway WebSocket 和 Webhook 入站传输。
* 私聊、群聊、频道消息发送。
* Markdown 和普通文本选择。
* 图片、语音、视频和文件上传。
* C2C 私聊流式消息状态机。
* Inline Keyboard 和 Interaction 回调。
* 出站调度、消息合并、错误处理和重试。

其整体结构可以概括为：

```text
QQ 用户消息
    ↓
QQ Gateway WebSocket / Webhook
    ↓
gateway.ts 解析 QQ 事件
    ↓
转换为 OpenClaw 入站上下文
    ↓
Agent / LLM / Tool Loop
    ↓
reply-dispatcher / outbound-deliver
    ↓
api.ts 调用 QQ OpenAPI
    ↓
QQ 客户端展示回复
```

其中，QQ API、流式状态机和 OpenClaw Agent 工具循环是相互分离的，并不是把所有逻辑直接写在消息事件处理函数中。

---

## 三、核心模块划分

仓库中的关键模块如下。

### 3.1 `src/api.ts`

负责 QQ HTTP API 的底层封装，包括：

* Access Token 获取和刷新。
* HTTP 请求头构造。
* 超时控制。
* 错误码和响应解析。
* 普通文本和 Markdown 发送。
* 媒体上传和媒体消息发送。
* 流式消息 API 调用。
* Interaction ACK。
* 大文件分片上传。

这是整个插件与 QQ OpenAPI 直接交互的核心文件。

### 3.2 `src/gateway.ts`

负责入站消息和 Gateway 生命周期，包括：

* 建立 QQ Gateway WebSocket。
* 接收并解析 QQ 事件。
* 区分私聊、群聊、频道和交互事件。
* 消息策略和权限判断。
* 调用 OpenClaw Runtime。
* 创建回复上下文和流式控制器。
* 把 OpenClaw 输出交给出站层。

### 3.3 `src/streaming.ts`

负责 C2C 私聊流式消息的完整生命周期，包括：

* 流式状态管理。
* 累计全文更新。
* 节流发送。
* 首包建立流式会话。
* 后续更新复用 `stream_msg_id`。
* 最终发送结束状态。
* 富媒体打断和重新建立流。
* 回调串行化。
* 多来源回调互斥。
* 回复边界检测。

### 3.4 `src/outbound-deliver.ts`

负责把 OpenClaw 输出转换成 QQ 可发送内容，例如：

* 普通文本。
* Markdown。
* 图片标签。
* 文件标签。
* 语音或视频。
* 混合文本和富媒体输出。

### 3.5 `src/reply-dispatcher.ts`

负责统一出站回复调度，包括：

* Token 失效后的重试。
* 不同消息目标的路由。
* 结构化输出分发。
* 错误提示。
* 普通回复与其他发送方式的协调。

### 3.6 `src/types.ts`

集中定义 QQ 相关数据结构，例如：

* C2C 消息事件。
* 群消息事件。
* 附件结构。
* Interaction 事件。
* Keyboard。
* WebSocket Payload。
* 流式消息请求体和状态常量。

---

## 四、鉴权实现

## 4.1 Token 获取接口

仓库通过以下接口获取 QQ Bot Access Token：

```text
POST https://bots.qq.com/app/getAppAccessToken
```

请求体为：

```json
{
  "appId": "机器人 AppID",
  "clientSecret": "机器人 Client Secret"
}
```

返回结果包含：

```json
{
  "access_token": "...",
  "expires_in": 7200
}
```

后续请求 QQ OpenAPI 时，统一携带：

```http
Authorization: QQBot <access_token>
Content-Type: application/json
```

QQ API 默认根地址为：

```text
https://api.sgroup.qq.com
```

代码也允许通过环境变量覆盖 API 根地址和 Token 地址，便于测试或私有环境部署。

## 4.2 Token 缓存设计

Token 缓存不是单个全局变量，而是按 `appId` 保存：

```text
Map<appId, TokenCache>
```

这样可以同时运行多个 QQ Bot 账号，而不会发生不同机器人之间 Token 串用的问题。

此外，仓库采用类似 singleflight 的并发控制：

* 当一个 `appId` 的 Token 已过期时，首个请求负责刷新。
* 同一时间内其他请求复用同一个刷新 Promise。
* 刷新完成或失败后清理进行中的 Promise。
* 不会因为并发消息同时触发多次 Token 请求。

这一点对于多账号和高并发场景非常重要。

---

## 五、HTTP API 封装

所有 QQ HTTP 请求统一经过 `apiRequest()`。

请求头固定包括：

```http
Authorization: QQBot <token>
Content-Type: application/json
User-Agent: QQBotPlugin/...
```

请求层还实现了：

* 默认 30 秒超时。
* 文件上传 120 秒超时。
* `AbortController` 主动取消。
* 请求和回包日志。
* `x-tps-trace-id` 记录。
* HTML 错误页识别。
* HTTP 错误和 QQ 业务错误区分。
* JSON 解析失败保护。
* 业务错误码 `code` 和 `err_code` 提取。

仓库定义了专门的 `ApiError`，保存：

* HTTP 状态码。
* 请求路径。
* QQ 业务错误码。
* QQ 原始错误消息。

因此，上层不仅知道请求失败，还能根据具体 QQ 错误码决定是否重试或展示特殊提示。

---

## 六、入站消息接入

## 6.1 Gateway 获取

插件首先调用：

```text
GET /gateway
```

获取 WebSocket 地址。

随后使用 WebSocket 长连接接收 QQ 事件。

WebSocket 统一负载结构为：

```ts
interface WSPayload {
  op: number;
  d?: unknown;
  s?: number;
  t?: string;
}
```

主要字段含义：

* `op`：Gateway 操作码。
* `d`：事件内容。
* `s`：事件序号。
* `t`：事件类型名称。

插件同时支持两种入站模式：

```text
websocket
webhook
```

默认以 WebSocket 为主，也可以配置为 Webhook。

---

## 七、C2C 私聊事件

私聊事件核心结构包括：

```ts
interface C2CMessageEvent {
  author: {
    id: string;
    union_openid: string;
    user_openid: string;
  };
  content: string;
  id: string;
  timestamp: string;
  attachments?: MessageAttachment[];
  message_type?: number;
  msg_elements?: MsgElement[];
}
```

关键字段用途如下：

| 字段                   | 用途          |
| -------------------- | ----------- |
| `author.user_openid` | 私聊回复目标      |
| `id`                 | 当前用户消息 ID   |
| `content`            | 文本内容        |
| `attachments`        | 图片、语音、文件等附件 |
| `message_type`       | 消息类型        |
| `msg_elements`       | 引用消息或结构化元素  |

被动回复时，事件中的 `id` 会作为发送接口请求体里的 `msg_id`。

---

## 八、群聊事件

群聊事件中，主要字段包括：

```ts
interface GroupMessageEvent {
  author: {
    id: string;
    member_openid: string;
    username?: string;
  };
  content: string;
  id: string;
  group_id: string;
  group_openid: string;
  mentions?: Mention[];
  attachments?: MessageAttachment[];
}
```

关键字段用途：

| 字段                     | 用途         |
| ---------------------- | ---------- |
| `group_openid`         | 群消息发送目标    |
| `author.member_openid` | 群内发送用户     |
| `id`                   | 当前用户消息 ID  |
| `mentions`             | 判断是否 @ 机器人 |
| `content`              | 群消息正文      |
| `attachments`          | 群消息附件      |

插件在真正调用 Agent 之前，还会执行：

* 群白名单判断。
* 是否需要 @ 机器人。
* 是否忽略只 @ 其他人的消息。
* 群级 Tool Policy。
* 群历史消息缓存。
* 群独立 Prompt。

说明其 QQ 层不仅是简单收发接口，还承担了场景策略和权限边界。

---

## 九、普通文本与 Markdown 发送

## 9.1 私聊发送接口

```text
POST /v2/users/{user_openid}/messages
```

## 9.2 群聊发送接口

```text
POST /v2/groups/{group_openid}/messages
```

## 9.3 普通文本请求体

```json
{
  "content": "回复内容",
  "msg_type": 0,
  "msg_seq": 123,
  "msg_id": "用户原始消息 ID"
}
```

## 9.4 Markdown 请求体

```json
{
  "markdown": {
    "content": "**回复内容**"
  },
  "msg_type": 2,
  "msg_seq": 123,
  "msg_id": "用户原始消息 ID"
}
```

仓库通过运行时配置 `markdownSupport` 决定使用哪种结构：

```ts
{
  markdown: { content },
  msg_type: 2,
  msg_seq
}
```

或者：

```ts
{
  content,
  msg_type: 0,
  msg_seq
}
```

私聊和群聊共用同一个消息体构造函数，仅发送路径不同。

---

## 十、`msg_id` 与 `msg_seq`

普通被动回复会携带：

```json
{
  "msg_id": "触发消息 ID",
  "msg_seq": 生成的消息序号
}
```

主动消息通常不携带 `msg_id`。

仓库当前生成 `msg_seq` 的方式不是严格按消息递增，而是通过：

```text
当前时间戳低位 XOR 随机数
```

最终限制在 `0~65535`。

这意味着普通消息发送中的 `msg_seq` 更偏向请求去重或冲突区分，而不是一个全局连续序号。

需要注意，普通消息的 `msg_seq` 语义和流式消息不同。

在流式场景中：

* 同一条流式会话的 `msg_seq` 必须固定。
* 不能每个 chunk 都重新生成。
* 真正递增的是 `index`。

---

## 十一、C2C 流式消息实现

## 11.1 使用范围

仓库配置中明确说明：

```text
仅 C2C 私聊支持流式消息 API
```

流式功能默认关闭，需要显式开启。

群聊仍然使用普通消息发送，而不是流式更新。

## 11.2 流式请求字段

仓库定义的流式请求体如下：

```ts
interface StreamMessageRequest {
  input_mode: "replace";
  input_state: 1 | 10;
  content_type: "markdown";
  content_raw: string;
  event_id: string;
  msg_id: string;
  stream_msg_id?: string;
  msg_seq: number;
  index: number;
}
```

各字段语义如下。

### `input_mode`

```text
replace
```

表示当前发送的 `content_raw` 会替换整条流式消息内容。

因此每次发送的是累计全文，而不是仅发送新增 token。

### `input_state`

```text
1
```

表示正文生成中。

```text
10
```

表示生成完成，是流式消息终结状态。

### `content_type`

仓库固定使用：

```text
markdown
```

### `content_raw`

当前要展示的完整累计文本。

例如 LLM 依次生成：

```text
晚
晚上
晚上好
```

那么三次请求的 `content_raw` 分别是：

```text
晚
晚上
晚上好
```

而不是：

```text
晚
上
好
```

### `event_id`

本轮 QQ 入站事件 ID。

### `msg_id`

触发本轮回复的原始消息 ID。

### `stream_msg_id`

第一次发送时不携带。

QQ 返回流式消息 ID 后，后续所有更新都携带相同的 `stream_msg_id`。

### `msg_seq`

同一条流式消息生命周期内保持不变。

新的流式会话才重新生成。

### `index`

从 0 开始，每次成功发送后递增。

新的流式会话重新从 0 开始。

---

## 十二、正确的流式调用示例

假设用户发送“晚上好”，机器人流式生成“晚上好呀”。

### 第一次发送

```json
{
  "input_mode": "replace",
  "input_state": 1,
  "content_type": "markdown",
  "content_raw": "晚",
  "event_id": "EVENT_ID",
  "msg_id": "SOURCE_MSG_ID",
  "msg_seq": 5,
  "index": 0
}
```

QQ 返回：

```json
{
  "stream_msg_id": "STREAM_ID"
}
```

### 第二次发送

```json
{
  "input_mode": "replace",
  "input_state": 1,
  "content_type": "markdown",
  "content_raw": "晚上",
  "event_id": "EVENT_ID",
  "msg_id": "SOURCE_MSG_ID",
  "stream_msg_id": "STREAM_ID",
  "msg_seq": 5,
  "index": 1
}
```

### 最终发送

```json
{
  "input_mode": "replace",
  "input_state": 10,
  "content_type": "markdown",
  "content_raw": "晚上好呀",
  "event_id": "EVENT_ID",
  "msg_id": "SOURCE_MSG_ID",
  "stream_msg_id": "STREAM_ID",
  "msg_seq": 5,
  "index": 2
}
```

关键约束：

```text
stream_msg_id 固定
msg_seq 固定
index 递增
content_raw 是累计全文
最后必须 input_state=10
```

---

## 十三、流式控制器设计

仓库没有让每个 LLM token 直接请求 QQ API，而是增加了独立的 `StreamingController`。

其设计目标包括：

1. 控制 QQ API 请求频率。
2. 严格保持流式状态顺序。
3. 避免多个回调源重复发送。
4. 识别 OpenClaw 是否开始了一段新回复。
5. 在工具调用和富媒体输出期间保持消息边界。
6. 保证最终结束状态只发送一次。

## 13.1 流式阶段

状态机包括：

```text
idle
streaming
completed
aborted
```

允许的主要转换为：

```text
idle → streaming
streaming → completed
streaming → aborted
idle → aborted
```

终态不能再次恢复。

## 13.2 节流策略

代码中的默认流式节流参数为：

```text
默认间隔：500ms
最小间隔：300ms
长间隔阈值：2000ms
长间隔后聚合窗口：300ms
```

这样做的目的不是追求逐 token 发送，而是在实时性和 QQ API 请求频率之间取平衡。

## 13.3 Flush 互斥

控制器保证：

* 同一时间只执行一个 flush。
* flush 过程中收到新文本，不并行发送。
* 当前 flush 完成后，再立即安排下一次。
* 可以等待进行中的 flush 完成。
* 完成或中止后清理定时器。

这可以防止：

* `index` 乱序。
* 后发请求先返回。
* 最终状态和中间状态交叉。
* 同一流出现并发更新。

## 13.4 全文前缀检测

控制器不会只根据字符串长度判断是不是同一条回复，而是判断：

```text
新文本是否以上一次文本为前缀
```

如果新文本不是上一份文本的自然延续，就认为发生了回复边界。

例如：

```text
上次：正在查询天气
本次：北京今天晴朗
```

第二段并不以上一段为前缀，可能意味着：

* 工具调用前后生成了不同回复段。
* 框架触发了新的 deliver。
* 最终答案开始了新阶段。

仓库会将其视为新的回复边界，而不是直接拿新文本覆盖旧文本。

代码特别强调：

```text
不 trim
不 strip
不随意修改原始文本
```

否则空格、换行或 Markdown 处理可能造成前缀判断错误。

## 13.5 回调来源互斥

OpenClaw 可能从多个回调入口产生输出。

控制器记录第一个实际到达的回调来源：

```text
firstCallbackSource
```

后续不同来源的重复回调会被忽略。

这一设计用于避免：

* partial reply 和 final reply 同时发送。
* 两条框架输出通道重复投递。
* 一次回答被当作两条消息发送。
* 最终文本再次拼到已经结束的流后面。

---

## 十四、富媒体发送

## 14.1 媒体类型

仓库定义：

```text
1 = 图片
2 = 视频
3 = 语音
4 = 文件
```

## 14.2 上传接口

私聊：

```text
POST /v2/users/{openid}/files
```

群聊：

```text
POST /v2/groups/{group_openid}/files
```

可以使用公网 URL：

```json
{
  "file_type": 1,
  "url": "https://example.com/image.png",
  "srv_send_msg": false
}
```

也可以使用 Base64：

```json
{
  "file_type": 1,
  "file_data": "BASE64_DATA",
  "srv_send_msg": false
}
```

返回值中关键字段为：

```text
file_info
```

## 14.3 媒体消息发送

上传完成后，再调用消息接口：

```json
{
  "msg_type": 7,
  "media": {
    "file_info": "上传返回的 file_info"
  },
  "msg_seq": 123,
  "msg_id": "原消息 ID"
}
```

这说明富媒体不是直接把图片 URL 塞进普通 Markdown，而是：

```text
先上传
再使用 file_info 发送媒体消息
```

## 14.4 大文件分片上传

对于大文件，仓库还实现了：

```text
upload_prepare
    ↓
使用预签名 URL 上传各分片
    ↓
upload_part_finish
    ↓
files 完成上传
```

上传准备接口会返回：

* `upload_id`
* `block_size`
* `parts`
* 可选并发数
* 可选重试超时

同时针对部分 QQ 业务错误码实现了持续重试机制。

---

## 十五、流式与富媒体的组合

QQ 流式接口只负责 Markdown 文本，不能把所有媒体结果直接作为同一流持续更新。

仓库的处理方式是：

```text
文本流式输出
    ↓
检测到媒体标签
    ↓
结束当前流，发送 input_state=10
    ↓
同步发送媒体
    ↓
如媒体后仍有文本，再开启新的流式会话
```

因此一次 Agent 回复可能对应：

```text
流式文本 A
图片
流式文本 B
文件
流式文本 C
```

每一段文本都可能是独立的流式会话。

新的流式会话需要：

* 新的 `msg_seq`
* `index` 重置为 0
* 首次不带 `stream_msg_id`
* QQ 返回新的 `stream_msg_id`

这比在同一流中强行混入媒体更稳定。

---

## 十六、Inline Keyboard 与交互事件

消息体可以携带：

```json
{
  "keyboard": {
    "content": {
      "rows": [
        {
          "buttons": []
        }
      ]
    }
  }
}
```

按钮 Action 类型包括：

```text
0 = 跳转链接
1 = 回调型
2 = 指令型
3 = mqqapi
```

用户点击回调型按钮后，Gateway 收到：

```text
INTERACTION_CREATE
```

插件随后调用：

```text
PUT /interactions/{interaction_id}
```

请求体包含：

```json
{
  "code": 0
}
```

也可以附带 `data`。

如果不及时 ACK，QQ 客户端中的按钮可能持续显示加载状态。

---

## 十七、工具调用与 QQ API 的关系

OpenClaw 的 LLM 工具调用不是 QQ Bot API 的能力。

QQ 接口中不存在类似以下字段：

```text
tool_call
function_call
tool_result
```

实际链路是：

```text
QQ 用户消息
    ↓
OpenClaw Agent
    ↓
LLM 产生 tool_call
    ↓
OpenClaw 执行工具
    ↓
工具结果回填给 LLM
    ↓
LLM 继续生成最终答案
    ↓
QQ 插件发送文本、流式或媒体结果
```

因此 QQ 插件只需要处理：

* 入站消息。
* 回复目标。
* 消息引用。
* Markdown。
* 流式更新。
* 富媒体。
* 按钮。
* 最终发送边界。

Tool Loop 的状态管理由 OpenClaw Runtime 负责。

---

## 十八、为什么工具调用容易造成重复消息

工具调用通常会让一次回复经历多个输出阶段：

```text
模型前置说明
    ↓
工具调用
    ↓
工具结果
    ↓
模型最终回答
```

如果 QQ 插件没有统一回复生命周期，可能出现：

1. 工具前文本开启一条流。
2. 工具结果后框架重新触发一条回复。
3. 最终回答再次走普通发送。
4. 同一答案发两次。
5. 第二次内容拼接到第一次流尾部。
6. 已完成流仍继续收到 chunk。
7. `msg_seq` 或 `index` 被错误重置。

`openclaw-qqbot` 通过以下机制降低风险：

* 独立 `StreamingController`。
* 回调严格串行。
* 首个回调来源锁定。
* 回复边界前缀检测。
* 终态不可再次写入。
* 流式和普通发送统一由外层调度。
* 富媒体先结束当前流，再单独发送。
* 最终结束状态只允许发送一次。

---

## 十九、可借鉴的架构设计

对于现有 Rust QQ Bot 项目，建议参考以下分层。

## 19.1 QQ API Client

职责仅限：

* Token 管理。
* HTTP 请求。
* QQ 请求体和回包类型。
* 错误码解析。
* 超时和重试。
* Trace ID 日志。

不应该在这一层处理 LLM 或工具循环。

建议抽象：

```text
QqApiClient
├── get_access_token
├── get_gateway_url
├── send_c2c_message
├── send_group_message
├── send_c2c_stream
├── upload_c2c_media
├── upload_group_media
└── acknowledge_interaction
```

## 19.2 StreamingController

职责仅限：

* 本轮流式消息生命周期。
* `stream_msg_id`
* `msg_seq`
* `index`
* `input_state`
* 累计全文。
* 节流。
* flush 串行。
* 结束和中止。
* 回复边界。

建议状态：

```text
Idle
Active
Completed
Aborted
```

需要保存：

```text
source_msg_id
event_id
stream_msg_id
msg_seq
next_index
last_raw_text
last_sent_text
phase
```

## 19.3 Reply Dispatcher

职责是决定：

```text
本轮输出究竟走：
- C2C 流式
- 普通 C2C
- 普通群聊
- Markdown
- 纯文本
- 图片
- 文件
- 语音
- 错误兜底
```

并保证：

```text
一次 Agent 回复只有一个最终出口
```

Dispatcher 不应让 partial、final、tool result 和普通发送各自独立调用 QQ API。

## 19.4 Agent / Tool Loop

Agent 层负责：

* LLM 请求。
* Tool Call。
* Tool Result。
* 多轮模型调用。
* 最终答案生成。

它只向 Dispatcher 报告：

```text
PartialText
FinalText
Media
ToolStatus
Error
```

不直接操作 `stream_msg_id` 或 `index`。

---

## 二十、对现有项目的直接启示

### 20.1 LLM Stream 不等于 QQ Stream

错误理解：

```text
LLM 每产生一个 token
→ 立即调用一次 QQ API
```

更合理的实现：

```text
LLM 输出累计
→ 本地节流聚合
→ QQ replace 模式发送累计全文
→ 最后发送 input_state=10
```

QQ 的流式 API 是一种消息更新协议，不是原样转发 LLM SSE。

### 20.2 同一流中 `msg_seq` 应固定

同一流式会话中：

```text
msg_seq 固定
stream_msg_id 固定
index 递增
```

不应每个 chunk 都重新生成 `msg_seq`。

### 20.3 只有成功后才能提交状态

推荐顺序：

```text
构造 index=N 请求
    ↓
调用 QQ API
    ↓
QQ 返回成功
    ↓
提交 next_index=N+1
```

如果请求失败，不应提前递增 `index`。

### 20.4 最终状态不能再普通补发

一旦流式成功结束：

```text
input_state=10
```

本轮最终答案就已经发送完成。

上层不应再把相同最终文本走一次普通消息发送。

只有当流式从未成功建立，或明确执行 fallback 时，才能使用普通发送。

### 20.5 不要随意修改累计文本

流式前缀比较依赖原始文本稳定递增。

因此在状态机内部不应随意：

* `trim`
* 去掉换行。
* 修改 Markdown。
* 自动增加前后缀。
* 重写空白字符。

展示格式转换应在进入流状态前统一完成。

### 20.6 工具调用前后需要回复边界

工具调用可能让模型输出从：

```text
我正在查询
```

变成：

```text
查询结果如下
```

第二段不一定是第一段的前缀。

系统需要决定：

* 拼接成同一逻辑回复。
* 结束旧流并建立新流。
* 或只展示最终答案。

不能简单把第二段当作旧流的累计全文。

---

## 二十一、风险和注意事项

### 21.1 仓库实现不等于协议最终规范

该仓库虽然位于腾讯组织下，但代码仍可能包含：

* 内测接口。
* 非公开字段。
* 特定版本兼容逻辑。
* 临时错误码。
* 尚未正式文档化的行为。

尤其是流式 API，应结合实际 QQ 开放平台权限和线上回包验证。

### 21.2 流式仅限 C2C

仓库明确限制流式用于私聊。

如果群聊强行使用相同流式接口，可能直接返回不支持或字段错误。

### 21.3 Markdown 客户端兼容性

即使接口接受 Markdown，不同 QQ 客户端版本也可能存在：

* PC 和手机表现不同。
* 首包解析不同。
* 特殊字符显示异常。
* 空 Markdown 或短首包异常。
* 引用和 Markdown 同时使用时行为不同。

因此发送协议正确不代表所有客户端表现完全一致。

### 21.4 `msg_seq` 实现值得验证

仓库普通消息使用随机混合方式生成 `msg_seq`，不代表所有业务场景都必须照抄。

现有项目如果已经有稳定的同消息单调序号机制，应优先根据实际 QQ 回包验证，不宜仅因为该仓库如此实现就直接替换。

### 21.5 多回调源重复问题

OpenClaw 通过 `firstCallbackSource` 解决部分重复回调问题。

Rust 项目如果同时存在：

* stream callback
* final callback
* dispatcher fallback
* tool completion callback
* error fallback

也应明确每个回调的所有权，而不是让多个回调都可以发送最终消息。

---

## 二十二、建议在 Rust 项目中的排查顺序

建议优先排查以下链路：

### 第一阶段：确认 QQ API 字段

检查：

```text
stream_msg_id 是否首包后保存
msg_seq 是否同一流固定
index 是否成功后递增
input_state=10 是否只发一次
content_raw 是否累计全文
event_id 和 msg_id 是否来自正确事件
```

### 第二阶段：确认发送所有权

检查：

```text
谁负责第一次流式发送
谁负责中间更新
谁负责最终 state=10
谁负责流式失败 fallback
谁负责普通最终消息
```

确保每种结果只有一个负责人。

### 第三阶段：确认 Agent 回调来源

检查：

```text
partial reply
final reply
tool result
idle
completion
error fallback
```

是否可能同时触发 QQ 出站。

### 第四阶段：确认文本边界

记录每次回调：

```text
raw_text
previous_raw_text
is_prefix
callback_source
stream_phase
stream_msg_id
msg_seq
index
```

通过日志确认第二次输出到底是：

* 同一回复累计文本。
* 工具后的新回复段。
* 重复 final。
* 普通 fallback。
* 框架二次 deliver。

---

## 二十三、建议的日志字段

为了排查流式和工具调用问题，建议每次 QQ 流请求记录：

```text
task_id
user_id
source_message_id
event_id
callback_source
stream_phase
stream_msg_id_present
msg_seq
index
input_state
content_chars
previous_content_chars
prefix_match
request_started
request_succeeded
state_committed
```

最终结束时记录：

```text
final_owner
final_delivery_mode
stream_completed
normal_send_skipped
fallback_used
fallback_reason
```

这样可以直接判断一次回答是否出现了两个最终出口。

---

## 二十四、最终判断

`openclaw-qqbot` 最值得参考的并不是单个 QQ API 调用，而是它将以下三个问题进行了分层：

```text
QQ API Client
    负责协议字段和 HTTP

StreamingController
    负责流式状态机

Reply Dispatcher
    负责一次回复的发送所有权
```

而 OpenClaw 的 Tool Loop 保持在更上层，不与 QQ API 字段耦合。

对现有 Rust QQ Bot 项目而言，优先借鉴以下几点：

1. 同一流固定 `msg_seq`。
2. 使用 `index` 表示更新顺序。
3. `content_raw` 发送累计全文。
4. 首包后保存 `stream_msg_id`。
5. 最终只发送一次 `input_state=10`。
6. 流式成功后禁止再次普通补发。
7. 所有回调进入统一串行队列。
8. 为 partial、final 和 tool loop 明确输出所有权。
9. 使用前缀匹配识别同一回复或新回复边界。
10. 富媒体出现时先结束文本流，再单独发送媒体。

从设计上看，这套实现说明 QQ 流式接入的难点并不主要在 HTTP 字段，而在于：

```text
如何把 LLM、工具调用、框架回调和 QQ 流状态统一成一次可靠且唯一的回复生命周期。
```

