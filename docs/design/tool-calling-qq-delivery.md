# Tool Calling 与 QQ 消息发送衔接设计说明

## 1. 背景

本文只描述当前仓库里的**真实实现**，不提出大规模重构方案。

当前链路已经分成三层：

- `qq-maid-gateway-rs/`：QQ 官方消息接入、私聊/群聊出站发送、C2C 流式发送状态机、主动推送落地。
- `qq-maid-core/`：会话、命令、记忆、Todo、普通聊天、Tool Calling 入口判断，以及具体业务 Tool 适配。
- `qq-maid-llm/`：Provider 协议、OpenAI Responses Tool Loop、ToolRegistry 和工具调用执行框架。

这三层当前总体边界是清楚的：**Core/LLM 负责“是否调用工具、如何执行工具、如何生成最终回复”；Gateway 负责“把 QQ 入站消息变成 CoreRequest，再把 CoreResponse 变成 QQ 出站 payload”**。关键文件见：

- Gateway 入口：`qq-maid-gateway-rs/src/gateway/protocol.rs::handle_envelope`
- Gateway 到 Core 桥接：`qq-maid-gateway-rs/src/respond.rs::respond_c2c` / `respond_group`
- Core 聊天与 Tool Calling 入口：`qq-maid-core/src/runtime/respond/chat_flow.rs::handle_chat`
- LLM Tool Loop：`qq-maid-llm/src/provider/openai/tool_loop.rs::openai_responses_tool_loop`
- QQ payload 构造：`qq-maid-gateway-rs/src/api.rs`、`qq-maid-gateway-rs/src/markdown.rs`、`qq-maid-gateway-rs/src/media.rs`

> 说明：本次额外检索了公开 `Tencent/OpenClaw` 主仓库，但未直接定位到可对读的 QQ 插件源码，因此下面“与 OpenClaw QQ 插件的对比”只借鉴其公开可见的**通用分层思路**，不声称已经完成对其 QQ 插件实现的逐行对比。

---

## 2. 当前实际调用链路

### 2.1 从收到 QQ 消息到进入 Core

#### 私聊 C2C 链路

1. QQ Gateway WebSocket 收到事件后，在 `qq-maid-gateway-rs/src/gateway/protocol.rs::handle_envelope` 按事件类型分发。`C2C_MESSAGE_CREATE` 先经 `parse_c2c_message` 解析成 `C2cMessage`。  
   参考：`qq-maid-gateway-rs/src/gateway/protocol.rs::handle_envelope`、`qq-maid-gateway-rs/src/gateway/event.rs::parse_c2c_message`

2. 私聊消息先进入聚合器 `MessageAggregator`。聚合器会通过 `RespondClient::classify_c2c` 询问 Core：这条物理消息是普通聊天，还是 pending/命令等必须立即处理的消息。  
   参考：`qq-maid-gateway-rs/src/gateway/aggregator/actor.rs::classify`、`qq-maid-gateway-rs/src/respond.rs::classify_c2c`、`qq-maid-core/src/runtime/respond.rs::RustRespondService::classify_inbound`

3. 聚合后的逻辑消息进入 `MessageDispatcher`，按 `scope_key` 串行调度，同 scope 串行、不同 scope 并发。私聊 scope 由 Gateway 直接复用 Core 规则 `private:{user_openid}`。  
   参考：`qq-maid-gateway-rs/src/gateway/dispatcher.rs::MessageDispatcherHandle::enqueue_c2c`、`qq-maid-gateway-rs/src/respond.rs::scope_key_from_c2c_message`

4. worker 调用 `qq-maid-gateway-rs/src/gateway/c2c.rs::handle_c2c_message`。这里会：
   - 做 reply cache 回填 `resolve_signals`
   - 处理本地 `/ping`
   - 调用 `RespondClient::respond_c2c`
   - 根据 Core 返回结果选择普通发送或 C2C 流式发送。  
   参考：`qq-maid-gateway-rs/src/gateway/c2c.rs::handle_c2c_message`

5. `RespondClient::respond_c2c` 把 `C2cMessage` 映射成 `CoreRequest`：
   - `actor.user_id = Some(user_openid)`
   - `conversation = Private { peer_id = user_openid }`
   - `platform = QqOfficial`  
   参考：`qq-maid-gateway-rs/src/respond.rs::core_request_from_c2c_message`

#### 群聊链路

1. `GROUP_AT_MESSAGE_CREATE` / `GROUP_MESSAGE_CREATE` 在 `handle_envelope` 中进入 `parse_group_message`。  
   参考：`qq-maid-gateway-rs/src/gateway/protocol.rs::handle_envelope`、`qq-maid-gateway-rs/src/gateway/event.rs::parse_group_message`

2. 群消息不走私聊聚合分类，而是直接进入 dispatcher。dispatcher 使用 `group:{group_openid}` 作为 scope。  
   参考：`qq-maid-gateway-rs/src/gateway/dispatcher.rs::MessageDispatcherHandle::enqueue_group`、`qq-maid-gateway-rs/src/respond.rs::scope_key_from_group_message`

3. worker 调用 `qq-maid-gateway-rs/src/gateway/group.rs::handle_group_message`，做群过滤、冷却、Core 调用和群回复发送。  
   参考：`qq-maid-gateway-rs/src/gateway/group.rs::handle_group_message`

4. `RespondClient::respond_group` 把群消息映射成：
   - `actor.user_id = member_openid`
   - `conversation = Group { group_id = group_openid }`
   - **会话 scope 只按群隔离，不按成员拆分**。  
   参考：`qq-maid-gateway-rs/src/respond.rs::core_request_from_group_message`

### 2.2 Core 内部：从 CoreRequest 到 Tool Loop / 普通聊天

1. `CoreHandle::respond` 先把 `CoreRequest` 转成 `RespondRequest`，再决定走 `Complete` 还是 `Stream`。关键判断在：
   - `should_stream_respond(req)`
   - `should_use_tool_calling(state, req)`  
   私聊普通聊天若启用 Tool Calling，则**不会走 CoreResponseStream**，而是直接走完整 Tool Loop 的 `Complete` 路径。  
   参考：`qq-maid-core/src/service.rs::CoreHandle::respond`、`should_stream_respond`、`should_use_tool_calling`

2. `RustRespondService::respond` 先做 pending、会话命令、翻译、天气、列车、搜索、RSS、Todo、Memory 等业务分流；只有兜底普通聊天才进入 `handle_chat`。  
   参考：`qq-maid-core/src/runtime/respond.rs::RustRespondService::respond`

3. `handle_chat` 判断是否进入 Tool Loop：
   - `self.tool_calling_enabled`
   - 非群聊 `!is_group_chat`
   - provider 支持 `supports_tool_calling(None)`  
   满足时调用 `LlmChatService::respond_with_tools`；否则走普通 `respond`。  
   参考：`qq-maid-core/src/runtime/respond/chat_flow.rs::handle_chat`

4. `LlmChatService::respond_with_tools` 会：
   - 构建聊天 messages
   - 生成 `ToolContext`
   - 组装 `ToolChatRequest`
   - 调用 `provider.chat_with_tools(...)`  
   参考：`qq-maid-core/src/runtime/respond/llm_service.rs::respond_with_tools`

5. OpenAI provider 当前的 Tool Loop 真正落在 `qq-maid-llm/src/provider/openai/tool_loop.rs::openai_responses_tool_loop`。它负责：
   - 请求 OpenAI Responses
   - 解析 `function_call`
   - 通过 `ToolRegistry::execute_json` 执行本地白名单工具
   - 把结果作为 `function_call_output` 再回送模型
   - 直到拿到最终文本回复。  
   参考：`qq-maid-llm/src/provider/openai/tool_loop.rs::openai_responses_tool_loop`

6. `ToolRegistry::execute_json` 是当前服务端工具执行入口。它只接受显式注册的工具，并附带超时、输出长度限制。  
   参考：`qq-maid-llm/src/tool.rs::ToolRegistry::execute_json`

7. 当前 Core 在构造 `RustRespondService` 时只注册了一个 `WeatherTool`。  
   参考：`qq-maid-core/src/runtime/respond.rs::RustRespondService::new`

8. `WeatherTool` 内部没有 QQ 逻辑，只是把模型参数转成 `WeatherRequest`，再复用已有天气执行器。  
   参考：`qq-maid-core/src/runtime/tools/weather.rs::WeatherTool::execute`

### 2.3 从 Core 返回到 QQ 发送

#### 私聊普通 complete 路径

1. Tool Loop 或普通聊天最终都先变成 `RespondResponse` / `CoreResponse`。  
   参考：`qq-maid-core/src/runtime/respond/llm_service.rs::response_from_output`、`qq-maid-core/src/service.rs::impl From<RespondResponse> for CoreResponse`

2. Gateway 在 `send_c2c_respond_response_with_sender` 中调用 `render_respond_response` 把 CoreResponse 渲染成：
   - `OutboundMessage::Text`
   - `OutboundMessage::Markdown`
   - （预留）`OutboundMessage::Image`  
   参考：`qq-maid-gateway-rs/src/gateway/c2c.rs::send_c2c_respond_response_with_sender`、`qq-maid-gateway-rs/src/render.rs::render_respond_response`

3. `send_outbound_with_fallback` 再调用 `QqApiClient` 发送；Markdown/图片失败时 fallback 到文本。  
   参考：`qq-maid-gateway-rs/src/api.rs::send_outbound_with_fallback`

#### 私聊流式路径

1. 当 Core 返回 `CoreRespondOutput::Stream` 时，Gateway 进入 `stream_respond_c2c`。  
   参考：`qq-maid-gateway-rs/src/gateway/c2c.rs::handle_c2c_message`、`qq-maid-gateway-rs/src/gateway/stream.rs::stream_respond_c2c`

2. `stream_respond_c2c` 把 `CoreResponseEvent::TextDelta` 转成 QQ C2C Markdown 流式 payload；首帧成功后，后续只能沿同一个 stream id/index 续接。  
   参考：`qq-maid-gateway-rs/src/gateway/stream.rs::stream_respond_c2c_with_sender`、`send_stream_chunk`、`send_stream_end`

3. 如果首帧没有成功创建 QQ stream id，则在 `Completed` 阶段回退到普通回复；一旦已进入 `Active`，就不再补发第二条普通全文。  
   参考：`qq-maid-gateway-rs/src/gateway/stream.rs::stream_respond_c2c_with_sender`

#### 群聊路径

1. 群聊当前不走 QQ 增量发送。即使 Core 返回 `Stream`，Gateway 也只是消费到 `Completed(response)` 再一次性发送。  
   参考：`qq-maid-gateway-rs/src/gateway/group.rs::consume_respond_stream`

2. 群 at 回复的 `<@member_openid>` 前缀只在 Gateway 出站边界补上，不让 Core 处理 QQ 提及语法。  
   参考：`qq-maid-gateway-rs/src/gateway/group.rs::prefix_group_reply_outbound`

---

## 3. 当前 QQ 出站接口与字段

## 3.1 出站接口位置

| 能力 | 模块 | 关键函数 |
| --- | --- | --- |
| C2C 文本发送 | `qq-maid-gateway-rs/src/api.rs` | `QqApiClient::send_c2c_text` |
| C2C Markdown 发送 | `qq-maid-gateway-rs/src/api.rs` | `QqApiClient::send_c2c_markdown` |
| C2C 图片发送 | `qq-maid-gateway-rs/src/api.rs` | `QqApiClient::send_c2c_image` |
| C2C Markdown 流式发送 | `qq-maid-gateway-rs/src/api.rs` | `QqApiClient::send_c2c_markdown_stream` |
| 群文本发送 | `qq-maid-gateway-rs/src/api.rs` | `QqApiClient::send_group_text` |
| 群 Markdown 发送 | `qq-maid-gateway-rs/src/api.rs` | `QqApiClient::send_group_markdown` |
| 普通回复渲染 | `qq-maid-gateway-rs/src/render.rs` | `render_respond_response` |
| 私聊发送包装 | `qq-maid-gateway-rs/src/gateway/c2c.rs` | `send_c2c_respond_response_with_sender` |
| 群发送包装 | `qq-maid-gateway-rs/src/gateway/group.rs` | `send_group_respond_response` |
| 主动推送 | `qq-maid-gateway-rs/src/gateway/push.rs` | `GatewayPushRuntime::push` |

## 3.2 题目中各字段当前在哪里生成

### `msg_id`

- **被动私聊/群聊回复**：来自入站 `message.message_id`，在 Gateway 里组装成 `C2cReplyTarget.msg_id` / `GroupReplyTarget.msg_id`，再传给 payload builder。  
  参考：`qq-maid-gateway-rs/src/gateway/c2c.rs::send_c2c_respond_response_with_sender`、`qq-maid-gateway-rs/src/gateway/group.rs::send_group_respond_response`
- **主动推送**：没有原始入站消息，因此 `msg_id = None`。  
  参考：`qq-maid-gateway-rs/src/gateway/push.rs::send_private_push`、`send_group_push`
- **说明**：当前代码把 `msg_id` 视为“回复所绑定的原始 QQ 消息 id”，不是 Tool Loop 任务 id，也不是 C2C stream id。  
  参考：`qq-maid-gateway-rs/src/api.rs::send_c2c_markdown_stream` 注释

### `msg_seq`

- 普通发送：由 `QqApiClient::next_msg_seq()` 生成。  
  参考：`qq-maid-gateway-rs/src/api.rs::next_msg_seq`
- C2C 流式发送：由 `C2cStreamState::begin_msg_seq_attempt` 管理重试时复用/提交语义。  
  参考：`qq-maid-gateway-rs/src/api.rs::C2cStreamState::begin_msg_seq_attempt`、`commit_msg_seq_attempt`

### `msg_type`

- 文本：`0`  
  参考：`qq-maid-gateway-rs/src/api.rs::build_c2c_text_payload`、`build_group_text_payload`
- Markdown：`2`  
  参考：`qq-maid-gateway-rs/src/markdown.rs::build_c2c_markdown_payload`、`build_group_markdown_payload`
- C2C Markdown 流式：也是 `2`  
  参考：`qq-maid-gateway-rs/src/api.rs::build_c2c_markdown_stream_payload`
- C2C 图片：`7`  
  参考：`qq-maid-gateway-rs/src/media.rs::build_c2c_image_payload`
- **待确认**：这些数值当前可从本仓库 builder 和测试确认，但与 QQ 官方文档的一一对应关系仍建议在真机环境复核。  
  参考：`qq-maid-gateway-rs/src/api.rs` 测试 `c2c_text_payload_matches_qq_shape`、`c2c_markdown_stream_payload_matches_reference_shape`

### `content`

- 只在文本 payload 中作为顶层字段生成。  
  参考：`qq-maid-gateway-rs/src/api.rs::build_c2c_text_payload`、`build_group_text_payload`
- 内容来源一般是 `RespondResponse.text`，由 `render_respond_response` 或 fallback 逻辑决定。  
  参考：`qq-maid-gateway-rs/src/render.rs::render_respond_response`
- Markdown/流式路径下，正文不走顶层 `content`，而走 `markdown.content`。  
  参考：`qq-maid-gateway-rs/src/markdown.rs`、`qq-maid-gateway-rs/src/api.rs::build_c2c_markdown_stream_payload`

### `markdown`

- `RespondResponse.markdown` 在 Gateway 渲染层被包装成 `MarkdownPayload`。  
  参考：`qq-maid-gateway-rs/src/render.rs::render_respond_response`
- 普通 Markdown payload 由 `build_c2c_markdown_payload` / `build_group_markdown_payload` 生成。  
  参考：`qq-maid-gateway-rs/src/markdown.rs`
- 流式 Markdown payload 由 `build_c2c_markdown_stream_payload` 生成，内容可能是首帧正文、增量 chunk 或结束标记。  
  参考：`qq-maid-gateway-rs/src/api.rs::build_c2c_markdown_stream_payload`、`qq-maid-gateway-rs/src/gateway/stream.rs::send_stream_chunk`、`send_stream_end`

### `stream`

- 只在 **C2C Markdown 流式发送** payload 中生成。  
  参考：`qq-maid-gateway-rs/src/api.rs::C2cMarkdownStreamPayload`
- 字段内容来自 `C2cStreamState` 和发送阶段参数：
  - `state`
  - `id`
  - `index`
  - `reset`  
  参考：`qq-maid-gateway-rs/src/api.rs::build_c2c_markdown_stream_payload`
- **待确认**：`state=1/10`、`reset=false`、首帧 `id=null`、后续使用首帧返回 id 的真实平台约束，代码里有注释和测试，但仍建议真机/官方文档复核。  
  参考：`qq-maid-gateway-rs/src/api.rs` 相关注释、`qq-maid-gateway-rs/src/gateway/stream.rs` 模块注释

### `keyboard`

- 当前仓库**没有生成 `keyboard` 字段**。  
  证据：全文检索 `qq-maid-gateway-rs/src`、`qq-maid-core/src`、`qq-maid-llm/src` 未发现 QQ keyboard payload builder 或发送入口。

### 私聊 `openid`

- 入站解析为 `C2cMessage.user_openid`。  
  参考：`qq-maid-gateway-rs/src/gateway/event.rs::parse_c2c_message`
- 出站发送时写入 API 路径 `/v2/users/{user_openid}/messages`。  
  参考：`qq-maid-gateway-rs/src/api.rs::post_c2c_message`、`post_c2c_stream_message`
- Gateway 同时把它映射给 Core 的：
  - `actor.user_id`
  - 私聊会话 `peer_id`
  - scope `private:{user_openid}`  
  参考：`qq-maid-gateway-rs/src/respond.rs::core_request_from_c2c_message`、`scope_key_from_c2c_message`

### 群聊 `group_openid`

- 入站解析为 `GroupMessage.group_openid`。  
  参考：`qq-maid-gateway-rs/src/gateway/event.rs::parse_group_message`
- 出站发送时写入 API 路径 `/v2/groups/{group_openid}/messages`。  
  参考：`qq-maid-gateway-rs/src/api.rs::post_group_message`
- Gateway 同时把它映射给 Core 的：
  - `conversation.group_id`
  - scope `group:{group_openid}`  
  参考：`qq-maid-gateway-rs/src/respond.rs::core_request_from_group_message`、`scope_key_from_group_message`

## 3.3 当前 QQ 层实际保存和传递的上下文字段

当前 QQ 层为了让工具调用前后最终能把消息发回正确目标，至少保存并传递这些字段：

### 私聊消息 `C2cMessage`

- `message_id`：用于被动回复 `msg_id`
- `event_id` / `source_message_ids` / `source_event_ids`：用于去重与聚合边界
- `user_openid`：用于 QQ 发送路径、Core actor、私聊 scope
- `content`
- `reply.message_id` / `reply.content`：用于 reply 协议文本拼接
- `timestamp` / `first_message_timestamp` / `last_message_timestamp`：当前用于聚合/诊断，不直接进入 QQ payload
- `attachments`：转成 `[附件 ...]` 文本备注进入 Core 文本协议  
  参考：`qq-maid-gateway-rs/src/gateway/event.rs::C2cMessage`、`qq-maid-gateway-rs/src/respond.rs::build_respond_content`

### 群消息 `GroupMessage`

- `message_id`：用于被动回复 `msg_id`
- `group_openid`：用于群发送路径、group scope
- `member_openid`：用于 Core actor，以及群 at 回复前缀 `<@member_openid>`
- `content`
- `reply`
- `attachments`
- `event_type`：决定是否加群 at 前缀
- `author_is_bot` / `author_is_self`：用于群过滤，不进入 Core Tool Loop  
  参考：`qq-maid-gateway-rs/src/gateway/event.rs::GroupMessage`、`qq-maid-gateway-rs/src/gateway/group.rs::group_reply_mention_prefix`

### Gateway 运行时还需要持有但不会传给 Tool Loop 的状态

- `ReplyCache`：只做 reply content 回填
- `BotOutboundCache`：只做群发机器人消息过滤
- `C2cStreamState`：只用于 QQ C2C 流式续接
- dispatcher/aggregator 内部的 `scope_key`、去重 reservation、冷却信息  
  参考：`qq-maid-gateway-rs/src/gateway/cache.rs`、`qq-maid-gateway-rs/src/api.rs::C2cStreamState`、`qq-maid-gateway-rs/src/gateway/dispatcher.rs`

**结论**：当前 Tool Calling 本身并不需要 `openid`、`group_openid`、`msg_seq`、QQ stream id 这些 QQ 细节；这些字段是 Gateway 为“最终把消息发回原目标”而持有的出站上下文，不应下沉进 Tool Loop。

---

## 4. Tool Calling 与 QQ 层的职责边界

## 4.1 Tool Loop、工具执行器、QQ 消息发送层分别在哪里

| 角色 | 当前模块 | 关键函数 / 类型 | 说明 |
| --- | --- | --- | --- |
| Tool Calling 入口判断 | `qq-maid-core` | `chat_flow.rs::handle_chat` | 决定私聊普通聊天是否进入 Tool Loop |
| Tool Calling 协议循环 | `qq-maid-llm` | `provider/openai/tool_loop.rs::openai_responses_tool_loop` | 处理 `function_call` / `function_call_output` |
| 服务端工具注册表 | `qq-maid-llm` | `tool.rs::ToolRegistry` | 白名单、超时、输出截断 |
| 具体工具适配 | `qq-maid-core` | `runtime/tools/weather.rs::WeatherTool` | 把业务执行器包装成 Tool |
| 真实业务执行器 | `qq-maid-core` | `DynWeatherExecutor.weather(...)` | Tool 复用现有天气能力 |
| QQ 回复渲染 | `qq-maid-gateway-rs` | `render.rs::render_respond_response` | 把 CoreResponse 变成 Text/Markdown/Image |
| QQ payload 构造与发送 | `qq-maid-gateway-rs` | `api.rs::QqApiClient::*` | 负责路径、payload、msg_seq、stream 等 |
| C2C 流式状态机 | `qq-maid-gateway-rs` | `gateway/stream.rs::stream_respond_c2c` | 只负责 Core delta → QQ stream |

## 4.2 当前工具调用结果、中间状态和最终回复如何进入 QQ 发送链路

### 工具调用结果

当前工具结果**不会直接进入 QQ 发送链路**。

它的实际路径是：

`WeatherTool.execute` → `ToolRegistry::execute_json` → OpenAI `function_call_output` → 模型继续生成最终答案 → `ChatOutcome.reply` → `RespondResponse` → Gateway 渲染/发送。

也就是说，当前工具结果默认只作为**模型上下文的一部分**回到 LLM，不直接变成一条 QQ 中间消息。  
参考：`qq-maid-core/src/runtime/tools/weather.rs::WeatherTool::execute`、`qq-maid-llm/src/tool.rs::ToolRegistry::execute_json`、`qq-maid-llm/src/provider/openai/tool_loop.rs::openai_responses_tool_loop`

### 中间状态

当前只有两类“中间状态”能进入 QQ 发送链路：

1. **普通聊天/搜索的 Core 文本增量**：
   - 私聊：`CoreResponseEvent::TextDelta` → `gateway/stream.rs` → QQ C2C Markdown stream
   - 群聊：会被 Gateway 消费掉，不增量发给群  
   参考：`qq-maid-core/src/service.rs::start_core_response_stream`、`qq-maid-gateway-rs/src/gateway/stream.rs::stream_respond_c2c_with_sender`、`qq-maid-gateway-rs/src/gateway/group.rs::consume_respond_stream`

2. **本地 fallback/系统提示**：
   例如 `/ping` 本地回复、dispatcher 拒绝提示、Core 调用失败后的“稍后再试”。这类消息不经过 Tool Loop。  
   参考：`qq-maid-gateway-rs/src/gateway/c2c.rs::render_local_ping_reply`、`qq-maid-gateway-rs/src/gateway/dispatcher.rs`

当前**没有**下面这些东西：

- “正在调用工具”状态消息
- “工具执行中/已完成”独立状态消息
- 工具增量结果直发 QQ
- 工具取消状态消息

### 最终回复

- 私聊普通 Tool Calling：当前走 `Complete` 路径，最终经 `send_c2c_respond_response_with_sender` 发送。  
  参考：`qq-maid-gateway-rs/src/gateway/c2c.rs::handle_c2c_message`
- 私聊普通非 Tool Loop 流式聊天：最终收尾仍由 `gateway/stream.rs` 发送结束帧。  
  参考：`qq-maid-gateway-rs/src/gateway/stream.rs::send_stream_end`
- 群聊：统一在拿到 `RespondResponse` 后一次性发出。  
  参考：`qq-maid-gateway-rs/src/gateway/group.rs::send_group_respond_response`

## 4.3 当前是否存在“Tool Calling 直接依赖 QQ 字段”或“QQ 层理解 Tool 协议”的问题

### 当前未发现的耦合

1. **Tool Loop 代码未直接依赖 QQ 字段**。  
   `ToolContext` 只有：
   - `task_id`
   - `user_id`
   - `scope_id`  
   没有 `openid`、`group_openid`、`msg_id`、`msg_seq`、QQ stream id。  
   参考：`qq-maid-llm/src/tool.rs::ToolContext`、`qq-maid-core/src/runtime/respond/llm_service.rs::tool_context_from_request`

2. **Gateway 不理解模型 Tool Call 协议**。  
   Gateway 只知道：
   - `RespondTransport::Complete`
   - `RespondTransport::Stream`
   - `RespondEvent::TextDelta/Completed/Failed`  
   它不解析 `function_call`、`function_call_output`、Tool schema。  
   参考：`qq-maid-gateway-rs/src/respond.rs::RespondTransport`、`qq-maid-gateway-rs/src/gateway/stream.rs`

3. **具体 QQ 提及语法、群回复前缀、msg_type/msg_seq、stream payload 都仍在 Gateway**。  
   Core 和 LLM 没有去理解 `<@member_openid>`、`/v2/users/{openid}/messages` 等平台细节。  
   参考：`qq-maid-gateway-rs/src/gateway/group.rs::prefix_group_reply_outbound`、`qq-maid-gateway-rs/src/api.rs`

### 当前仍值得注意的边界点

1. **`ToolContext.task_id` 当前复用了 `RespondRequest.message_id`，没有独立任务 ID**。  
   对首期 WeatherTool 足够，但如果后续要做“状态提示、取消、文件结果、多步任务审计”，单靠平台 message_id 可能不够。  
   参考：`qq-maid-core/src/runtime/respond/llm_service.rs::tool_context_from_request`

2. **Gateway 聚合器会调用 Core 的 `classify_inbound`**。  
   这不是 QQ/Tool 协议耦合，但说明 Gateway 的调度策略依赖 Core 的命令/pending 判断。当前这样做是为了避免在 Gateway 重写一套命令分类。  
   参考：`qq-maid-gateway-rs/src/gateway/aggregator/actor.rs::classify`

3. **群聊流式回复被 Gateway 收敛成最终一次性发送**。  
   这不是职责混合，但会影响未来若要做“工具状态提示”时的能力边界：当前只有私聊 C2C 真流式发送器。  
   参考：`qq-maid-gateway-rs/src/gateway/group.rs::consume_respond_stream`

---

## 5. 与 OpenClaw QQ 插件的对比

> 说明：当前只能确认公开 OpenClaw 主仓库存在明显的“Provider Tool Loop / Plugin Runtime / Transport 分层”思路；未直接检索到可对读的 QQ 插件源码，因此以下只讨论**值得借鉴的设计方向**，不声称与其 QQ 插件完全一致。

## 5.1 值得借鉴的点

### 1）工具协议层与聊天通道层分开

本仓库当前已经接近这个方向：

- Tool Loop 在 `qq-maid-llm`
- 业务 Tool 在 `qq-maid-core`
- QQ 出站在 `qq-maid-gateway-rs`

这是应该继续保持的。后续不要让 QQ 发送层去理解 `function_call` / `function_call_output`，也不要让 Tool Loop 知道 QQ 的 `msg_seq`、`group_openid`。

### 2）工具执行只走服务端白名单

当前 `ToolRegistry` 已经是白名单执行器，且只注册了 `WeatherTool`。这个方向比把任意工具暴露给模型安全得多。  
参考：`qq-maid-llm/src/tool.rs::ToolRegistry`、`qq-maid-core/src/runtime/respond.rs::RustRespondService::new`

### 3）通道层只负责投递，不负责解释工具语义

当前 Gateway 只处理：

- 入站消息解析
- scope/target 保留
- 普通发送/流式发送/推送

这正是后续继续扩工具时最应该保留的边界。

## 5.2 不适合当前仓库直接照搬的点

### 1）不适合把完整 TypeScript 插件运行时原样搬到 Rust

本仓库当前是：

- 单进程 Rust
- 强类型 CoreService
- 已有固定 QQ 发送链路
- 现阶段只需要少量受控 Tool

因此没有必要为了“像 OpenClaw 一样”提前引入完整插件宿主、插件生命周期、复杂 transport 适配层。

### 2）不适合让 QQ 层承载 Tool 状态机

如果未来加“调用中提示”，也应该是：

- Core/任务层产出“可展示状态事件”
- Gateway 只把这个事件翻译成 QQ 文本/Markdown/图片

而不是把 `function_call`、工具轮次、provider response id 这些逻辑塞进 Gateway。

### 3）不适合提前抽象过多 QQ 专属富交互字段

当前仓库没有 `keyboard` 实现，也没有明确的图片/文件工具结果协议。现阶段不宜为了对齐某个插件设计，先引入大量未落地的 QQ 交互抽象。应该先保留：

- 文本
- Markdown
- C2C Markdown stream
- 受控图片/文件扩展边界

---

## 6. 当前问题与风险

### 1）Tool Loop 当前没有可见中间态

当前工具调用是“内部完成后直接产出最终答案”。如果工具慢、需要多步、或者未来加入文件处理，QQ 用户侧目前看不到：

- 已经开始调用工具
- 当前在执行哪一步
- 工具失败发生在哪一步

证据：当前没有任何 Tool status event 类型；Gateway 只消费 `TextDelta/Completed/Failed`。  
参考：`qq-maid-core/src/service.rs::CoreResponseEvent`、`qq-maid-gateway-rs/src/gateway/stream.rs`

### 2）`task_id` 还不是独立任务身份

当前 `ToolContext.task_id` 优先取 `message_id`，否则才生成 UUID。对未来以下场景不够稳：

- 任务取消
- 文件归属
- 多工具多轮审计
- 工具状态消息与最终回复关联

参考：`qq-maid-core/src/runtime/respond/llm_service.rs::tool_context_from_request`

### 3）群聊没有独立的流式/状态消息能力

当前群聊即便 Core 返回 Stream，也会在 Gateway 里被折叠成最终一次性发送。后续若要支持“工具状态提示”，要么继续只支持私聊，要么单独设计群聊可接受的状态消息策略。  
参考：`qq-maid-gateway-rs/src/gateway/group.rs::consume_respond_stream`

### 4）QQ 字段所有权目前总体正确，但文档化还不够

目前代码里已经实现了：

- `openid` / `group_openid` 只在 Gateway 持有和发送
- `msg_seq` / `stream` 只在 Gateway 生成
- Tool Loop 只拿 `task_id/user_id/scope_id`

但如果后续没把这条边界写清楚，容易出现：

- Core 直接生成 QQ payload
- Tool 结果结构里塞入 QQ 路由字段
- Gateway 开始理解 Tool Call 协议

---

## 7. 后续建议

## 7.1 工具调用与 QQ 消息发送之间应怎样衔接

建议继续保持当前衔接方式：

1. **Tool Loop 只产出“最终回复”或“中间状态事件”**，不产出 QQ payload。
2. **Gateway 只接受统一的 Core 出站语义**，再翻译成 QQ 文本/Markdown/流式 payload。
3. 若后续要增加“工具状态提示”，应新增类似于：
   - `ToolStarted`
   - `ToolProgress`
   - `ToolFinished`
   - `ToolRequiresConfirmation`
   - `ToolCancelled`  
   这样的 **Core 级事件**，而不是让 Gateway 解析 provider `function_call` item。

## 7.2 QQ 接口字段应该由哪一层负责

建议字段所有权保持如下：

| 字段/概念 | 负责层 |
| --- | --- |
| `user_openid` / `group_openid` | Gateway |
| `msg_id` / `msg_seq` / `msg_type` | Gateway |
| QQ `stream` 控制字段 | Gateway |
| QQ 提及语法 `<@member_openid>` | Gateway |
| `text` / `markdown` / 图片/文件结果的通用出站语义 | Core |
| Tool Loop 轮次、工具名、工具参数、工具结果 | LLM/Core |
| 工具执行上下文 `task_id/user_id/scope_id` | Core/LLM |

## 7.3 后续增加工具状态提示时的边界

建议预留：

- Core 负责产生“用户可见的状态文案或结构化状态事件”
- Gateway 负责决定状态消息在 QQ 上用：
  - 普通文本
  - Markdown
  - 还是复用现有 C2C stream
- 不要让 Tool 本身直接发 QQ 消息
- 不要让 Tool 返回 `msg_id/group_openid` 等平台字段

## 7.4 后续增加图片/文件结果时的边界

建议预留：

- Tool / Core 只返回“图片/文件结果描述”或受控文件句柄
- Gateway 再把它映射成 QQ 图片/文件发送
- 私聊/群聊发图、发文件的能力差异，应留在 Gateway 处理
- `msg_type=7` 这类平台细节继续只在 Gateway builder 中维护

## 7.5 后续增加任务取消时的边界

建议预留：

- Core/任务层维护独立 `task_id`
- Gateway 只负责把用户取消输入映射成“取消当前 scope 下某任务”的请求
- 真正的取消判断（任务是否存在、是否可取消、是否属于当前用户）应在 Core/任务层完成
- 不要用 QQ `message_id` 直接充当长期任务主键

---

## 8. 待确认项

以下内容当前**不能只靠代码完全确认**，需要真机或官方文档复核：

1. QQ C2C Markdown stream 的 `stream.state`、`stream.id`、`stream.index`、`reset` 的全部平台约束。  
   代码侧依据：`qq-maid-gateway-rs/src/api.rs`、`qq-maid-gateway-rs/src/gateway/stream.rs`

2. `msg_type=0/2/7` 与 QQ 官方发送能力的完整对应关系，尤其是不同会话类型下的限制。  
   代码侧依据：`qq-maid-gateway-rs/src/api.rs`、`qq-maid-gateway-rs/src/markdown.rs`、`qq-maid-gateway-rs/src/media.rs`

3. `extract_sent_message_id` 和 `extract_c2c_text_stream_id` 当前兼容的响应 JSON 形状是否覆盖所有真实 QQ 返回包。  
   代码侧依据：`qq-maid-gateway-rs/src/api.rs::extract_sent_message_id`、`extract_c2c_text_stream_id`

4. 群聊是否存在可用、稳定、值得接入的“真流式”发送语义；当前代码没有实现。  
   代码侧依据：`qq-maid-gateway-rs/src/gateway/group.rs::consume_respond_stream`

5. QQ `keyboard` 在本仓库当前未实现；后续若要接入，需要官方文档确认 payload 形状、私聊/群聊支持情况和与 Markdown/流式的组合约束。  
   证据：当前仓库无 `keyboard` builder/发送入口。

6. 若要对比“OpenClaw QQ 插件”的具体做法，仍需补充其可访问源码或明确链接；当前只能借鉴公开通用分层思路，不能把外部实现细节当作既定事实。
