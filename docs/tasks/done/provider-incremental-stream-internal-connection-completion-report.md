# Provider 真实增量流内部贯通完成报告

## 一、基本信息

* PR：#47
* 分支：`feat/provider-delta-stream`
* 任务阶段：LLM 流式改造第二阶段
* 改造范围：Provider → LLM → Core → Gateway 的进程内真实增量流
* 用户可见行为：QQ 侧仍只在最终 `Completed(CoreResponse)` 后发送一次完整消息

本阶段没有实现 QQ 消息逐段刷新，也没有恢复 Gateway → Core 的 HTTP/SSE 通讯。

---

## 二、改造背景

第一阶段已经完成 Gateway → Core 的进程内调用和：

```text
CoreRespondOutput::Complete
CoreRespondOutput::Stream
```

业务响应边界。

但第一阶段中的 Stream 主要还是一个异步业务通道：

```text
Core 立即返回 Stream
→ producer 执行原有完整业务流程
→ 等待 LLM 生成完整结果
→ 发送 Completed(CoreResponse)
→ Gateway 最终发送一次
```

虽然它解决了普通聊天和 `/查` 被旧完整请求总超时中断的问题，但 Provider 的真实 token/text delta 尚未向上传递。

因此第一阶段结束后，用户侧仍然表现为：

```text
等待
→ 一次性出现完整回答
```

第二阶段的目标是打通真正的内部增量链路：

```text
Provider SSE / delta
→ LlmStreamEvent::TextDelta
→ CoreResponseEvent::TextDelta
→ Gateway 持续消费
→ Completed(CoreResponse)
→ QQ 最终发送一次
```

---

## 三、本阶段目标

本次改造主要完成以下目标：

1. 为 OpenAI Responses、Chat Completions、DeepSeek 和 BigModel 建立真实标准流。
2. 普通聊天不再等待 Provider 完整结果，而是持续消费增量事件。
3. `/查` 使用真实 `query_stream()`，不再通过完整结果人工切片伪装流式。
4. Core 在转发增量的同时聚合完整正文。
5. 最终正文、会话落库和 Gateway 发送仍以唯一的 `Completed(CoreResponse)` 为准。
6. 保留已有模型候选链、协议 fallback、健康观测和完整结果调用能力。
7. 不改变 QQ Markdown/text fallback、reply cache、outbound cache 和最终发送语义。

---

## 四、整体调用链

### 改造前

```text
Provider
→ 等待完整 ChatOutcome
→ Core 获得完整 RespondOutput
→ Completed(CoreResponse)
→ Gateway 发送一次
```

Provider 层即使使用了 SSE，也通常会先在 LLM crate 内收集完整正文，再把完整结果返回 Core。

### 改造后

```text
Provider 真实 SSE
→ LlmStreamEvent::TextDelta
→ LlmChatService::stream_respond
→ CoreResponseEvent::TextDelta
→ Gateway 消费并忽略展示
→ Core 聚合同一条流的完整正文
→ session 落库和响应后处理
→ CoreResponseEvent::Completed
→ Gateway 渲染并发送一次
```

当前已经属于真正的内部流式链路。

不过 Gateway 本阶段仍不展示增量，因此用户看到的 QQ 回复依然是最终一次发送。

---

## 五、LLM 标准流改造

### 1. 统一标准流事件

LLM 层使用统一事件表达 Provider 输出：

```rust
LlmStreamEvent::TextDelta(String)

LlmStreamEvent::Completed {
    usage,
    finish_reason,
    fallback_used,
}
```

其中：

* `TextDelta` 表示模型正文增量；
* `Completed` 是每条成功流唯一的终止事件；
* usage、finish reason 和 fallback 状态随完成事件返回；
* 流错误通过 `Result::Err` 返回；
* collector 必须读取到明确完成或 EOF。

### 2. 保留完整结果调用

内部结构化任务仍然可以使用：

```rust
LlmProvider::chat()
```

获得完整 `ChatOutcome`。

适用场景包括：

* 翻译；
* 自动标题；
* Todo 解析；
* Memory 草稿；
* 健康检查；
* 其他不需要向上转发 delta 的任务。

`chat()` 和 `stream_chat()` 保持不同语义：

* `stream_chat()` 面向真实增量消费；
* `chat()` 面向完整结果和兼容性 fallback。

---

## 六、Provider 真实增量流

### 1. OpenAI Responses

OpenAI Responses 已支持：

* 解析 `response.output_text.delta`；
* 解析拒绝文本 delta；
* 记录 `response.completed`；
* 提取 completed response 中的 usage；
* 识别 `response.failed`；
* 识别 `response.incomplete`；
* 识别上游 error；
* 在没有正文 delta 时，从 completed response 回补完整正文。

回补只会在此前没有输出任何正文 delta 时发生，避免：

```text
增量正文
+
completed 中的完整正文
=
答案重复
```

### 2. Chat Completions

Chat Completions 标准流已支持：

* `choices[].delta.content`；
* completed message 兼容回补；
* usage 提取；
* `[DONE]`；
* `finish_reason`；
  -中文 UTF-8 跨 chunk 拼接；
* SSE frame 跨网络 chunk 解析。

DeepSeek 和 BigModel 继续复用同一套 OpenAI 兼容 Chat Completions 流实现，仅保留各自：

* base URL；
* API key；
  -默认模型；
* 模型前缀校验。

---

## 七、流正常完成语义

本次明确修复了“HTTP EOF 被错误视为成功”的问题。

### 1. OpenAI Responses

只有收到：

```text
response.completed
```

才允许产生成功的 `LlmStreamEvent::Completed`。

若已经产生部分正文，但未收到 `response.completed` 就发生 EOF，则返回流错误，不会把半截正文保存为成功回答。

### 2. Chat Completions

Chat Completions 必须满足以下至少一个正常完成条件：

* 收到 `[DONE]`；
* 收到兼容接口提供的有效 `finish_reason`。

通用 SSE parser 不再吞掉 `[DONE]`，而是把它保留给上层状态机判断。

如果没有 `[DONE]` 或 `finish_reason` 就发生 EOF，则视为异常结束。

### 3. Web Search

`/查` 的 Responses Web Search 只有收到：

```text
response.completed
```

才会返回成功的 `WebSearchOutcome`。

如果只收到部分 answer 后连接关闭，不会再把部分内容作为完整搜索结果发送给用户。

### 4. 已输出正文后的错误

如果错误发生在首个非空 delta 之后，错误阶段会标记为类似：

```text
stream_after_delta
```

用于区分：

* 尚未输出正文、仍可安全 fallback；
* 已经输出正文、不能切换模型或协议拼接答案。

---

## 八、Fallback 语义

### 1. 模型候选链

ModelRoute 的流式候选链遵循：

```text
尚未输出非空 delta
→ 可以尝试下一个候选模型

已经输出非空 delta
→ 不再静默切换候选
→ 当前流以错误结束
```

这避免了把两个模型的回答无提示拼接到一起。

### 2. OpenAI Responses → Chat Completions

在 `OPENAI_API_MODE=auto` 下：

* Responses 初始化失败时，可以切到 Chat Completions；
* Responses 流建立后，在首个非空 delta 前发生可恢复错误时，也可以在同一 `LlmStream` 中切到 Chat Completions；
* 一旦 Responses 已经输出正文，不再切换 Chat Completions。

fallback 成功后：

```text
Completed.fallback_used = true
```

### 3. 完整 `chat()` 的兼容重试

当 `LLM_STREAM=true`，但调用方使用完整 `chat()` 时，仍保留原有兼容逻辑：

```text
SSE 空流或可恢复读取错误
→ 补一次同 Provider 非流请求
→ 返回完整 ChatOutcome
```

该逻辑适用于 OpenAI、DeepSeek 和 BigModel。

因此翻译、标题、健康检查等完整结果任务不会因为兼容网关偶发的 SSE 抖动直接失败。

### 4. `stream=false`

当配置关闭 Provider 流式请求时：

```text
Provider 执行非流请求
→ 获得完整 ChatOutcome
→ 包装为单个 TextDelta
→ 再发送 Completed
```

因此：

* Provider 不会强制向不支持 SSE 的 endpoint 发送 `stream=true`；
* Core 仍可保持统一的进程内 Stream 边界；
* health snapshot 中的 stream 状态和实际请求行为一致。

---

## 九、普通聊天流式路径

普通聊天现在使用：

```text
LlmChatService::stream_respond()
```

执行真实 Provider 流。

该路径继续复用原有：

* session 获取和创建；
* Pending 判断；
  -成员编号和身份上下文；
  -长期 Memory 注入；
* RAG 知识检索；
  -系统 Prompt；
  -对话历史；
  -模型配置；
  -回答后处理；
* Markdown/text 双通道；
* diagnostics；
* session 历史落库。

发生 `TextDelta` 时：

1. Core 持续向 Gateway 转发；
2. Core 同时聚合完整原始正文；
3. Gateway 本阶段只消费，不展示；
4. Provider 完成后，Core 生成最终 `RespondOutput`；
5. assistant exchange 只写入一次；
6. 最终发送唯一 `Completed(CoreResponse)`。

不会出现：

```text
delta 拼接正文
+
Completed 完整正文
=
重复保存或重复发送
```

---

## 十、`/查` 真实增量流

`/查` 已改为使用：

```text
query_executor.query_stream()
```

真实消费 OpenAI Responses Web Search SSE。

调用链为：

```text
用户发送 /查
→ Core 返回进程内 Stream
→ Web Search 发起 stream=true 请求
→ 持续解析 response.output_text.delta
→ 转发 CoreResponseEvent::TextDelta
→ 收到 response.completed
→ 提取最终 answer 和 sources
→ 格式化联网查询回复
→ session 落库
→ Completed(CoreResponse)
→ Gateway 最终发送一次
```

测试中增加了可控 SSE delta 场景，验证第一个增量在完整搜索结果产生前已经到达，而不是先生成完整结果再人工切片。

---

## 十一、Gateway 行为

本阶段没有修改 Gateway 的用户可见发送策略。

Gateway 当前处理方式仍是：

```text
TextDelta
→ 持续消费
→ 不拼接最终正文
→ 不发送 QQ 消息

Completed(CoreResponse)
→ 渲染 text / Markdown
→ 调用 QQ OpenAPI
→ 成功后写入 cache
```

继续保留：

* Markdown 发送；
* text fallback；
* C2C 引用发送；
* 群聊发送；
* reply cache；
* group outbound cache；
* 真实发送成功后再写缓存；
  -发送失败不伪造成功。

因此本阶段不会造成 QQ 群聊逐 token 刷屏，也不会出现 Markdown 被任意切断的问题。

---

## 十二、取消和失败处理

Core 业务流继续支持：

```text
CoreResponseStream::cancel()
```

以及 Stream Drop 时标记取消。

普通聊天转发 delta 时，如果 receiver 已关闭：

* channel send 返回失败；
* producer 停止继续转发；
* 底层 Provider stream 被释放。

`/查` 使用独立 query task：

* Gateway/Core 接收端取消后；
* delta 转发失败；
* query task 会被 abort；
* 避免继续 poll Web Search Provider stream。

失败路径不会：

* 生成虚假 `Completed`；
* 写入成功 cache；
* 保存半截回答为成功 assistant 消息；
* 再补发一条伪造的失败消息。

当前取消传播仍属于 best-effort，后续还可以增加统一 cancellation token 和更细粒度的取消观测。

---

## 十三、健康观测和 Metrics

ObservedProvider 的流式观测已接入真实流事件。

正常完成时记录：

* Provider；
* Model；
  -总耗时；
  -首事件时间；
  -首 token 时间；
* usage；
* fallback_used。

以下情况记录失败：

* Provider 明确错误；
  -未完成 EOF；
  -流读取错误；
  -未产生 Completed。

带有：

```text
health_observation=ignore
```

的旁路任务不会覆盖主聊天健康状态，例如后台自动标题。

Core 的流聚合不再使用：

```text
total_latency_ms = 0
fallback_used = false
```

等假值，而是通过现有 `MetricsRecorder` 记录实际流式指标。

---

## 十四、自动标题异步化

自动标题原先位于主聊天响应路径中：

```text
正文生成完成
→ session 落库
→ 等待标题模型
→ 才返回 Completed
```

这会让正文已经生成完的请求继续等待另一次 LLM 调用。

现在调整为：

```text
正文生成完成
→ assistant exchange 落库
→ 返回主响应
→ 后台 best-effort 生成标题
```

标题生成：

* 不再阻塞主聊天 `Completed`；
  -失败不影响本轮聊天成功；
  -慢响应不延迟用户收到回答；
  -通过 `health_observation=ignore` 避免覆盖主模型健康状态。

---

## 十五、后台标题并发覆盖修复

自动标题改为后台执行后，曾存在旧 `SessionRecord` 快照覆盖新会话数据的风险。

风险链路为：

```text
第二轮聊天后复制 SessionRecord
→ 后台生成标题

第三、第四轮继续写入新消息

后台标题完成
→ 使用旧 SessionRecord 调用 save()
→ 全量覆盖数据库中的 history、state、pending 或 summary
```

最终修复方式是在 `SessionStore` 增加：

```rust
update_title_if_current(
    session_id,
    expected_title,
    new_title,
)
```

该方法只执行条件 SQL：

```sql
UPDATE sessions
SET title = ?,
    updated_at = ?
WHERE session_id = ?
  AND title = ?;
```

后台任务现在只持有：

* `session_id`；
  -生成标题所需的 history 快照。

生成完成后，只在当前标题仍是默认标题时更新 title。

因此：

* 不会回写完整 SessionRecord；
  -不会覆盖后续消息；
  -不会覆盖 pending；
  -不会覆盖 summary；
  -不会覆盖其他 state；
  -不会覆盖用户手工 `/rename`；
  -多个并发标题任务只有第一个有效结果可以写入。

---

## 十六、主要修改范围

本次主要涉及：

### LLM 层

```text
qq-maid-llm/src/sse.rs
qq-maid-llm/src/provider/mod.rs
qq-maid-llm/src/provider/status.rs
qq-maid-llm/src/provider/deepseek.rs
qq-maid-llm/src/provider/bigmodel.rs
qq-maid-llm/src/provider/openai/mod.rs
qq-maid-llm/src/provider/openai/chat.rs
qq-maid-llm/src/provider/openai/responses.rs
qq-maid-llm/src/provider/openai/stream.rs
qq-maid-llm/src/provider/openai/fallback.rs
qq-maid-llm/src/web_search.rs
```

### Core 层

```text
qq-maid-core/src/service.rs
qq-maid-core/src/runtime/respond.rs
qq-maid-core/src/runtime/respond/chat_flow.rs
qq-maid-core/src/runtime/respond/llm_service.rs
qq-maid-core/src/runtime/respond/search_flow.rs
qq-maid-core/src/storage/session.rs
```

### 测试

包括：

* Provider SSE 解析测试；
  -真实 delta 测试；
  -异常 EOF 测试；
* `[DONE]` 和 finish reason 测试；
* stream=false 测试；
  -完整 `chat()` 非流 fallback 测试；
  -首 delta 前 fallback 测试；
  -首 delta 后禁止 fallback 测试；
  -健康观测测试；
  -普通聊天 Core delta 测试；
* `/查` 端到端 SSE delta 测试；
* Gateway 最终只发送一次回归测试；
  -自动标题不阻塞测试；
  -自动标题旧快照不覆盖历史测试；
  -手工 rename 不被后台标题覆盖测试。

---

## 十七、验证结果

主体改造阶段已执行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p qq-maid-llm
cargo test -p qq-maid-core
cargo test --workspace --all-features
cargo build --workspace --release --all-features
git diff --check
```

全部通过。

后台标题覆盖修复阶段额外执行：

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo test -p qq-maid-core storage::session::tests::update_title_if_current --all-features
cargo test -p qq-maid-core runtime::respond::tests::session::delayed_auto_title --all-features
cargo test -p qq-maid-core --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git diff --check
```

全部通过。

最终标题修复提交未重复执行 release build，因为该提交只涉及 Core 会话字段更新和测试，没有修改依赖、配置、启动路径或发布产物；本 PR 的主体流式改造已经执行过 release 构建。

---

## 十八、未改变的行为

本次明确没有修改：

* QQ 用户可见逐段输出；
* QQ 消息更新；
* Markdown 分片；
* text fallback；
* reply cache；
* group outbound cache；
  -短命令的 Complete 路径；
* Todo；
* Memory；
* RSS；
  -天气；
  -火车；
  -翻译的完整结果语义；
* Gateway → Core 进程内调用结构。

也没有恢复：

* Gateway → Core localhost HTTP；
  -内部 SSE 传输；
  -内部 JSON chunk 协议。

---

## 十九、已知限制

### 1. QQ 侧仍不可见增量

虽然内部已经是真实流，但 Gateway 仍忽略 `TextDelta`，只发送最终 `Completed`。

因此用户侧首字体验暂时没有变化。

### 2. 尚未增加流空闲超时

当前仍主要依赖：

* reqwest 请求超时；
  -现有业务超时；
  -上游 HTTP 行为。

后续可以细分：

-建连超时；
-首 token 超时；
-流空闲超时；
-整体最大时长；
-平台发送超时。

### 3. 取消传播仍是 best-effort

当前通过：

* Stream Drop；
  -取消标记；
  -channel send 失败；
  -query task abort；

停止后续处理。

后续可以引入统一 cancellation token，使搜索、Provider 和 Core producer 更及时地响应取消。

### 4. Provider HTTP 总超时仍然存在

真实增量流解决的是内部等待和事件传递问题，不代表 Provider 请求可以无限持续。

reqwest client 仍使用现有请求超时配置。

### 5. 主动取消可能影响健康状态

部分主动取消场景仍可能被健康观测记录为 provider cancelled/timeout。

后续可以进一步区分：

-调用方主动取消；
-平台发送失败导致取消；
-真正的 Provider 故障。

---

## 二十、完成结论

PR #47 已完成第二阶段目标：

```text
Provider 真实增量流
→ LLM 标准事件
→ Core 真实 TextDelta
→ Gateway 进程内消费
→ 最终 Completed
```

当前已经完成：

* OpenAI Responses 真实流；
* Chat Completions 真实流；
* DeepSeek 真实流；
* BigModel 真实流；
  -候选模型流式 fallback；
* OpenAI Responses 协议 fallback；
  -异常 EOF 正确识别；
* stream=false 兼容；
  -完整 chat() 非流重试；
  -普通聊天真实增量；
* `/查` 真实增量；
  -真实 metrics 和健康观测；
  -自动标题异步化；
  -后台标题旧快照覆盖修复；
  -最终正文和缓存唯一写入语义。

准确的阶段描述是：

> Provider 到 Gateway 的内部真实增量流已经贯通；Gateway 当前持续消费增量，但 QQ 侧仍只在 `Completed(CoreResponse)` 后发送一次最终消息。

后续若继续实施用户可见流式，只需要在 Gateway/平台发送层设计合理的缓冲、分段或消息更新策略，不再需要重新拆改 Provider、LLM 和 Core 的流式底座。
