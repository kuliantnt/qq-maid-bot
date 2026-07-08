# 统一响应事件流设计基线

本文对应 Issue #366 的 Phase 1：梳理现有响应路径，定义后续收敛到统一流式 Agent Runtime 的最小事件模型和迁移边界。本文只记录设计基线，不改变当前运行行为。

## 目标边界

统一响应事件流的目标不是让所有平台都真实 token streaming，而是让 Core 对 Chat、Tool Loop、WebSearch 和 slash command 尽量输出同一种响应事件，再由 Gateway 根据平台能力渲染。

必须保留的边界：

- Gateway 负责平台字段、消息长度、分片、Markdown 降级、流式 state/index 和 ref_index 回填。
- Core 负责业务路由、session、pending、Todo、Memory、RSS、WebSearch 命令和具体业务 Tool。
- LLM 负责 Provider 协议、SSE、fallback、Tool Loop 协议和工具调用执行框架。
- Tool Calling 只执行服务端显式注册的白名单工具，工具结果必须以真实执行结果为准。
- 模型中间草稿、tool arguments、原始 JSON 和内部错误栈不得作为用户可见事件外发。

## 当前响应路径

| 路径 | 当前入口 | 当前输出形态 | 现状判断 |
| --- | --- | --- | --- |
| 普通聊天 | `RespondPlan::StreamingChat` -> `respond_stream` -> `handle_chat_stream` | provider 支持时产生 `TextDelta`，最终 `Completed` | 已接入 `CoreResponseStream`，是当前最接近目标的路径。 |
| Tool Loop | `RespondPlan::CompleteToolLoop` -> `handle_chat` -> `respond_with_tools` | `Status(ToolLoopStarted/Running/Finalizing)` + `Completed` | Core 已有可见进度壳，但工具轮本身仍是完整等待，最终回答也不是流式生成。 |
| WebSearch | `RespondPlan::WebSearch` -> `respond_web_search_stream` | provider 支持时复用 `/查` 的 `query_stream` delta，最终 `Completed` | 已接入 `CoreResponseStream`，但事件语义仍被压成普通文本 delta。 |
| slash command | `CommandDispatcher` 内确定性分发 | `RespondResponse` complete | 仍是完整响应路径，尚未纳入统一事件模型。 |
| pending 确认 | `handle_pending_operation` | `RespondResponse` complete | 应保持确定性，不默认进入 Agent Loop。 |
| 群聊 stream | Gateway `consume_respond_stream` | 消费到 `Completed` 后普通群消息发送 | 已是 buffered render；当前不发送群进度，避免刷屏。 |

核心代码基线：

- Core 事件契约：`qq-maid-core/src/service/types.rs::CoreResponseEvent`
- Core stream 包装：`qq-maid-core/src/service/streaming.rs::start_core_response_stream`
- Respond 路由：`qq-maid-core/src/runtime/respond/router.rs::RespondRouter::plan`
- 命令分发：`qq-maid-core/src/runtime/respond/command_dispatcher.rs`
- Tool Loop 调用：`qq-maid-core/src/runtime/respond/chat_flow/mod.rs::handle_chat`
- LLM 工具执行：`qq-maid-llm/src/provider/tool_loop.rs::ToolLoopExecutor`
- C2C 流式渲染：`qq-maid-gateway-rs/src/gateway/stream/delivery.rs`
- 群聊聚合渲染：`qq-maid-gateway-rs/src/gateway/group.rs::consume_respond_stream`

## 当前事件契约

当前 Core 已有最小事件：

```text
CoreResponseEvent::Status(CoreResponseStatus)
CoreResponseEvent::TextDelta(String)
CoreResponseEvent::Completed(CoreResponse)
CoreResponseEvent::Failed(CoreRespondFailure)
```

当前状态类型只有：

```text
ToolLoopStarted
ToolLoopRunning
ToolLoopFinalizing
```

这已经覆盖了“有一个统一 stream 外壳”的基础，但还不足以表达工具轮、单个工具、命令开始/结束、最终回答开始等语义。后续应优先扩展既有事件契约，而不是新增一套平行 stream 抽象。

## 最小目标事件模型

建议把外部稳定契约继续收敛在 `CoreResponseEvent`，第一阶段只扩展可见事件，不暴露模型内部消息。

```text
ResponseStarted
Status(kind, text)
TextDelta(text)
Completed(CoreResponse)
Failed(CoreRespondFailure)
```

其中 `StatusKind` 可分阶段扩展：

```text
ToolLoopStarted
ToolRoundStarted
ToolCallStarted
ToolCallFinished
ToolCallFailed
ToolLoopFinalizing
CommandStarted
CommandFinished
FinalAnswerStarted
```

实现时可以先不引入 `ResponseStarted`，因为当前 stream 创建本身已隐含开始；若后续 Gateway 需要统一初始化渲染状态，再补充该事件。`FinalAnswerStarted` 可以先作为 `StatusKind`，等最终回答流式路径稳定后再决定是否独立成事件。

用户可见规则：

- `TextDelta` 只承载最终回答正文或 WebSearch 真实结果增量。
- 工具轮产生 tool call 时，只发系统生成的 status/progress，不发模型同轮自然语言草稿。
- 工具参数、工具原始结果、JSON、stack、secret 不进入任何用户可见事件。
- `Completed` 仍是最终权威响应，session、diagnostics、visible snapshot 和 ref_index 相关信息以它为准。
- `Failed` 必须携带用户可理解的安全失败文案，不能要求 Gateway 解析内部错误。

## Tool Loop 事件落点

后续 Tool Loop 支持事件输出时，不应让 `qq-maid-llm` 反向依赖 `qq-maid-core`。可选实现方向：

1. 在 `qq-maid-llm` 定义 provider 级 `ToolLoopProgressEvent` 或回调 sink。
2. `ToolLoopExecutor::prepare_call` / `execute_prepared_call` 在准备、开始、完成、失败时触发内部进度事件。
3. Core 在调用 `respond_with_tools` 时传入回调，把 provider 级事件映射成 `CoreResponseEvent::Status`。
4. Core 负责把工具名映射成安全、短、用户可见的提示文案；LLM 层只传结构化事实，不拼业务文案。

这样可以保持依赖方向：

```text
qq-maid-core -> qq-maid-llm
```

同时避免 `qq-maid-llm` 理解 QQ、session、Todo 可见编号或 Gateway 渲染策略。

## 最终回答流式化

当前 `respond_with_tools` 返回完整 `ChatOutcome`，工具完成后的最终回答也随 Tool Loop 一次性返回。Phase 3 的关键不是 Gateway，而是 LLM Tool Loop 需要区分：

- 工具调用轮：只产出受控 status，不外显模型草稿。
- 最终回答轮：禁止继续调用工具后，优先使用 provider streaming 产生最终 `TextDelta`。
- provider 不支持 streaming 时，保留聚合输出并只发 `Completed`。

需要注意：如果 final answer streaming 已经开始，后续 provider 错误不能回退到另一个 provider 重新生成不同全文，避免用户已看到的内容和最终答案不一致。这个规则应沿用现有 stream fallback 的“delta 后不 fallback”原则。

## WebSearch 收敛方式

WebSearch 当前已经走 `CoreResponseStream`，但 `/查` 的“正在联网查询中”与真实搜索 delta 都表现为 `TextDelta`。后续收敛时建议：

- 查询启动提示改成 `Status(WebSearchStarted)` 或复用通用 `ToolCallStarted`。
- 搜索结果 delta 若已是最终回答正文，可以继续走 `TextDelta`。
- 如果后续 WebSearch 纳入通用 Tool Loop，`WebSearchTool::query_stream` 的增量应通过同一 progress/final-answer 边界输出。
- `/查` 作为兼容入口保留，不强制变成 LLM Tool Call。

## slash command 接入方式

slash command 仍应保持确定性执行，不默认变成 LLM Tool Call。建议迁移顺序：

1. 短命令先由 Core 包装成事件流：`CommandStarted` -> `Completed`。
2. 长耗时命令再补 `Status` 进度，如 RSS 拉取、WebSearch、外部查询。
3. 命令业务函数仍返回 `RespondResponse`，先由外层 adapter 转成事件，等稳定后再决定是否让命令内部直接产出事件。

这样可以减少一次性改造所有命令的风险。

## Gateway 渲染策略

Gateway 的统一职责是消费事件并按平台能力渲染：

| 策略 | 当前或目标平台 | 行为 |
| --- | --- | --- |
| Streaming render | QQ C2C 支持 Markdown stream 时 | `TextDelta` 增量发送，`Completed` 发结束帧；首帧成功后不再补发普通全文。 |
| Buffered render | 群聊、微信服务号、关闭 C2C stream 时 | 消费事件到 `Completed`，按最终 `CoreResponse` 发送。 |
| Hybrid render | 私聊工具调用、未来可控群聊进度 | 最多发送少量 `Status`，最终正文以 `Completed` 或 final delta 收口。 |

消息长度、UTF-8/中文/emoji/Markdown 安全分片、Markdown 降级和图片能力仍属于 Gateway。Core 不应引入平台发送上限常量。

ref_index 规则：

- 普通 complete / buffered render 以实际发送出去的最终正文记录。
- progress/status 不作为主要可引用正文。
- C2C active stream 当前未确认 QQ final 回包字段能被 quote 回传，因此暂不写 bot outbound ref_index；该限制应保持到真机确认。

## 分阶段迁移建议

### Phase 2：Tool Loop 进度事件

- 在 `qq-maid-llm` Tool Loop 执行器旁增加内部 progress 事件或回调。
- Core 将工具开始、完成、失败映射为安全 `Status`。
- 私聊可显示较细进度；群聊默认仍不发送或只发送一条受控状态。
- 不改变工具业务逻辑、权限、白名单、超时和输出大小限制。

### Phase 3：工具完成后的最终回答流式

- 在 Tool Loop 最终回答阶段接入 provider streaming。
- `TextDelta` 只从最终回答轮产生。
- provider 不支持 streaming 时保持 `ProgressThenComplete`。
- 测试覆盖“工具轮不外显模型草稿”和“最终回答可流式”。

### Phase 4：WebSearch 并入统一事件语义

- 把启动提示从文本 delta 收敛为 status。
- 保留 `/查` 兼容入口。
- 评估 WebSearch Tool Loop 与 `/查` 是否能复用同一 final-answer stream。

### Phase 5：slash command 事件包装

- 先在外层包装短命令，不改各命令业务函数。
- 长耗时命令按需增加进度事件。
- pending 确认继续保持确定性路径。

### Phase 6：Gateway 统一事件渲染

- 把 C2C stream、C2C stream disabled 和 group consume 的公共事件消费规则抽出。
- 继续由平台 capability 决定 streaming/buffered/hybrid。
- 分片和 ref_index 仍以 Gateway 最终实际发送结果为准。

### Phase 7：清理旧路径

- 新路径稳定后再收敛 `CompleteToolLoop` 命名和 WebSearch 单独 stream helper。
- 删除重复 helper 前必须有 Chat、Tool Loop、WebSearch、slash、群聊开关测试覆盖。

## 需要测试覆盖的关键行为

- 私聊普通聊天仍能输出 `TextDelta` 并正常 `Completed`。
- 私聊 Tool Loop 至少输出一个受控 status，工具参数和模型草稿不外显。
- 工具完成后的最终回答在 provider 支持时能继续流式输出。
- provider 不支持 streaming 时仍能聚合发送完整最终回复。
- 群聊未唤醒不触发工具；群聊 Tool Loop 关闭时明确工具请求不回落无 tools 普通聊天让模型猜外部状态。
- slash command 行为保持兼容，事件包装不改变命令结果。
- 超长最终正文仍由 Gateway 安全分片。
- ref_index 记录最终实际发送正文，不记录处理中状态。

## 当前未解决问题

- 当前未发现可直接复用的通用 command event adapter，需要实现前再评估 `CommandDispatcher` 的最小改造点。
- 当前 C2C active stream 的 ref_index 回填仍需 QQ 真机确认最终帧或回调字段。
- Tool Loop final answer streaming 需要 provider 协议层设计，不能只在 Core 外层补 synthetic delta。
- 群聊 progress 策略需要结合真实刷屏风险设置节流，默认应继续保守。
