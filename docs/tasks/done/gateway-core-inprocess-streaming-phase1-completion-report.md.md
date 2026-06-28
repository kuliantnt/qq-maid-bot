# Gateway → Core 进程内流式响应改造完成情况

## 一、改造背景

Gateway → Core 完成进程内调用重构后，原有流式响应能力被移除，Core 请求退化为等待完整业务处理结束后一次性返回。

此前 `CoreHandle::respond` 会对整个 `RustRespondService::respond(req)` 应用 `LLM_REQUEST_TIMEOUT_SECONDS`，默认超时时间为 90 秒。

这导致以下请求在联网搜索、RAG 检索、Prompt 构造或 LLM 生成时间较长时，可能在结果完成前被整体超时中断：

* 普通聊天
* 普通聊天中的 RAG 知识注入
* `/查`
* 其他耗时较长的 LLM 生成流程

本次改造的首要目标，是恢复 Gateway 与 Core 之间的进程内流式业务边界，使长耗时请求不再受“必须在 90 秒内生成完整回答”的限制。

本次改造不恢复 Gateway → Core 的 localhost HTTP、内部 SSE endpoint 或 JSON chunk DTO。

---

## 二、当前完成状态

本次已完成第一阶段的最小流式边界改造。

当前状态可以概括为：

```text
Gateway
    │
    │ CoreRequest
    ▼
CoreService::respond
    │
    ├─ Complete(CoreResponse)
    │
    └─ Stream(CoreResponseStream)
            │
            └─ 当前 producer 仍复用完整业务流程，
               最终发送 Completed(CoreResponse)
```

已经建立了正确的进程内业务流边界，但 LLM provider 的真实 token delta 尚未完整接入 Core 业务事件。

---

## 三、已完成内容

### 1. 新增 Core 完整响应与流式响应边界

`CoreService::respond` 现在返回：

```rust
CoreRespondOutput::Complete(CoreResponse)
CoreRespondOutput::Stream(CoreResponseStream)
```

两种响应形式分别用于：

* `Complete`：短命令和本地快速响应。
* `Stream`：普通聊天、`/查` 等可能耗时较长的用户可见生成流程。

Gateway 不再假设 Core 的所有请求都必须一次性返回完整 `CoreResponse`。

### 2. 普通聊天与 `/查` 立即返回 Stream

普通直接聊天和 `/查` 进入 Core 后，会先创建进程内业务流并立即向 Gateway 返回接收端。

联网搜索、RAG 检索、Prompt 构造和 LLM 调用在 Core producer 中继续执行。

因此 Gateway 不再等待完整回答生成结束后，才从 `CoreHandle::respond` 获得响应。

### 3. 拆分旧的完整请求超时语义

原有 90 秒超时继续用于短命令的完整响应流程。

流式请求不再要求：

> 搜索、LLM 调用和完整回答必须在旧的完整请求超时内全部完成。

这解决了普通聊天和 `/查` 因完整生成时间过长而被 Core 请求总超时直接夹断的问题。

### 4. 短命令继续使用 Complete

以下短命令及本地业务流程继续返回完整响应，不因本次改造全部变成流式：

* `/ping`
* `/help`
* `/todo`
* `/memory`
* 天气
* 火车
* RSS 管理
* 其他无需长时间生成的命令

`/ping` 的 `upstream_check()` 仍保留独立的完整调用语义。

### 5. 新增 Core 业务失败边界

Core 新增最小失败类型 `CoreFailureKind`。

Gateway 只接收：

* 错误类别
* 用户可见文案
* 是否可重试

Gateway 不需要依赖 Core 内部的 provider、搜索、存储或其他底层错误类型。

详细错误信息继续留在 Core 日志中。

### 6. 明确最终正文的唯一来源

在流式响应中：

* `TextDelta` 当前只被 Gateway 消费。
* Gateway 不使用 `TextDelta` 重新拼接最终正文。
* `Completed(CoreResponse)` 是最终渲染和发送的唯一权威正文来源。

这避免了增量文本与最终完整正文被重复拼接，造成答案重复发送。

### 7. Gateway 支持消费进程内业务流

Gateway 的响应处理现已支持：

```text
Complete
→ 走原完整发送路径

Stream
→ 持续消费 CoreResponseEvent
→ 等待 Completed(CoreResponse)
→ 渲染并发送最终回复
```

当前 QQ 侧仍然只发送一次最终消息，不逐 token 发送，避免群聊或私聊刷屏。

### 8. 保留现有发送和缓存语义

流式路径继续复用现有：

* `render_respond_response`
* `send_outbound_with_fallback`
* `send_group_outbound_with_fallback`
* Markdown / text fallback
* C2C 引用消息
* 群聊 outbound cache
* reply cache

reply cache 和群聊 outbound cache 只在真实 QQ 消息发送成功后写入一次。

发送失败时不记录虚假成功状态。

### 9. 初步建立 LLM 标准流类型

`qq-maid-llm` 已增加标准基础类型：

* `LlmStreamEvent`
* `LlmStream`
* 流收集 collector

这些类型用于后续将 OpenAI、DeepSeek、BigModel 等 provider 的真实增量输出统一接入 Core。

当前仅完成标准接口和基础类型，现有 provider 尚未全部切换为通过该标准流向 Core 输出真实 token delta。

### 10. 开发分支与文件控制

本次修改位于：

```text
feat/streaming-core-respond
```

以下未跟踪文件未纳入本次提交：

```text
docs/tasks/done/gateway-core-inprocess-call-analysis-report.md
scripts/sync_knowledge.sh
```

未修改真实 `.env`，未写入敏感配置或凭据。

---

## 四、当前实际调用链

### 普通聊天

```text
Gateway 收到消息
→ 构造 CoreRequest
→ CoreService::respond 识别为流式请求
→ 创建有界业务流
→ 立即返回 CoreRespondOutput::Stream
→ Core producer 执行现有完整聊天流程
    → 会话上下文
    → 成员映射
    → 长期记忆
    → RAG 检索
    → Prompt 构造
    → LLM 完整业务调用
    → session 写入
→ producer 发送 Completed(CoreResponse)
→ Gateway 渲染并发送一次最终消息
→ 发送成功后写 reply cache
```

### `/查`

```text
Gateway 收到 /查
→ 构造 CoreRequest
→ CoreService::respond 立即返回 Stream
→ Core producer 执行联网查询和 LLM 生成
→ producer 发送 Completed(CoreResponse)
→ Gateway 渲染并发送最终查询结果
```

### 短命令

```text
Gateway 收到短命令
→ CoreService::respond
→ 完整执行本地业务
→ 返回 Complete(CoreResponse)
→ Gateway 使用原发送路径
```

---

## 五、本次解决的问题

本次已经解决：

1. 普通聊天必须等待完整 LLM 回答后才能从 Core 返回的问题。
2. `/查` 必须等待搜索和完整回答全部完成后才能从 Core 返回的问题。
3. 长耗时请求容易被旧的 90 秒完整请求总超时中断的问题。
4. Gateway/Core 进程内边界只能传递完整响应的问题。
5. 流式业务失败直接暴露 Core 内部错误类型的问题。
6. 流式路径中最终正文可能重复拼接和重复发送的问题。
7. 流式发送后 reply cache 或 outbound cache 可能重复写入的问题。

---

## 六、尚未完成的内容

本次尚未完成真正的 provider token 级流式接入。

当前 Core producer 仍然复用现有完整业务流程，等待完整 `CoreResponse` 生成后发送：

```rust
CoreResponseEvent::Completed(CoreResponse)
```

因此当前业务流主要用于：

* 提前建立 Gateway → Core 响应通道。
* 解除旧完整请求总超时。
* 为后续真实增量流建立边界。

尚未完成的内容包括：

### 1. Provider 真实增量事件接入

尚未将以下 provider 的真实 token delta 全量转换为标准 `LlmStreamEvent`：

* OpenAI Responses
* OpenAI Chat Completions
* DeepSeek
* BigModel

### 2. `chat()` 统一通过标准流收集

当前 provider 尚未全部改为：

```text
stream_chat()
→ 标准 LlmStreamEvent
→ collector
→ 完整 ChatOutcome
```

现有完整聊天调用仍保留原业务实现。

### 3. Core `TextDelta` 真实输出

虽然 Core 已定义和支持 `TextDelta`，但当前普通聊天和 `/查` 尚未将 provider 的真实 token delta 持续发送到 Gateway。

### 4. 真正的流式候选模型降级

尚未完整实现：

* 未产生文本前允许切换候选模型。
* 已产生部分文本后失败，不自动拼接下一个模型回答。
* provider 中途失败的精确业务事件传播。

### 5. 取消信号完整传播

Gateway 丢弃 stream 后，Core producer 最终可以在发送事件时发现 receiver 已关闭。

但当前 producer 执行的是完整业务流程，在等待联网搜索或完整 LLM 请求期间，暂时不能保证立即取消。

后续需要将取消信号传播到：

* 联网搜索
* RAG
* LLM provider stream
* 其他耗时步骤

避免调用方已经离开后仍继续消耗模型 token。

### 6. 分阶段流式超时

后续仍需进一步区分：

* 搜索超时
* LLM 建连超时
* 首 token 超时
* 流空闲超时
* 单次平台发送超时

本次主要解决的是旧的“完整回答总超时”，尚未完成全部超时语义细分。

### 7. 用户可见增量发送

当前 QQ 侧仍然只发送一次最终结果。

尚未实现：

* 进度状态消息
* 平台消息更新
* 分批增量刷新
* 长答案分段发送

这部分不影响当前 Core 超时修复，可作为后续独立体验优化。

---

## 七、当前阶段判断

本次改造属于：

```text
第一阶段：Core/Gateway 进程内业务流边界
```

第一阶段已经完成：

* `Complete | Stream` 响应分流
* 普通聊天和 `/查` 提前返回 Stream
* 旧完整请求总超时拆分
* Gateway 业务流消费
* 最终正文唯一来源
* 最小失败边界
* 发送成功后缓存写入
* LLM 标准流基础类型

第二阶段仍待完成：

```text
第二阶段：Provider 真实增量流接入
```

第二阶段预计包括：

* provider 输出统一 `LlmStreamEvent`
* `chat()` 通过标准流收集
* 真 token delta 转换为 Core `TextDelta`
* 流式模型候选链和降级
* 首 token 与空闲超时
* 取消信号向搜索和 provider 传播
* 可选的用户可见流式展示

因此当前实现不是最终完整流式方案，但已经完成了正确且可继续扩展的进程内流式边界，并解决了当前最直接的长请求总超时问题。

---

## 八、验证结果

已执行并通过：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release --all-features
git diff --check
```

本次未修改 shell 脚本，因此未额外执行脚本检查。

测试、静态检查和 release 构建均通过。

---

## 九、后续建议

下一步建议单独建立任务，完成 provider 真实增量流接入，不与当前第一阶段混在同一次大改中。

后续任务重点应为：

1. 将现有 provider SSE 解析结果转换为统一 `LlmStreamEvent`。
2. 让完整 `chat()` 通过 collector 收集标准流。
3. 普通聊天和 `/查` 将真实 delta 转换为 Core `TextDelta`。
4. 明确部分输出后的错误和候选模型降级行为。
5. 增加 cancellation token，停止无接收方的搜索和 LLM 调用。
6. 增加首 token、流空闲和搜索阶段的独立超时。
7. 根据 QQ 平台能力决定是否增加用户可见增量更新。
