
# Gateway → Core 进程内调用与流式响应边界改造完成报告

> 本文记录 Gateway → Core 进程内调用重构及第一阶段流式响应边界改造的实际完成情况。
>
> 本文描述的是改造后的当前状态，不再代表重构前的 localhost HTTP / SSE 架构。
>
> 需要特别说明：当前已经建立进程内 `Complete | Stream` 业务边界，并解决旧完整请求总超时问题；但 LLM provider 的真实 token delta 尚未完整贯通到 Gateway，因此当前阶段不等同于“完整的 token 级流式响应”。

---

## 一、改造背景

项目原先虽然由同一个二进制进程启动 Gateway 和 Core，但 Gateway → Core 的业务请求仍通过 localhost HTTP 完成。

重构前的主要调用链为：

```text
QQ Gateway WebSocket
→ qq-maid-gateway-rs 解析事件
→ 构造 HTTP JSON RespondRequest
→ POST Core /v1/respond
→ Core 反序列化请求
→ 执行业务 flow、搜索、RAG、LLM
→ 返回 JSON 或 SSE
→ Gateway 解析 JSON / SSE
→ 渲染 text / markdown
→ 调用 QQ OpenAPI 发送
```

该架构存在以下问题：

1. Gateway 和 Core 已处于同一进程，却仍维护内部 HTTP client/server。
2. 请求和响应需要重复进行 JSON 序列化与反序列化。
3. 流式响应依赖内部 SSE 编码、解析和文本事件协议。
4. Gateway 和 Core 分别维护相似但不完全一致的请求、响应 DTO。
5. Core ready 状态依赖 localhost `/healthz` 轮询。
6. 长耗时请求需要等待完整结果，容易被统一总超时中断。
7. 错误语义混合了 HTTP 状态、业务错误和 provider 错误。
8. 内部通讯层增加了调用链复杂度，但没有提供真正的跨进程收益。

本次重构的核心目标是：

> 移除 Gateway → Core 主业务链路中的 localhost HTTP 和 SSE 传输，让 Gateway 通过进程内强类型接口调用 Core。

在完成进程内调用后，又发现普通聊天、RAG 和 `/查` 等长耗时请求仍然需要等待完整业务结果才能返回 Gateway，因此继续增加了第一阶段进程内流式响应边界。

---

## 二、改造目标

本次改造包含两个相互关联的目标。

### 1. Gateway → Core 进程内调用

将以下内部传输职责从主业务链路中移除：

* localhost HTTP 请求；
* HTTP JSON 请求和响应 DTO；
* HTTP status 处理；
  -内部 SSE 编码和解析；
* `Accept: text/event-stream` 协商；
* 仅服务于组件间通讯的 readiness 探测。

Gateway 保留平台适配和 QQ 发送职责，Core 保留会话、命令、RAG、Memory、Todo、搜索和 LLM 编排职责。

### 2. 建立进程内流式响应边界

解决以下长耗时场景仍被完整请求总超时中断的问题：

* 普通聊天；
* 普通聊天中的 RAG 检索；
* `/查` 联网搜索；
* 较长 Prompt 或长回答；
* 模型建连、首包或生成耗时较长的请求。

目标不是重新引入 HTTP SSE，而是使用 Rust 进程内强类型流，在 Gateway 和 Core 之间传递业务响应事件。

---

## 三、重构前架构摘要

### 1. Gateway → Core 请求链路

重构前 Gateway 负责：

* 解析 QQ Gateway WebSocket 事件；
* 将 QQ C2C / Group 消息转换为 Gateway 内部事件；
* 执行去重、群消息过滤和冷却；
* 拼接引用和附件文本；
* 构造 HTTP `RespondRequest`；
* 调用 Core `/v1/respond`；
* 根据响应 `Content-Type` 判断 JSON 或 SSE；
* 解析完整响应或手写 SSE frame；
* 渲染并发送 QQ 消息。

Core 负责：

* 接收 `HttpRespondRequest`；
* 转换成内部 `RespondRequest`；
* 分发 session、translation、weather、train、search、RSS、Todo、Memory 和普通聊天；
* 注入 Memory、RAG 和会话上下文；
* 调用 LLM；
* 返回完整 JSON 或 Web Search SSE。

### 2. 旧完整请求超时

`CoreHandle::respond` 或此前对应调用层会对完整的：

```text
RustRespondService::respond(req)
```

应用统一的：

```text
LLM_REQUEST_TIMEOUT_SECONDS
```

默认值为 90 秒。

因此普通聊天和 `/查` 都需要在 90 秒内完成：

```text
搜索 / RAG
→ Prompt 构造
→ LLM 建连
→ 模型完整生成
→ CoreResponse 构造
```

只要整个流程超过该时间，请求就可能被直接中断。

这也是本次流式边界改造的直接原因。

---

## 四、改造后的总体架构

当前 Gateway → Core 主调用关系已经演进为：

```text
QQ / OneBot 平台
        │
        ▼
Gateway
  - 平台事件解析
  - 去重和群聊过滤
  - 引用与附件适配
  - QQ 消息渲染和发送
        │
        │ CoreRequest
        ▼
CoreService / CoreHandle
  - 会话和 Pending
  - 命令路由
  - Memory / Todo / RSS
  - RAG / 联网搜索
  - Prompt 构造
  - LLM 调用与业务编排
        │
        ├─ CoreRespondOutput::Complete
        │
        └─ CoreRespondOutput::Stream
```

Core 的响应边界现在包含：

```rust
CoreRespondOutput::Complete(CoreResponse)
CoreRespondOutput::Stream(CoreResponseStream)
```

其中：

* `Complete` 用于短命令和快速本地业务；
* `Stream` 用于普通聊天、RAG 和 `/查` 等长耗时用户可见生成流程。

Gateway 不再通过 HTTP `Content-Type` 判断响应类型，而是直接匹配强类型枚举。

---

## 五、当前完成状态

本次已完成：

```text
第一阶段：进程内调用和业务流边界
```

当前实现的真实形态为：

```text
Gateway 调用 CoreService::respond
→ Core 很快返回 Complete 或 Stream
→ Stream producer 在后台继续执行原完整业务 flow
→ 得到完整 CoreResponse
→ producer 发送 Completed(CoreResponse)
→ Gateway 渲染并发送最终消息
```

因此当前已完成的是：

* 进程内调用；
* 完整响应与流式响应的类型边界；
* 长请求不再被旧完整请求总超时直接夹断；
* Gateway 能消费 Core 的业务流；
* 最终结果只发送一次。

当前尚未完成的是：

```text
Provider SSE / token delta
→ LlmStreamEvent::TextDelta
→ CoreResponseEvent::TextDelta
→ Gateway 增量发送或消息更新
```

---

## 六、已完成改动

### 1. Gateway → Core 改为进程内调用

Gateway 不再依赖原有 localhost HTTP 请求来调用 Core 主业务入口。

Gateway 通过 Core 提供的强类型 handle 或 service 接口发起请求。

进程内调用保留了清晰的职责边界：

* Gateway 不直接持有 Core 的 storage、executor 或内部业务 flow；
* Gateway 不直接构造 `RustRespondService`；
* Core 通过统一 facade 对外提供业务能力；
* Gateway 只依赖稳定的请求、响应和错误类型。

### 2. Core 响应支持 Complete 和 Stream

`CoreService::respond` 当前返回：

```rust
CoreRespondOutput::Complete(CoreResponse)
CoreRespondOutput::Stream(CoreResponseStream)
```

该设计允许短命令继续使用简单的完整响应，同时让长耗时自然语言生成在进入 Core 后快速返回流句柄。

没有为了流式支持而强制所有命令都进入 channel 或事件状态机。

### 3. 普通聊天立即返回 Stream

普通直接聊天现在会进入流式业务边界。

Core 不再等待以下步骤全部完成后才向 Gateway 返回：

* 会话上下文读取；
* 成员信息和映射处理；
  -长期 Memory 注入；
* RAG 知识检索；
* Prompt 构造；
* LLM 完整生成。

Core 会先建立业务流，再由 producer 执行后续完整流程。

这解决了长 Prompt、长上下文或长回答必须在旧总超时内全部完成的问题。

### 4. RAG 随普通聊天进入流式边界

当前仓库没有独立的 RAG 命令。

普通聊天中的：

```text
knowledge_index.search_context(...)
```

仍属于聊天业务 flow 的一部分。

由于普通聊天已经进入 `Stream` 边界，RAG 检索和后续 LLM 生成也不再阻塞 `CoreService::respond` 返回。

### 5. `/查` 立即返回 Stream

`/查`、`/查询` 或对应联网搜索入口现在使用进程内业务流。

当前调用关系为：

```text
Gateway 收到 /查
→ CoreService::respond 返回 Stream
→ producer 执行联网查询
→ 整理搜索结果
→ 调用现有 LLM / Query flow
→ 构造完整 CoreResponse
→ 发送 Completed(CoreResponse)
```

因此 `/查` 不再需要在旧完整请求 90 秒超时内完成全部联网查询和答案生成。

### 6. 短命令继续使用 Complete

以下短命令和本地业务默认继续返回完整响应：

* `/ping`
* `/help`
* `/todo`
* `/memory`
* 天气；
* 火车；
* RSS 管理；
* 其他不需要长时间自然语言生成的本地命令。

这些命令不需要为了架构统一而增加不必要的 channel 和 producer。

### 7. 保留 upstream check 独立语义

`/ping check` 或对应 upstream 检查仍保持独立完整调用语义。

该调用：

* 不进入普通聊天；
* 不创建业务 session；
* 不参与用户聊天流；
* 不因普通聊天流式化而改变用途。

### 8. 调整完整请求超时范围

旧的完整请求总超时不再包住普通聊天和 `/查` 的整个生命周期。

当前超时语义为：

```text
Complete：
    继续适用完整调用超时。

Stream：
    CoreService::respond 只负责尽快返回 Stream；
    后续业务处理不再受旧“完整回答必须在 90 秒内结束”的约束。
```

本次改造解决的是旧完整请求总超时。

搜索超时、LLM 建连超时、首 token 超时、流空闲超时和 QQ 平台发送超时，仍可在后续继续细分。

### 9. 新增 Core 业务响应事件

Core 业务流使用强类型事件表达响应进度和最终状态。

当前事件至少包含类似语义：

```rust
CoreResponseEvent::TextDelta(String)
CoreResponseEvent::Completed(CoreResponse)
CoreResponseEvent::Failed(CoreFailure)
```

Gateway 只依赖 Core 业务事件，不接触：

* OpenAI Responses 原始事件；
* Chat Completions SSE chunk；
* DeepSeek provider JSON；
* BigModel provider JSON；
* HTTP body；
* SSE frame 文本。

### 10. Completed 是最终正文唯一来源

当前明确规定：

```text
Completed(CoreResponse)
```

是 Gateway 最终渲染和发送的唯一权威正文来源。

`TextDelta` 当前只被消费，不进行：

* 正文拼接；
* QQ 发送；
* 最终响应覆盖；
* 重复落库。

该约束避免了：

```text
delta 累积正文
+
Completed 中的完整正文
=
答案重复
```

### 11. Gateway 支持消费 Core 业务流

Gateway 当前根据 `CoreRespondOutput` 分流：

```text
Complete
→ 使用现有完整响应渲染和发送路径

Stream
→ 消费 CoreResponseEvent
→ 等待 Completed(CoreResponse)
→ 渲染并发送最终结果
```

Gateway 不再解析内部 SSE，也不需要根据 HTTP `Content-Type` 判断响应形式。

### 12. QQ 侧保持最终一次发送

当前 Gateway 不会逐 token 向 QQ 发送消息。

普通聊天和 `/查` 的 `TextDelta` 当前不会直接产生 QQ 消息。

最终只在收到：

```text
Completed(CoreResponse)
```

后发送一次完整结果。

该行为避免了：

* 每个 token 发送一条消息；
* 群聊刷屏；
* QQ 平台频率限制；
* Markdown 分片导致格式损坏；
* 多段消息引用关系混乱。

这也意味着当前用户看到的体验仍然是：

```text
等待
→ 一次性出现完整回答
```

而不是可见的 token 级增量输出。

### 13. 保留现有渲染和发送逻辑

流式边界没有改变 Gateway 原有的平台发送职责。

当前继续复用：

* `render_respond_response`
* `send_outbound_with_fallback`
* `send_group_outbound_with_fallback`
* text / Markdown 双通道；
* Markdown 失败回退 text；
* C2C 引用消息；
* 群聊发送目标；
* QQ 返回消息 ID 提取。

Core 不负责决定 QQ 平台使用 text、Markdown 还是 image。

### 14. 缓存只在真实发送成功后写入

当前保留：

* C2C reply cache；
* 群聊 outbound cache；
* 群消息回复机器人识别所需缓存。

缓存只在真实 QQ 消息发送成功后写入。

不会因为 Core 已产生 `Completed` 就提前写入缓存。

发送失败时：

* 不记录虚假发送成功；
* 不写 reply cache；
* 不写 group outbound cache；
* 不再额外尝试补发另一条失败消息。

### 15. 新增最小 Core 失败类型

Core 新增了最小业务失败边界，例如：

```rust
CoreFailureKind
```

Gateway 只需要看到：

* 错误类别；
* 用户可见文案；
* 是否可重试。

Gateway 不再直接依赖：

* provider 内部错误；
  -搜索实现错误；
* storage 错误；
* LLM SDK 或 HTTP 错误；
* Core 内部错误链。

详细错误原因继续由 Core 日志记录。

### 16. 建立 LLM 标准流基础类型

`qq-maid-llm` 已增加标准流基础类型：

```text
LlmStreamEvent
LlmStream
collector
```

目标事件语义包括：

* 文本增量；
* token usage；
  -完成和 finish reason；
* 流中途错误。

这些类型为后续统一以下 provider 的真实增量流提供入口：

* OpenAI Responses；
* OpenAI Chat Completions；
* DeepSeek；
* BigModel。

当前阶段只建立了基础类型和 collector，尚未将全部 provider 的真实 delta 贯通到 Core。

---

## 七、当前调用链

### 1. 普通聊天

当前普通聊天链路为：

```text
QQ 消息
→ Gateway 解析 C2C / Group 事件
→ Gateway 去重、群聊过滤和冷却
→ Gateway 构造 CoreRequest
→ CoreService::respond
→ 返回 CoreRespondOutput::Stream
→ Core producer 执行聊天业务 flow
    → 读取会话
    → 处理成员映射
    → 构造 Memory context
    → 执行 knowledge RAG 检索
    → 组装 Prompt
    → 调用现有完整 LLM flow
    → 获得完整回答
    → 写入必要的 session 状态
→ producer 发送 Completed(CoreResponse)
→ Gateway 渲染完整响应
→ 调用 QQ OpenAPI 发送一次
→ 发送成功后写入 reply / outbound cache
```

### 2. `/查`

当前联网查询链路为：

```text
用户发送 /查
→ Gateway 构造 CoreRequest
→ CoreService::respond
→ 立即返回 CoreRespondOutput::Stream
→ producer 执行搜索业务 flow
    → 联网查询
    → 搜索结果整理
    → Prompt 构造
    → LLM 生成完整结果
→ producer 发送 Completed(CoreResponse)
→ Gateway 渲染并发送最终查询结果
```

### 3. 短命令

当前短命令链路为：

```text
用户发送 /ping、/help、/todo 等
→ Gateway 构造 CoreRequest
→ CoreService::respond
→ Core 完整执行业务
→ 返回 CoreRespondOutput::Complete(CoreResponse)
→ Gateway 使用原完整发送路径
```

---

## 八、本次已经解决的问题

### 1. 移除 Gateway → Core 主链路 HTTP 传输负担

主业务调用不再需要：

* reqwest POST；
* JSON 序列化；
* JSON 反序列化；
* HTTP status 判断；
* `Content-Type` 协商；
* SSE frame 编码；
* Gateway 手写 SSE parser。

### 2. 解决长请求完整等待问题

普通聊天和 `/查` 不再要求整个业务流程在旧的 90 秒总超时内结束。

即使：

* 搜索较慢；
* RAG 数据较多；
* Prompt 较长；
* 模型首包较慢；
  -回答较长；

Gateway 也可以先获得 `Stream`，而不是同步等待完整 `CoreResponse`。

### 3. 统一 Complete 和 Stream 边界

短命令不需要承担流式复杂度，长耗时任务也不再强制包装为完整响应。

### 4. 去除 Gateway 对 Core 内部错误的依赖

Gateway 只负责用户文案和平台发送，不再理解 provider、storage 或搜索内部错误。

### 5. 避免流式正文重复发送

`Completed(CoreResponse)` 是唯一最终正文来源，避免 delta 和 final 双重拼接。

### 6. 保留 QQ 发送层既有语义

重构没有把 Markdown、fallback、reply cache 和 QQ OpenAPI 逻辑搬进 Core。

---

## 九、当前尚未完成的内容

### 1. 当前不是真正的 token 级流式

当前 producer 仍然复用原来的完整业务 flow。

也就是说：

```text
Core 返回 Stream
→ producer 等待完整 LLM 结果
→ 只发送 Completed
```

而不是：

```text
LLM 每产生一个 token delta
→ Core 发送 TextDelta
→ Gateway 持续接收
```

因此当前更准确的名称是：

> 进程内异步业务流边界。

不能将其描述为：

> Provider token 级流式响应已经完成。

### 2. Provider 尚未统一接入 LlmStream

OpenAI Responses、Chat Completions、DeepSeek、BigModel 等 provider 的真实流式事件尚未全部转换为统一的：

```text
LlmStreamEvent
```

目前新增的 `LlmStreamEvent`、`LlmStream` 和 collector 主要是后续接入基础。

### 3. `chat()` 尚未完全改为收集标准流

目标架构应为：

```text
provider.stream_chat()
→ LlmStreamEvent
→ collector
→ ChatOutcome
```

这样：

* 用户可见生成可以直接转发 delta；
* 内部结构化任务可以收集完整结果；
* provider 不需要维护两套解析逻辑。

当前该目标尚未全部实现。

### 4. Core TextDelta 尚未承载真实 token

虽然 Core 已有 `TextDelta` 事件边界，但普通聊天和 `/查` 尚未持续发送真实 provider delta。

当前 Gateway 消费 `TextDelta`，但不会拼接和发送。

### 5. 用户仍然看不到增量输出

QQ 侧目前依然等到 `Completed` 后发送一次完整消息。

因此用户体验仍是：

```text
等待较长时间
→ 完整回答出现
```

本次解决的是内部请求总超时，而不是用户可见首字延迟。

### 6. 取消传播仍不完整

Gateway 发送失败或主动丢弃 stream 后，receiver 会关闭。

但当前 producer 在执行完整搜索或 LLM 调用期间，可能无法立刻发现 receiver 已被丢弃。

可能出现：

```text
Gateway 已不再等待结果
→ Core 仍继续执行搜索或完整 LLM 请求
→ 直到尝试发送 Completed 才发现 channel 已关闭
```

后续需要增加：

* cancellation token；
* 搜索阶段取消；
* provider stream 取消；
* 每次 delta 或关键步骤检查取消状态。

### 7. 超时尚未完全拆分

本次主要解决旧的完整请求总超时。

后续仍应考虑区分：

* 联网搜索超时；
* LLM 建连超时；
* 首 token 超时；
* 流空闲超时；
* 整体任务最大时长；
* QQ OpenAPI 单次发送超时。

### 8. 流式候选模型降级尚未完全实现

目标语义应为：

```text
尚未输出任何文本：
    可以尝试下一个候选模型。

已经输出部分文本：
    不允许无提示拼接下一个模型的完整答案。
```

由于当前仍使用完整业务 flow，该语义尚未在真实 delta 层完整实现。

### 9. 主动推送链路不属于本阶段结论

本报告主要记录 Gateway → Core 请求响应链路和第一阶段流式边界。

RSS / Todo 等 Core → Gateway 主动推送是否已经全部移除内部 HTTP，应以对应实现和独立完成报告为准。

不能仅根据本次流式边界改造，推断所有主动推送链路均已完成进程内迁移。

---

## 十、当前已知限制

### 1. Stream 当前主要用于解除同步等待

当前 Stream 的主要价值是：

* 提前结束 `CoreService::respond` 的同步等待；
* 避免旧完整请求总超时；
* 为未来真实增量流提供稳定边界。

它暂时不是用户可见流式输出。

### 2. TextDelta 当前不会改变最终发送结果

Gateway 当前不会使用 `TextDelta` 重建最终正文。

这是有意设计，用于避免：

* 重复正文；
* 不完整正文覆盖 final；
* Markdown 分段错误；
* provider delta 和最终 CoreResponse 不一致。

### 3. 流异常关闭需要继续观察

需要继续确认以下情况：

* producer 未发送 `Completed` 或 `Failed` 就退出；
* channel 被异常关闭；
* Core task panic；
* receiver 被 Gateway 丢弃；
* 搜索完成但 LLM 初始化失败。

这些场景应确保：

* Gateway 不误判为成功；
* 不重复补发错误；
* 日志中保留可定位信息；
* 不永久等待。

---

## 十一、测试与验证

本次改造已执行以下检查：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release --all-features
git diff --check
```

上述检查均通过。

本次未修改 shell 脚本，因此默认未新增对应脚本检查。

### 建议继续保留的回归验证

除自动化测试外，建议在实际运行环境继续验证：

1. 私聊执行 `/查 今日ai新闻`，确认不再固定在旧完整请求超时后失败。
2. 群聊执行 `/查`，确认群消息最终只发送一次。
3. 发送要求较长回答的普通聊天，确认不再被旧总超时中断。
4. 故意制造搜索失败，确认用户只收到一次明确错误。
5. 故意制造 LLM 配置错误，确认流能明确结束。
6. 检查私聊 reply cache 只写入一次。
7. 检查群聊 outbound cache 只写入一次。
8. 检查 session 中 assistant 消息没有重复。
9. 检查 Gateway 流消费没有再次套用旧 90 秒完整生命周期超时。
10. 检查流异常关闭时不会永久挂起。

---

## 十二、分支与文件状态

本次修改位于：

```text
feat/streaming-core-respond
```

以下未跟踪文件保持未纳入本次提交：

```text
docs/tasks/done/gateway-core-inprocess-call-analysis-report.md
scripts/sync_knowledge.sh
```

本次执行过程中：

* 未修改真实 `.env`；
* 未写入 token、AppSecret 或其他敏感信息；
* 未删除已有有效注释；
* 新增了必要的中文注释；
* 子 agents 仅用于只读盘点 LLM、Core 和 Gateway 三层；
* 子 agent 结论已经与主线程源码检查交叉验证。

---

## 十三、当前阶段结论

当前可以将改造状态定义为：

### 已完成

* Gateway → Core 进程内强类型调用；
* Core `Complete | Stream` 响应边界；
* 普通聊天立即返回 Stream；
* 普通聊天中的 RAG 随流式边界执行；
* `/查` 立即返回 Stream；
* 短命令继续使用 Complete；
* 旧完整请求总超时拆分；
* Gateway 进程内流消费；
* 最终正文唯一来源；
* 最小 Core 业务失败类型；
* 真实 QQ 发送成功后再写缓存；
* LLM 标准流基础类型和 collector；
* workspace 格式化、Clippy、测试和 release 构建通过。

### 尚未完成

* provider 真实 token delta 全量接入；
* `chat()` 全面改为收集统一标准流；
* `LlmStreamEvent` 向 Core `TextDelta` 持续转发；
* Gateway 增量发送或更新同一条 QQ 消息；
* 首 token 和流空闲超时；
* 完整取消传播；
* 真正的流式候选模型降级；
* 部分输出后的统一错误语义。

因此本阶段结论为：

> Gateway → Core 进程内调用及第一阶段业务流边界已经完成，旧完整请求总超时问题已经得到结构性修复；但 provider 到 Gateway 的 token 级增量链路尚未完成。

---

## 十四、后续实施建议

后续建议单独建立第二阶段任务，不与本次已稳定的进程内边界继续混改。

### 第二阶段：Provider 真实增量流

建议范围：

1. 将 OpenAI Responses 流事件转换为统一 `LlmStreamEvent`。
2. 将 Chat Completions 流事件转换为统一 `LlmStreamEvent`。
3. 将 DeepSeek、BigModel 流事件接入相同标准。
4. 让完整 `chat()` 通过 collector 收集标准流。
5. 普通聊天和 `/查` 使用 `stream_chat()`。
6. 将 `LlmStreamEvent::TextDelta` 转换为 `CoreResponseEvent::TextDelta`。
7. Core 同时累积最终完整正文，用于 session 落库和 `Completed`。
8. 未输出文本前允许候选模型降级。
9. 已输出部分文本后失败时，不自动拼接下一模型。
10. 增加 cancellation token 和分阶段超时。

### 第三阶段：用户可见增量呈现

是否实施取决于 QQ 平台能力和产品体验。

可选方案：

* 更新同一条消息；
* 按字符数和时间间隔刷新；
* 长回答合理分段；
* 仅在 `/查` 阶段发送“正在查询”状态；
* 最终使用完整 `Completed` 校验和收尾。

该阶段不应每个 token 发送一条独立 QQ 消息。

---

## 十五、最终说明

本次重构没有恢复或重新引入 Gateway → Core 的 HTTP、SSE 或 JSON chunk 传输。

当前使用的是进程内强类型调用和业务响应流。

需要避免两个错误表述：

错误表述：

```text
LLM 全链路真实流式已经完成。
```

准确表述：

```text
Gateway → Core 进程内流式响应边界已经完成；
普通聊天和 /查 已解除旧完整请求总超时；
provider 真实 token delta 的全链路接入仍属于后续阶段。
```

该实现已经解决当前最直接的长请求超时问题，同时为后续真正的 LLM 增量流提供了清晰、可测试、无需恢复 HTTP/SSE 的架构基础。
