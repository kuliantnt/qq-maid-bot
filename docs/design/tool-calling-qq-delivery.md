# Tool Calling 与 QQ 投递边界

> 状态：当前设计基线，更新于 2026-07-30。早期只有 OpenAI Responses 最小 Tool Loop 时的链路说明已移入 [设计归档](./archive/tool-calling-qq-delivery-v1.md)。

本文只约束 Agent / Tool 结果如何经过 Core 进入平台投递，不复制 QQ OpenAPI 字段或各业务 Tool 的内部规则。

## 职责边界

| 层 | 负责 | 不负责 |
| --- | --- | --- |
| Gateway | 平台事件解析、`CoreRequest` 映射、QQ / OneBot / 微信渲染、引用、分段、流式序列与发送结果 | Tool Schema、业务成功判断、Todo / Memory 权限和 Provider 协议 |
| Core | 场景路由、Tool 注册、业务上下文、领域后处理、确定性回执、Session 和可见实体快照 | QQ `msg_seq`、stream id、`<@user_id>` 或 Provider SSE 帧 |
| LLM | Provider 协议、候选路由、Agent Loop 轮次、Tool Call 校验/执行协议、超时/取消和 `AgentRunDiagnostics` | Todo、Memory、RSS 等业务归属与平台发送 |
| 业务 Tool 领域 | 参数解析、权限、持久化、幂等、可见实体、pending 与成功验真 | 拼装 QQ payload 或用模型文案伪造完成结果 |

依赖方向保持 `gateway -> core -> llm -> common`。LLM 不反向依赖 Core，Core 也不绕过 LLM 自行维护 Provider Tool Calling 协议。

## 当前调用链

1. Gateway 将 QQ 官方、OneBot 或微信事件归一化为平台无关的 `CoreRequest`。平台原始目标留在 Gateway 及投递上下文。
2. `qq-maid-core/src/runtime/respond/agent_route.rs` 只使用场景开关、Provider 能力、群聊策略和工具白名单决定是否进入 Agent Runtime，不用业务关键词决定工具能力。
3. 私聊普通纯文本在能力允许时可进入通用 Agent Runtime。群聊完整 Tool Loop 默认关闭；只有显式场景策略允许的工具才能暴露，当前还可以独立进入 `MemoryOnly` 受限模式。Slash、pending 确认、非文本输入和宿主机代码执行不得默认进入通用 Tool Loop。
4. `qq-maid-llm/src/agent_loop/` 维护请求级轮次、工具执行、超时、取消、候选模型和 diagnostics。模型只能调用当轮 `ToolRegistry` 显式注册的 Tool。
5. Core 在 `qq-maid-core/src/runtime/tools/agent_turn.rs` 将整轮可信 Tool 结果投影到各业务域。Todo、Memory 和 Search 等域自行维护成功验真、确定性展示、Session 快照与诊断；通用调度层不理解具体 Tool 名称和业务动作。
6. Core 将状态、文本增量、最终回复或结构化失败写入 `CoreResponseEvent::Status / TextDelta / Completed / Failed`。
7. Gateway 依平台能力消费这些事件：QQ 官方 C2C 可流式发送；QQ 群聊、OneBot 一期和微信通常收敛到可信 `Completed` 后一次投递。

## 平台与业务数据所有权

| 数据 | 所有者 | 用途 |
| --- | --- | --- |
| QQ `msg_id` / `event_id` / `msg_seq` / stream id / index | Gateway | 被动回复关联、流式续写、平台幂等与发送 |
| OneBot `message_id` / `echo` / segment | Gateway | OneBot API 关联、引用、`at` 和媒体投递 |
| conversation / actor / interaction scope | Core | 会话、权限、pending 与可见实体隔离 |
| owner scope | 业务领域 | Todo / Memory / RSS 等持久化数据归属 |
| delivery target / `PushTarget` / `PushMention` | Gateway + Core 投递契约 | 主动推送的真实平台目标和被提醒成员 |
| Tool arguments / result / effect metadata | LLM Tool 协议 + Core 业务域 | 执行、验真、回执与诊断 |

`scope_key` / `owner_key` 是业务隔离键，不是可反解的平台发送地址。Tool 也不应接收 QQ `msg_seq`、stream id 或群 `at` 前缀来决定业务操作。详细术语见 [Scope 与 Identity 边界](./scope-identity-boundary.md)。

## 可信结果与状态事件

- `AgentRunDiagnostics` 在请求级累计模型轮次、模型发出的工具、已执行工具、可信结果、结果未知工具和终止原因。成功与失败共用同一语义。
- 工具一旦开始可能产生副作用，候选模型失败后不得自动重放该副作用。取消后不得启动新工具；已开始但未取得可信结果的工具必须保留 unknown 状态。
- `CoreResponseEvent::Status` 只表示可展示的进度/状态语义，不是业务成功证明。Gateway 可按平台能力展示或忽略，但不得把 Status 当成 `Completed`。
- Todo 写入、Memory 保存、RSS 变更等成功文案必须由各领域消费真实 Tool / 持久化结果后生成，不能直接相信模型最终文本。
- 工具原始结果默认只进入 Agent 后续轮次和 Core 领域后处理，不应越过 Core 直接变成 QQ 消息。

## QQ C2C 流式发送不变量

1. 首帧成功前，Gateway 可保留一次普通全文回退的可能。
2. 首帧成功并取得 QQ stream id 后，本轮回复归该 stream 所有；后续帧和最终帧失败均不再补发第二条普通全文。
3. 同一 stream 的 `msg_seq`、stream id 和 index 推进由 Gateway 状态机维护；只在平台真实成功后提交下一状态。
4. `Completed` 是最终响应的唯一可信所有者。`TextDelta` 是过程文本，`Status` 是状态，`Failed` 保留结构化失败与 Agent diagnostics。
5. QQ 流式失败只能影响平台投递结果，不得倒推 Tool 未执行或业务写入失败。业务与投递结果必须分别记录。

## 新增能力检查

新增 Tool、状态事件、媒体结果或平台投递方式时，至少确认：

- Tool 已在服务端注册，并受场景白名单、参数 Schema、权限、超时和输出大小限制。
- 写工具的业务结果、幂等、pending、用户可见编号和确定性回执留在对应 `tools/<domain>/`。
- 平台输出只消费 Core 事件和通用消息结构，没有将 QQ / OneBot 字段下沉到 Core 或 LLM。
- 失败和取消测试覆盖“工具未开始”、“工具结果未知”、“业务已成功但平台投递失败”和“首帧成功后续帧失败”。
- 日志只记录脱敏的 stop reason、轮次、工具名、投递阶段和错误摘要，不记录工具原始结果、聊天正文、完整平台 ID 或凭证。
