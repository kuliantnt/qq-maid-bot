# 消息并发调度架构改造

## 背景

当前 Gateway 的消息处理链路是全局串行的。

`qq-maid-gateway-rs/src/gateway/protocol.rs:run_gateway_once` 的 WebSocket 主循环收到一条消息后，在当前 `tokio::select!` 分支内同步等待 `handle_envelope` → `handle_c2c_message` / `handle_group_message` 完整执行结束，才会回到循环顶部继续读取下一条 WebSocket 消息。

完整阻塞链路：

```text
run_gateway_once (protocol.rs:88)
  └─ tokio::select! { read.next() }
       └─ handle_envelope (protocol.rs:199)
            └─ handle_c2c_message (mod.rs:333) / handle_group_message (mod.rs:163)
                 └─ RespondClient::respond_c2c / respond_group (respond.rs)
                      └─ CoreHandle::respond (service.rs:168)
                           └─ timeout { service.respond(req) }  ← 等待 LLM 完整回复
                                └─ 流式路径: start_core_response_stream (service.rs:319)
                                     └─ tokio::spawn { run_streaming_respond }
                                          └─ Gateway consume_respond_stream (mod.rs:301)
                                               └─ send_group_outbound_with_fallback  ← QQ 发送
```

关键阻塞点：

- **`handle_c2c_message`（mod.rs:333）**：私聊消息 await `respond.respond_c2c` + `consume_respond_stream` + `send_outbound_with_fallback`
- **`handle_group_message`（mod.rs:163）**：群聊消息 await `respond.respond_group` + `consume_respond_stream` + `send_group_outbound_with_fallback`
- 两条路径都在 `handle_envelope` 的同步调用链内完成，`read.next()` 要等这一切结束。

因此群聊中一次 LLM 长回复会阻塞所有其他私聊和群聊消息。

## 目标

将消息处理模型从「所有消息全局串行」改为「同会话严格串行、不同会话并发、重型 LLM 调用受全局上限控制」。

完成后应满足：

1. 群聊长响应不阻塞其他私聊/群聊。
2. 同一会话（私聊用户或群）内消息严格按接收顺序处理。
3. 不出现会话历史乱序、pending 串线、重复发送或状态覆盖。
4. 不允许无限创建任务或无限积压消息。
5. 调度器具有明确的启动、关闭、回收和异常处理机制。

## 总体设计

### 架构分层

```
┌─────────────────────────────────────────────────────────┐
│ Gateway 层（qq-maid-gateway-rs）                         │
│                                                         │
│  run_gateway_once WS 读循环                              │
│        │                                                │
│        ▼                                                │
│  handle_envelope 解析与路由                              │
│        │                                                │
│        ▼                                                │
│  MessageDispatcher（新增 dispatcher.rs）                 │
│    · 按 scope_key 创建 / 查找会话队列                     │
│    · 有界 mpsc 队列入队                                  │
│    · 单 scope 单 worker 串行                             │
│    · worker 状态机与退休协调                             │
│    · 全局活跃 worker slot 上限                           │
│    · 队列满载系统通知                                    │
│    · 关闭信号传播                                        │
│                                                         │
├─────────────────────────────────────────────────────────┤
│ Core / LLM 层（qq-maid-core + qq-maid-llm）              │
│                                                         │
│  LimitingLlmProvider（位于 qq-maid-llm/src/provider/     │
│  limiter.rs，由 Core 装配层按配置包装最终 DynLlmProvider）│
│    · chat() / stream_chat() 统一受 Semaphore 约束        │
│    · permit 覆盖完整流生命周期                           │
│    · 对调用方完全透明（实现 LlmProvider trait）           │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

**职责边界：**

| 组件                  | 归属        | 职责                                                                           |
| --------------------- | ----------- | ------------------------------------------------------------------------------ |
| `MessageDispatcher`   | Gateway     | 按 scope 排队、同会话串行、不同会话并发、worker 生命周期、消息背压、系统通知   |
| `LimitingLlmProvider` | qq-maid-llm | 在 `chat()` / `stream_chat()` 入口统一控制所有 LLM 调用并发数；Core 只负责装配 |

Dispatcher **不持有** LLM Semaphore，不感知 Core 内部的 LLM 并发限制。

### 数据流

```text
WS 读循环 (不阻塞)
        │
        ▼
MessageDispatcher.enqueue(msg)
        │
        ├─ 成功（actor 最终确认接纳）→ 消息进入 scope 队列或 retiring backlog → worker 消费
        │                                      │
        │                                      ▼
        │                              handle_c2c/group_message
        │                                      │
        │                                      ▼
        │                              RespondClient
        │                                      │
        │                                      ▼
        │                              CoreHandle::respond
        │                                      │
        │                   ┌──────── stream_respond → LimitingLlmProvider::stream_chat
        │                   │
        │                   └──────── respond        → LimitingLlmProvider::chat
        │                                                         │
        │                                                         ▼
        │                                            真正的 Provider::stream_chat / chat
        │
        └─ 队列满 → Dispatcher 系统通知通道 → send_rejection_message → QQ
```

## 实现要求

### 1. 解耦 WebSocket 读取与业务处理

**目标文件**：`qq-maid-gateway-rs/src/gateway/protocol.rs`

`run_gateway_once` 的 `tokio::select!` 中 `read.next()` 分支当前直接 await `handle_envelope`。改造后：

1. 解析 WS frame → 提取 `GatewayEnvelope`。
2. 解析业务事件 → 提取 `C2cMessage` 或 `GroupMessage`。
3. 将消息投递给 `MessageDispatcher`（见第 2 节）。
4. 立即回到 `tokio::select!` 继续读取下一条 WS 消息。

不得在 WS 读取循环中等待 LLM、Core 响应流、QQ 发送、数据库操作。

心跳（`OP_HEARTBEAT`）、重连（`OP_RECONNECT`）、关闭帧（`Message::Close`）仍留在主循环中处理，不受业务并发改造影响。

### 2. 引入统一消息调度器（MessageDispatcher）

**目标文件**：新建 `qq-maid-gateway-rs/src/gateway/dispatcher.rs`

`MessageDispatcher` 统一负责以下职责，不包含 LLM 并发控制：

- 根据 `ConversationKey`（即 `CoreRequest::scope_key()`）查找或创建会话队列
- 将消息放入对应会话的 `tokio::sync::mpsc` 有界队列
- 创建和管理会话 worker（每个 scope 一个 `tokio::spawn`）
- 控制单会话总待处理消息数（`worker queue + retiring backlog`）不超过 `CONVERSATION_QUEUE_CAPACITY`
- 控制全局活跃 worker 数量上限（`MAX_ACTIVE_CONVERSATION_WORKERS`）
- 使用有界 command channel + `oneshot` ack 串行处理 enqueue、退休与清理
- 管理 worker 空闲回收（`CONVERSATION_WORKER_IDLE_TIMEOUT_SECS`）
- 维护 worker 状态机、注册表及其串行化边界（见第 10 节）
- 管理系统拒绝通知通道（见第 8 节）
- 处理 Gateway 关闭（接收 `CancellationToken`）
- 记录调度日志

不要将零散的 `tokio::spawn` 放在 `handle_c2c_message` / `handle_group_message` 中。

C2C 和 Group 两种消息类型应转换为统一的内部消息后进入同一个调度层。当前 C2C 和 Group 处理函数在 `mod.rs` 中结构相似，可在 Dispatcher 内部用 enum 统一消息类型。

### 3. LLM 并发限制器（LimitingLlmProvider）

**目标文件**：新建 `qq-maid-llm/src/provider/limiter.rs`

LLM 并发限制属于 `qq-maid-llm` 的 provider 包装层，Core 只在装配阶段根据 `AppConfig` 包装最终的 `DynLlmProvider`。不要把实现放回 `qq-maid-core/src/provider/mod.rs` 这个兼容重导出入口。

#### 3.1 装配位置

`LlmRuntime::from_config_with_push_sink`（`qq-maid-core/src/app/mod.rs`）需要在 Core 装配阶段创建一份共享的 LLM concurrency gate，并把它同时交给 `LimitingLlmProvider` 和 `LimitingWebSearchExecutor`。建议按以下顺序组装：

1. `build_provider(&config.llm_config())`
2. `observe_provider(...)`
3. 创建共享的 `Arc<Semaphore>`，并在 `MAX_CONCURRENT_RESPONSES > 0` 时启用
4. 用同一把 gate 分别包装 `LimitingLlmProvider::new(...)` 与 `LimitingWebSearchExecutor::new(...)`
5. 注入 `AppState` 和 `RustRespondService`

`MAX_CONCURRENT_RESPONSES = 0` 时不创建 `Semaphore`，`chat()`、`stream_chat()`、`query()` 和 `query_stream()` 都直接透传。

#### 3.2 trait 实现

`LimitingLlmProvider` 必须同时实现 `chat()` 和 `stream_chat()`，两者共用同一个 `Arc<Semaphore>`。

```rust
#[async_trait]
impl LlmProvider for LimitingLlmProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let Some(semaphore) = &self.semaphore else {
            return self.inner.chat(req).await;
        };
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| LlmError::provider("LLM semaphore closed", "limiter"))?;
        let result = self.inner.chat(req).await;
        drop(permit);
        result
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        let Some(semaphore) = &self.semaphore else {
            return self.inner.stream_chat(req).await;
        };
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| LlmError::provider("LLM semaphore closed", "limiter"))?;
        let inner = self.inner.stream_chat(req).await?;
        Ok(Box::pin(PermitHoldingStream { inner, _permit: permit }))
    }

    fn name(&self) -> &'static str { self.inner.name() }
    fn model(&self) -> &str { self.inner.model() }
    fn stream_enabled(&self) -> bool { self.inner.stream_enabled() }
}
```

`PermitHoldingStream` 内部只保存 `inner: LlmStream` 和 `_permit: OwnedSemaphorePermit`；`Drop` 由字段自动释放 permit，`poll_next` 只是转发到底层流。

#### 3.3 permit 生命周期

- `chat()`：在调用 `self.inner.chat(req)` 之前获取 permit，函数返回或被取消 / 超时 drop 时自动释放。
- `stream_chat()`：在 `self.inner.stream_chat(req)` 成功后，把 permit 移入返回的 `PermitHoldingStream`，一直持有到以下任一时刻：
  - 流正常 EOF；
  - 流返回错误；
  - 调用方提前 drop 流；
  - 调用任务被取消。
- `inner.stream_chat(req)` 创建失败时，permit 会随函数返回自动释放。
- `Semaphore::acquire_owned()` 的 `AcquireError` 不能忽略，必须显式映射为错误并上抛。

#### 3.4 为什么不能依赖默认实现

不能使用 `LlmProvider::stream_chat()` 的默认实现，因为它会退回到 `chat()` 并把单次结果伪装成一条流事件，从而丢失真实增量流。

`stream_chat()` 必须直接调用 `inner.stream_chat(req)`，不得用 `inner.chat(req)` 代替。这样 `TextDelta` 仍会按真实增量输出，而不是单次返回完整正文。

#### 3.5 需要覆盖的调用面

`chat()` 继续覆盖标题、翻译、记忆、Todo、RSS 相关的结构化调用；`stream_chat()` 直接覆盖 Core 的真实流式主聊天。两个入口共用同一并发上限，避免流式主聊天绕开 limiter。

#### 3.6 流式 limiter 测试

至少覆盖以下场景：

1. 并发上限为 1 时，第一个流尚未消费完成，第二个流不得开始底层 Provider 调用。
2. 第一个流对象创建完成但尚未消费完时，permit 仍未释放。
3. 第一个流正常 EOF 后，第二个流可以开始。
4. 第一个流被提前 drop 后，permit 可以释放。
5. 流式 `TextDelta` 仍按增量产生，不退化为完整正文单次返回。
6. 非流式 `chat()` 仍受同一个并发上限控制。
7. `MAX_CONCURRENT_RESPONSES=0` 时，`chat()` 和 `stream_chat()` 都直接透传。

#### 3.7 Web Search 并发限制

`/查` 当前通过 `WebSearchExecutor::query()` / `query_stream()` 直接发起 OpenAI Responses 请求，不能因为只包装 `LlmProvider` 就绕过并发上限。需要在 Core 装配阶段再包一层 `LimitingWebSearchExecutor`，并与 `LimitingLlmProvider` 共用同一个 `Arc<Semaphore>`。

#### 3.7.1 装配与封装

- `LimitingWebSearchExecutor` 放在 `qq-maid-llm` 的 executor 包装层，Core 只负责注入共享 gate。
- `query()` 在完整调用期间持有 permit，直到返回结果或错误为止。
- `query_stream()` 在完整搜索流生命周期内持有 permit，直到正常完成、错误、取消或提前 drop。
- `MAX_CONCURRENT_RESPONSES = 0` 时，`query()` 与 `query_stream()` 也必须直接透传，不创建任何额外等待。
- 不要创建两把互不相关的 Semaphore；`/查` 和普通聊天必须合并到同一并发上限。

#### 3.7.2 需要覆盖的测试

1. 普通聊天与 `/查` 竞争同一个 `MAX_CONCURRENT_RESPONSES` 上限。
2. 两个 `/查` 同时发起时受同一并发上限控制。
3. 搜索流提前取消、提前 drop 或返回错误后，permit 能立即释放。
4. `MAX_CONCURRENT_RESPONSES = 0` 时，`query()` 与 `query_stream()` 都直接透传。

### 4. ConversationKey：直接复用 scope_key

**已存在**：`qq-maid-core/src/service.rs:429`

```rust
impl CoreRequest {
    pub fn scope_key(&self) -> String {
        match &self.conversation {
            CoreConversation::Private { peer_id } => format!("private:{peer_id}"),
            CoreConversation::Group { group_id } => format!("group:{group_id}"),
        }
    }
}
```

- `"private:{peer_id}"` — 同一私聊用户的所有消息进入同一队列
- `"group:{group_id}"` — 同一群的所有消息进入同一队列
- 私聊和群聊天然隔离（不同前缀）

`scope_key` 是 `SessionStore` 使用的会话边界（`sessions.scope_key` 列），直接复用可保证调度语义与存储语义一致。

Gateway 在 `respond.rs` 中调用 `core_request_from_c2c_message` / `core_request_from_group_message` 后即可通过 `CoreRequest::scope_key()` 获得 key，不需要在 Gateway 层重新定义。

### 5. 每个会话使用有界消息队列

每会话维护一个 `tokio::sync::mpsc::channel(CONVERSATION_QUEUE_CAPACITY)` 队列。

每会话同时只允许一个 worker 消费消息（单 `tokio::spawn`，循环 recv → 处理 → recv）。

要求：

- 同会话严格按入队顺序处理（FIFO mpsc）
- 当前消息完成（业务处理 + LLM + QQ 发送 + 错误处理）后才处理下一条
- 不同会话的 worker 可以并发运行
- 不允许单会话无限积压
- 队列满时行为见第 8 节

### 6. 不同会话并发处理

可并发场景：

- 群聊 A 与私聊 B 并发
- 群聊 A 与群聊 B 并发
- 私聊 A 与私聊 B 并发
- 普通聊天与另一会话的本地命令并发

不因等待 LLM、消费流式响应、网络发送、摘要、自动标题而阻塞其他会话。

### 7. 轻型命令不得被 LLM 并发池阻塞

LLM Semaphore 位于真正调用模型的 `chat()` / `stream_chat()` 入口，不经过这两个入口的本地命令天然不受影响：

- `/ping` — 本地诊断，在 `handle_c2c_message` 中提前返回（mod.rs:358），不调用 provider
- `/todo` 本地查询、列表、完成、删除 — 纯 SQLite 操作，不调用 LLM
- `/记忆` 不带参数查看列表 — 纯 SQLite 操作
- `/恢复` / `/resume` 无参数列表 — 纯 SQLite 操作
- `/help` — 本地帮助文本
- `/rss` 订阅列表/删除 — 本地操作
- `/state` — 本地状态查询

即使消息进入 Dispatcher 队列并被 worker 处理，这些命令的调用链也不会请求 LLM permit。

### 8. 消息背压与满载策略

#### 8.1 三层资源边界

| 限制                      | 配置                              | 默认值 | 所属模块 |
| ------------------------- | --------------------------------- | ------ | -------- |
| 单会话队列容量            | `CONVERSATION_QUEUE_CAPACITY`     | 16     | Gateway  |
| 全局活跃 worker slot 上限 | `MAX_ACTIVE_CONVERSATION_WORKERS` | 64     | Gateway  |
| 全局 LLM 调用并发         | `MAX_CONCURRENT_RESPONSES`        | 4      | Core     |

`MAX_ACTIVE_CONVERSATION_WORKERS` 由独立的 `Arc<Semaphore>` slot 池保证；活跃 worker 计数器只用于日志和指标，不作为唯一正确性机制。`MAX_CONCURRENT_RESPONSES` 仍由 Core 侧的另一把 Semaphore 独立控制，两者不能混用。

单会话总待处理消息数只有一套容量语义：`worker queue` 中的消息数加上 `Dispatcher retiring backlog` 中的消息数，二者之和不得超过 `CONVERSATION_QUEUE_CAPACITY`。scope 从 `Active` 切换到 `Retiring` 时，不能因此获得额外的无界缓存空间；也不新增单独的 backlog 容量配置。达到容量时，继续复用现有的系统拒绝通知机制。

#### 8.2 队列满载处理

**问题**：队列满时需要向 QQ 发送「当前消息较多」提示，但 WS 读取循环不能等待 QQ 网络发送。

**方案**：`MessageDispatcherHandle` 只持有有界 `command_tx`，而 `DispatcherActor` 独占 scope 注册表、backlog、worker slot 和退出观察。系统通知通道仍由 actor 内部持有，但它只负责在调度结果已经明确后发送拒绝提示。

```rust
struct MessageDispatcherHandle {
    command_tx: mpsc::Sender<DispatcherCommand>,
}

struct DispatcherActor {
    scopes: HashMap<ConversationKey, ScopeState>,
    backlog: HashMap<ConversationKey, VecDeque<InboundMessage>>,
    worker_slots: Arc<Semaphore>,
    reject_tx: mpsc::Sender<RejectNotification>,
    // 仅用于观察 worker 退出与退出原因，不对外共享
    worker_watchers: HashMap<ConversationKey, JoinHandle<()>>,
}

struct RejectNotification {
    target: QqReplyTarget,  // C2cReplyTarget 或 GroupReplyTarget
    message: String,
}
```

`DispatcherActor` 只做调度裁决、状态迁移和通知投递，不执行网络、LLM 或数据库操作；通知发送可以由单独的后台 worker 承担，但它仍然只消费有限的有界通道。

通道容量建议与 `MAX_ACTIVE_CONVERSATION_WORKERS` 一致，满载时行为：

1. **拒绝通知通道未满**：正常入队通知 worker → 发送「当前消息较多，请稍后再试」。
2. **拒绝通知通道已满**：不创建新任务，记录结构化警告日志（scope_key、队列长度、容量、原因），递增 `reject_dropped` 计数器。
3. **禁止**：在此路径上无限 `tokio::spawn`、静默丢弃、伪造成功。

通知 worker 与业务 worker 使用同一个 `QqApiClient` 和 `GatewayRuntimeStatus`。

#### 8.3 活跃 worker 满载处理

当活跃 worker slot 不足时：

1. 已有 scope 的消息**仍可入队**，只要对应 worker 已经存在且状态不是 `Closed`。
2. 新 scope 的消息先通过 `try_acquire_owned()` 获取 slot；失败时直接拒绝创建 worker，并走系统通知通道返回错误。
3. worker 正常退出、取消或 panic 时，其 `OwnedSemaphorePermit` 自动释放，slot 回到池中。
4. 活跃 worker 计数器只记录当前数量和上限，不参与正确性判断。
5. 该 slot 池只管会话 worker，上游 `MAX_CONCURRENT_RESPONSES` 仍由 Core 的 LLM Semaphore 独立控制。

### 9. Dispatcher 内部系统通知机制

通知 worker 生命周期：

- Dispatcher 创建时启动，绑定 Dispatcher 的 `CancellationToken`
- 循环从 `reject_tx` 接收 `RejectNotification`
- 对每条通知调用 `send_c2c_text_with_status` / `send_group_text_with_status`
- 通知发送失败记录 warning，不重试
- Dispatcher 关闭时收到 cancel，停止接收新通知，drain 已有通知或超时取消

### 10. worker 生命周期管理

本设计明确采用**单一 Dispatcher actor 独占注册表**的路线：

- `MessageDispatcherHandle` 对外只暴露 `enqueue()`（或等价的 `await` 入口）；
- `MessageDispatcherHandle` 只持有有界 `command_tx`，不持有共享注册表或 worker 状态；
- `DispatcherActor` 独占 `HashMap<ConversationKey, ScopeState>`、backlog、worker slot 和退出观察状态；
- 命令先进入有界 command channel，再由 actor 返回 `oneshot` ack；只有 ack 表示消息已进入 Active worker queue 或 Retiring backlog，才算最终接纳成功；
- actor 内不得执行网络、LLM 或数据库操作，也不得对 worker 队列或拒绝通知队列执行可能长期等待的 `send().await`；
- enqueue、空闲退休、worker 退出、worker panic、shutdown 都通过同一条命令通道串行处理；
- command channel、worker queue、reject channel 均必须是有界通道；`enqueue` 需要非阻塞投递或明确短超时策略；
- `oneshot` sender 被关闭、actor 退出或 ack 超时时，都必须返回明确的 `DispatcherUnavailable` / `Shutdown` 错误，不能永久等待；
- 一个 scope 满载不得阻塞其他 scope 的调度。

这样可把以下操作放进同一个串行化边界：

1. 查找 scope 注册项；
2. 判断 worker 是否仍接受新消息；
3. 向该 worker 队列入队；
4. worker 从 `Active` 切换到 `Retiring`；
5. 从注册表删除 worker；
6. 为该 scope 创建新 worker。

#### 10.1 状态机

每个 scope 只有一个注册表条目，状态为：

```text
Active   -> 接受新消息，拥有唯一对外可见 sender
Retiring -> 不再接受新消息；旧 worker 等待完成退休或退出；新消息先进入 dispatcher 侧 backlog
Closed   -> 无存活 worker；条目可删除或在同一 actor 轮次内重建
```

可继续保留单调递增的 `worker_generation` 作为日志和事件关联字段，但它**不能**单独承担正确性保证；真正的正确性来自 Dispatcher actor 的串行协议。

#### 10.2 创建、复用与 slot 获取

- 新 scope 首条消息到达时，Dispatcher 先对 worker slot 池执行 `try_acquire_owned()`。
- 获取成功后才创建 worker，并把 `OwnedSemaphorePermit` 与 worker 生命周期绑定。
- 获取失败时，不创建 worker，直接走系统通知通道返回“当前会话较多，请稍后再试”。
- 已存在 `Active` worker 的 scope 直接复用原 worker。
- `Retiring` scope 的新消息不会直接发给旧 sender，而是进入 dispatcher 侧 backlog，等待旧 worker 真正关闭后由 successor worker 继续处理；backlog 与 worker queue 共用同一套 `CONVERSATION_QUEUE_CAPACITY` 语义，不新增第二套容量。

#### 10.3 空闲退休协议

worker 空闲检测仍可基于 `tokio::time::timeout(CONVERSATION_WORKER_IDLE_TIMEOUT_SECS)`，但 timeout 触发后**不能直接自行删除注册表并退出**。

正确流程：

1. worker 超时后向 Dispatcher 发送 `WorkerIdleExpired { scope_key, generation }`。
2. Dispatcher 在同一串行边界内检查该 scope：
   - 若状态仍为 `Active`，且当前 sender/代次与事件匹配，则切换为 `Retiring`；
   - 同时把注册表中的“可接受新消息 sender”移除或标记为不可用，保证后续 enqueue 不能再绕过退休状态；
   - 若这时已经有 backlog，则只保留 backlog，不再让旧 worker 接受新消息。
3. Dispatcher 回发退休决定给 worker：
   - `RetireNow`：worker 结束循环并退出；
   - `StayActive`：说明期间已有新活动或事件已过期，worker 继续工作。
4. 旧 worker 报告 `WorkerExited` 后，Dispatcher 若发现该 scope backlog 非空，则在**同一 actor 轮次**内申请 / 复用 slot 并创建 successor worker，再把 backlog 按原始 FIFO 顺序灌入新队列。
5. backlog 为空时，再把条目标记为 `Closed` 并删除。
6. 若 backlog 满载，则复用第 8 节的系统拒绝通知通道返回明确拒绝，而不是给 scope 额外扩容。

这组约束保证：

- 返回入队成功的消息，一定已进入活跃 worker 队列或 Dispatcher 自己维护的 backlog，后续一定会绑定消费者；
- `Retiring` worker 不再接受新消息；
- 新旧 worker 不会同时消费同一 scope；
- sender clone 不会绕过 retirement 状态，因为真正对外暴露 sender 的只有 Dispatcher；
- worker 退出与新消息同时发生时，enqueue 与退休决定在同一 actor 内串行裁决，不会出现“报告成功但消息丢失”；
- backlog 始终受同一 `CONVERSATION_QUEUE_CAPACITY` 约束，scope 从 `Active` 切到 `Retiring` 不会得到额外无界缓存。

#### 10.4 Worker panic、取消与清理

- 不再使用“下一条消息 `send()` 失败后再发现 worker 已死”的延迟清理方案。
- 每个 worker 创建后都要有明确的结束观察者，可采用 supervisor await `JoinHandle`，或等价的 join 观察任务。
- task panic 后，`JoinHandle.await` 返回 `JoinError`；应使用 `JoinError::is_panic()` 区分 panic，与 cancelled 不是同一状态。
- supervisor 收到 worker 结束事件后，在 Dispatcher actor 内完成以下清理：
  - 从注册表移除或关闭对应条目；
  - 递减活跃 worker 指标；
  - 释放 / 丢弃 worker 持有的 slot permit；
  - 若该 scope backlog 非空，则创建 successor worker 继续处理；
  - 记录 panic / cancel / normal exit 的结构化日志。

因为 slot permit 绑定在 worker 任务持有的 `OwnedSemaphorePermit` 上，正常退出、任务取消和 panic unwind 都会自动释放 permit；Dispatcher 侧只负责把注册表和指标清理干净。

### 11. 关闭与重连语义

#### 11.1 临时 WebSocket 断线与自动重连

`gateway::run`（mod.rs:94）中的重连 loop 结构：

```rust
loop {
    match run_gateway_once(...).await {
        Ok(()) | Err(_) => {
            sleep(reconnect_delay()).await;
            // 重连，复用同一个 MessageDispatcher
        }
    }
}
```

- `MessageDispatcher` 在 `run` 中创建一次，不随 `run_gateway_once` 重复创建
- 断线期间已入队任务继续执行（worker 仍存活）
- QQ 发送会失败（WebSocket 断开），worker 正常处理错误并继续
- 重连后新消息继续入队

#### 11.2 正常进程退出（Ctrl+C）

`main.rs` 中 `gateway_handle.abort()` 改为通过 `CancellationToken` 触发 graceful shutdown：

1. 收到 Ctrl+C → 触发 `shutdown_token.cancel()`
2. `MessageDispatcher` 停止接收新消息（`enqueue` 返回 `Err(Shutdown)`）
3. WS 主循环中的 `handle_envelope` 收到 shutdown 后不再投递
4. 启动 **drain 阶段**：
   - 已入队任务继续处理
   - 最大等待 `SHUTDOWN_DRAIN_TIMEOUT_SECS`（建议常量 10 秒）
   - Drain 期间：记录每完成一条消息递减计数
5. Drain 超时后：
   - 向每个 worker 发送取消（通过各 scope 的 `CancellationToken` 或 drop sender）
   - 等待 worker `JoinHandle` 完成（带短超时，如 1 秒）
   - 记录未完成任务数量和剩余活跃 worker 数量
6. 不允许关闭过程无限等待 LLM、流消费或 QQ 网络请求

#### 11.3 shutdown 常量

```rust
/// 正常退出时，等待已入队任务完成的最大时间。
const SHUTDOWN_DRAIN_TIMEOUT_SECS: u64 = 10;
/// drain 超时后，等待单个 worker 退出的最大时间。
const WORKER_CANCEL_TIMEOUT_SECS: u64 = 1;
```

### 12. 保持流式响应能力

**现状**：已打通 `CoreHandle::respond` 流式路径（`service.rs:168-206`），Gateway 通过 `consume_respond_stream`（`mod.rs:301`）消费 `CoreResponseStream`。

**要求**：

- 不同会话可同时消费各自的响应流（不同 worker 并发）
- 同一会话仍只消费一条响应流（单 worker 串行保证）
- 一流失败不影响其他会话
- 流结束后才允许同会话下一条消息开始
- 跨会话流事件不混合
- 不退回非流式实现

### 13. reply cache 隔离

#### 13.1 当前实现

`MessageCache`（`mod.rs:58`）当前是 `HashMap<String, String>`，仅以 `message_id` 为 key。

`resolve_signals`（`mod.rs:81`）：

- 将每条消息的 `message_id → content` 写入 cache
- 当 reply 引用了已知 `message_id` 时，回填 `reply.content`

#### 13.2 修正后的 key 设计

除非能找到 QQ 官方协议对 `message_id` 跨所有私聊和群聊全局唯一的明确保证，否则 reply cache 必须改为组合 key：

```rust
struct ReplyCacheKey {
    conversation_kind: ConversationKind,
    scope_id: String,
    message_id: String,
}
```

其中：

- `conversation_kind` 区分私聊 / 群聊；
- `scope_id` 直接复用 `scope_key()` 的身份空间；
- `message_id` 只在同一 scope 内做消息定位。

也可以等价实现为 `(scope_key, message_id)`；关键点是**命名空间隔离由 key 负责，不是由 `Mutex` 负责**。

#### 13.3 并发与存储约束

- 并发改造后需将 cache 包裹为 `Arc<Mutex<HashMap<ReplyCacheKey, String>>>`。
- `Mutex` 只负责并发读写安全，不负责跨 scope 隔离。
- `resolve_signals` 在 worker 线程中调用，每次短暂获取锁后释放。
- 淘汰策略可保持现状，或后续增加简单上限；但不能回退到只按 `message_id` 建 key。

#### 13.4 回归测试要求

- 不同私聊使用相同测试 `message_id` 不互相命中
- 不同群使用相同测试 `message_id` 不互相命中
- 私聊和群聊使用相同测试 `message_id` 不互相命中
- 同 scope 引用消息仍能正确回填

### 14. 排查共享状态安全性

**关键共享状态及当前锁模型**：

| 状态                   | 位置                                      | 锁模型                         | 并发改造要求                                           |
| ---------------------- | ----------------------------------------- | ------------------------------ | ------------------------------------------------------ |
| `SessionStore`         | `storage/session.rs`                      | `Mutex<Connection>` (std)      | ✅ 已有锁，不跨 .await 持有                            |
| `MemoryStore`          | `storage/memory.rs`                       | `Mutex<Connection>` (std)      | ✅ 同上                                                |
| `TodoStore`            | `storage/todo.rs`                         | `Mutex<Connection>` (std)      | ✅ 同上                                                |
| `RssStore`             | `storage/rss.rs`                          | `Mutex<Connection>` (std)      | ✅ 同上                                                |
| `KnowledgeStore`       | `storage/knowledge.rs`                    | `Mutex<Connection>` (std)      | ✅ 同上                                                |
| `reply_cache`          | `mod.rs` `HashMap<ReplyCacheKey, String>` | 无锁                           | ⚠️ 需改为 `Arc<Mutex<HashMap<ReplyCacheKey, String>>>` |
| `group_outbound_cache` | `mod.rs`                                  | `Arc<Mutex<BotOutboundCache>>` | ✅ 已有锁，但需检查 C2C 路径是否意外共享               |
| `GatewayRuntimeStatus` | `ping/mod.rs`                             | 内部 `Mutex`                   | ✅ 已有锁                                              |
| `MessageDedupe`        | `dedupe.rs`                               | 内部 `Mutex<HashMap>`          | ✅ 已有锁                                              |
| `GroupCooldowns`       | `group_filter.rs`                         | 内部 `Mutex`                   | ✅ 已有锁                                              |
| `ResumeState`          | `protocol.rs` 局部                        | 无锁                           | ⚠️ 需确认只在 WS 主循环中修改（主循环仍串行，安全）    |
| `UpstreamStatus`       | `qq-maid-llm`                             | 内部 `RwLock`                  | ✅ 已有锁                                              |

**特别排查**：

- **自动标题后台任务**（`chat_flow.rs:323`）：已经通过 `tokio::spawn` 独立运行，调用 `session_store.update_title_if_current` 使用条件更新。并发改造后该后台任务仍可与同一 scope 的 worker 并发运行（worker 处理下一条消息时，之前的 auto title 可能还在跑）。`update_title_if_current` 的条件更新（只覆盖 `DEFAULT_SESSION_TITLE`）已提供保护。

- **session history 读写**：同 scope 内部由 worker 串行保证无快照覆盖。后台自动标题写入通过条件更新保护。

- **SQLite 锁范围**：所有 `Mutex<Connection>` 都是 `std::sync::Mutex`，只在同步 SQL 操作期间持有。当前代码无跨 `.await` 持有情况，改造时保持不变。

### 15. 排查 SQLite 锁范围

当前 SQLite 使用 `std::sync::Mutex<Connection>`（`storage/database.rs:33`），改造不要求变更连接池。

检查要点（`qq-maid-core/src/storage/` 下所有 `connection()` 调用点）：

- session、memory、todo、rss、knowledge 的 `connection()` 返回 `MutexGuard` 后，都在同步代码块内使用
- 不存在跨 `.await` 持有 `MutexGuard` 的情况
- 如果改造后并发 worker 增多导致数据库锁竞争成为新瓶颈，应记录而非擅自重构

### 16. 增加可观测性

在 `MessageDispatcher` 中增加结构化日志（使用 `tracing`），至少记录：

- 活跃会话 worker 数量（创建 +1，退出 -1）
- 当前全局 worker 数与上限
- 单会话队列长度（入队 / 出队时可选记录）
- 队列满拒绝次数（`reject_total` 计数器）
- 系统通知丢弃次数（`reject_dropped` 计数器）
- worker 创建、退出、panic 恢复
- 关闭时剩余任务和 worker 数量

在 `LimitingLlmProvider` 中增加日志：

- 当前等待 permit 的任务数（`semaphore.available_permits()` 反推）

日志中不输出完整消息正文或用户 openid（复用 `mask_openid` 脱敏）。

## 配置要求

### Core 配置

新增到 `qq-maid-core/src/config.rs` 的 `AppConfig`：

```env
# 全局 LLM 调用最大并发数；0 表示不限制
MAX_CONCURRENT_RESPONSES=4
```

| 属性     | 值                                                                                         |
| -------- | ------------------------------------------------------------------------------------------ |
| 默认值   | 4                                                                                          |
| 最小值   | 0（不限制）                                                                                |
| 最大值   | 256（硬上限，防止误配置）                                                                  |
| 0 的语义 | 不启用并发限制。内部使用 `Option<Semaphore>`，0 时 `semaphore = None`，`chat()` 直接透传。 |
| 非法值   | 启动时返回配置错误（如负数、超过 256）                                                     |

### Gateway 配置

新增到 `qq-maid-gateway-rs/src/config/mod.rs` 的 `AppConfig`：

```env
# 单会话消息队列容量
CONVERSATION_QUEUE_CAPACITY=16
# 全局活跃会话 worker 最大数量
MAX_ACTIVE_CONVERSATION_WORKERS=64
# 会话 worker 空闲回收超时（秒）
CONVERSATION_WORKER_IDLE_TIMEOUT_SECS=300
```

| 配置                                    | 默认值 | 最小值 | 最大值 | 0 的语义       |
| --------------------------------------- | ------ | ------ | ------ | -------------- |
| `CONVERSATION_QUEUE_CAPACITY`           | 16     | 1      | 256    | 非法，启动报错 |
| `MAX_ACTIVE_CONVERSATION_WORKERS`       | 64     | 1      | 1024   | 非法，启动报错 |
| `CONVERSATION_WORKER_IDLE_TIMEOUT_SECS` | 300    | 10     | 3600   | 非法，启动报错 |

更新 `runtime/config/.env.example` 添加以上配置项及注释。

配置缺失时使用默认值，不阻止程序启动。

## 必须保持的现有行为

- 同会话消息顺序不变
- 会话历史顺序不变（`SessionStore` 的 `history` 追加写入）
- pending 只能由正确 scope 和正确用户继续（`pending.rs` 的 scope 校验）
- C2C 和群聊身份识别逻辑不退化（`respond.rs` 的 `core_request_from_*`）
- 流式响应继续工作（`CoreResponseStream` / `consume_respond_stream`）
- 自动标题不覆盖手工标题（`update_title_if_current` 条件更新）
- 现有命令语义不变
- 不修改与消息调度无关的业务功能

## 禁止事项

- 不要在 WS 入口无界 `tokio::spawn` 每条消息
- 不要自己实现线程池
- 不要使用 `std::thread::spawn` 处理完整消息
- 不要使用 `spawn_blocking` 承载完整异步响应链路
- 不要允许无限消息积压
- 不要让同一会话多任务并行修改历史
- 不要用全局 Mutex 包住完整消息处理
- 不要持有同步锁跨 `.await`
- 不要在 WS 主循环中直接 await QQ 发送（包括拒绝通知）
- 不要让拒绝通知通道自身无限积压
- 不要让零 permit 的 Semaphore 导致所有 LLM 请求永久等待
- 不要进行无关大规模重构
- 不要伪造测试结果

## 关键源码索引

| 组件               | 文件                                              | 关键函数/结构                                                                              |
| ------------------ | ------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| 主入口             | `src/main.rs`                                     | `main`, `gateway_handle`                                                                   |
| Core 启动          | `qq-maid-core/src/app/`                           | `LlmRuntime::from_config_with_push_sink`                                                   |
| Gateway 启动       | `qq-maid-gateway-rs/src/app/mod.rs`               | `run_with_config`                                                                          |
| 主循环             | `qq-maid-gateway-rs/src/gateway/mod.rs`           | `run` (line 94)                                                                            |
| WS 循环            | `qq-maid-gateway-rs/src/gateway/protocol.rs`      | `run_gateway_once` (line 88)                                                               |
| 事件分发           | `qq-maid-gateway-rs/src/gateway/protocol.rs`      | `handle_envelope` (line 199)                                                               |
| 私聊处理           | `qq-maid-gateway-rs/src/gateway/mod.rs`           | `handle_c2c_message` (line 333)                                                            |
| 群聊处理           | `qq-maid-gateway-rs/src/gateway/mod.rs`           | `handle_group_message` (line 163)                                                          |
| 流消费             | `qq-maid-gateway-rs/src/gateway/mod.rs`           | `consume_respond_stream` (line 301)                                                        |
| Reply cache        | `qq-maid-gateway-rs/src/gateway/mod.rs`           | `resolve_signals` (line 81), `MessageCache`, `ReplyCacheKey`                               |
| Gateway→Core       | `qq-maid-gateway-rs/src/respond.rs`               | `RespondClient`, `core_request_from_*`                                                     |
| Core 入口          | `qq-maid-core/src/service.rs`                     | `CoreHandle::respond` (line 168)                                                           |
| 流式起点           | `qq-maid-core/src/service.rs`                     | `start_core_response_stream` (line 319)                                                    |
| scope_key          | `qq-maid-core/src/service.rs`                     | `CoreRequest::scope_key` (line 429)                                                        |
| LLM Provider trait | `qq-maid-llm/src/provider/mod.rs`                 | `LlmProvider` (line 60), `chat()`, `stream_chat()`                                         |
| Limiter 装配       | `qq-maid-core/src/app/mod.rs`                     | `LlmRuntime::from_config_with_push_sink`                                                   |
| Limiter 实现位置   | `qq-maid-llm/src/provider/limiter.rs`             | `LimitingLlmProvider`, `PermitHoldingStream`                                               |
| Provider 调用      | `qq-maid-core/src/runtime/respond/llm_service.rs` | `LlmChatService::respond` → `provider.chat()`，`stream_respond` → `provider.stream_chat()` |
| 自动标题 LLM       | `qq-maid-core/src/runtime/respond/title.rs`       | `provider.chat()` (line 51)                                                                |
| 翻译 LLM           | `qq-maid-core/src/runtime/translation.rs`         | `self.provider.chat()` (line 145)                                                          |
| RSS 翻译 LLM       | `qq-maid-core/src/runtime/rss/scheduler.rs`       | `translation_service.translate()` (line 335)                                               |
| Gateway 配置       | `qq-maid-gateway-rs/src/config/mod.rs`            | `AppConfig`                                                                                |
| Core 配置          | `qq-maid-core/src/config.rs`                      | `AppConfig` (line 120)                                                                     |
| 全局状态           | `qq-maid-core/src/http/routes.rs`                 | `AppState` (line 38)                                                                       |
| SQLite 封装        | `qq-maid-core/src/storage/database.rs`            | `SqliteDatabase`, `Mutex<Connection>`                                                      |
| 自动标题           | `qq-maid-core/src/runtime/respond/chat_flow.rs`   | `tokio::spawn` (line 323)                                                                  |
| 去重               | `qq-maid-gateway-rs/src/gateway/dedupe.rs`        | `MessageDedupe`                                                                            |
| 冷却               | `qq-maid-gateway-rs/src/gateway/group_filter.rs`  | `GroupCooldowns`                                                                           |
| 运行时状态         | `qq-maid-gateway-rs/src/gateway/ping/mod.rs`      | `GatewayRuntimeStatus`                                                                     |
| 环境变量模板       | `runtime/config/.env.example`                     | —                                                                                          |

## 验收标准

### 核心用户故事

1. 群聊 A 正在生成长回复时，用户私聊发送 `/ping`，私聊能立即处理。
2. 群聊 A 正在生成长回复时，用户私聊发送 `/todo`，待办命令可独立处理。
3. 群聊 A 发生 LLM 超时或错误，不影响其他私聊和群聊。
4. 两个不同群可同时处理普通聊天。
5. 两个不同私聊可同时处理消息。

### 会话顺序

1. 同一 scope 连续两条消息，第二条等待第一条完整处理。
2. 同一 scope 回复顺序与接收顺序一致。
3. 不出先后发消息先写入历史。
4. 不出同一 scope 多响应流交错发送。

### 会话隔离

1. 不同 scope 历史不串线。
2. 私聊与群聊历史不串线。
3. pending 不被其他 scope 消费。
4. reply cache 通过组合 key 隔离，不再依赖 `message_id` 全局唯一性假设。

### 并发限制

1. 同时运行的重型 LLM 调用不超过 `MAX_CONCURRENT_RESPONSES`（0 时不限制）。
2. 同时活跃的会话 worker 不超过 `MAX_ACTIVE_CONVERSATION_WORKERS`。
3. 等待 LLM permit 不阻塞 WS 继续读取消息。
4. 同一 scope 积压消息不提前占用多个 LLM permit；流式调用也必须持有 permit 直到流结束、错误、drop 或取消。
5. 轻型本地命令不被 LLM Semaphore 阻塞（不经过 `chat()` / `stream_chat()`）。

### 背压与资源

1. 单会话总待处理消息数（worker queue + retiring backlog）不超过 `CONVERSATION_QUEUE_CAPACITY`。
2. 队列满时通过系统通知通道发送提示；通知通道自身满载时记录日志和计数，不死循环。
3. 空闲 worker 能在 `CONVERSATION_WORKER_IDLE_TIMEOUT_SECS` 后回收。
4. 大量不同 scope 瞬间涌入时，新 scope 受 `MAX_ACTIVE_CONVERSATION_WORKERS` 限制，已有 scope 不受影响。
5. Gateway 重连不重复创建 Dispatcher 或 worker。

### 数据一致性

1. 不出旧 session 快照覆盖新历史。
2. 自动标题不覆盖手工标题。
3. 失败请求不被记为成功。
4. 消息不重复回复。
5. 数据库锁不跨 `.await` 持有。

### 关闭与重连

1. 临时 WebSocket 断线时 Dispatcher 保持存活，已入队任务继续执行或明确失败。
2. 正常退出时：停止接收新消息 → drain 已入队任务（等待 `SHUTDOWN_DRAIN_TIMEOUT_SECS`）→ 超时取消剩余 worker → 记录未完成任务数。
3. 关闭过程不无限等待 LLM、流消费或 QQ 网络请求。

### worker 生命周期竞态

1. worker 从 `Active` 切到 `Retiring` 后，不再接受新消息；成功入队的消息要么进入活跃队列，要么进入 Dispatcher backlog，且两者总和始终受 `CONVERSATION_QUEUE_CAPACITY` 约束。
2. backlog 满时返回明确拒绝，并继续复用现有系统通知机制，不给 scope 新增无界缓存。
3. backlog 转移到 successor worker 后保持原始 FIFO 顺序。
4. Worker panic 后由 supervisor 通过 `JoinError::is_panic()` 触发清理与重建，不依赖下一条消息 `send()` 失败后才发现。
5. 同一 scope 不会同时存在两个 `Active` worker，sender clone 也不会绕过 retirement 状态。

## 测试要求

**必测场景**（优先使用 mock provider / channel / barrier，不依赖真实 LLM）：

- 同 scope 消息严格串行
- 不同 scope 消息并发
- 群聊长响应不阻塞私聊 `/ping`
- `chat()` 与 `stream_chat()` 共用同一个 LLM Semaphore，上限生效
- 第一个流未消费完成时，第二个流不得开始底层 Provider 调用
- 第一个流正常 EOF 后，第二个流可以开始
- 第一个流提前 drop 后，permit 可以释放
- `stream_chat()` 真实输出 `TextDelta` 增量，不退化为单次完整正文
- `MAX_CONCURRENT_RESPONSES=0` 时 `chat()` 与 `stream_chat()` 都透传
- 全局活跃 worker slot 上限生效（`MAX_ACTIVE_CONVERSATION_WORKERS`）
- 已达 worker slot 上限时已有 scope 仍可入队
- `Retiring` 状态下，worker queue 与 backlog 总量不超过 `CONVERSATION_QUEUE_CAPACITY`
- `Retiring` backlog 满载时返回明确拒绝
- backlog 转入 successor worker 后保持 FIFO
- `enqueue` 返回成功的消息最终一定存在消费者
- Dispatcher command channel 满时不会阻塞 WS 读循环，也不会伪造接纳成功
- 单会话队列满载：拒绝通知发送成功
- 单会话队列满载：拒绝通知通道满时日志计数，不死循环
- worker 空闲回收
- worker 退休与新消息入队竞态不丢消息
- worker panic 后清理 registry / slot / metrics
- Gateway 临时断线重连：Dispatcher 不重建
- Gateway 正常关闭：drain + 超时取消 + 计数
- session history 不乱序
- pending 不串线
- reply cache 跨私聊、跨群聊、私聊群聊隔离
- 同 scope reply cache 引用消息仍能正确回填
- 流式响应跨 scope 不混合

## PR 描述同步要点

- `LimitingLlmProvider` 最终放在 `qq-maid-llm/src/provider/limiter.rs`，由 `qq-maid-core` 装配层包装最终 `DynLlmProvider`。
- `chat()` 与 `stream_chat()` 都获取 permit；`stream_chat()` 必须调用真实 `inner.stream_chat(req)`。
- permit 释放时机要明确写为：`chat()` 返回、流 EOF、流错误、调用方提前 drop、任务取消。
- worker 上限由独立 worker slot Semaphore 保证，不再依赖“先读计数器再创建”的竞态方案。
- worker retirement 使用 `Active / Retiring / Closed` 状态机，并由 Dispatcher actor 串行化 enqueue、退休与清理。
- Dispatcher command channel 必须有界，`enqueue` 只有在 actor 通过 ack 确认后才算成功。
- backlog 与 worker queue 共用同一套容量语义，不新增单独 backlog 配置。
- worker panic 由 supervisor 通过 `JoinHandle.await` + `JoinError::is_panic()` 观察和清理；cancelled 与 panic 要区分。
- reply cache 使用组合 key，推荐 `ReplyCacheKey { conversation_kind, scope_id, message_id }`。
- 实际新增配置共 4 项：`MAX_CONCURRENT_RESPONSES`、`CONVERSATION_QUEUE_CAPACITY`、`MAX_ACTIVE_CONVERSATION_WORKERS`、`CONVERSATION_WORKER_IDLE_TIMEOUT_SECS`。

完成后运行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## 本次不处理

- SQLite 连接池改造（当前 `Mutex<Connection>` 保持不变）
- 分布式消息队列
- 多进程共享会话调度
- QQ 原生流式展示
- Provider 技术栈重写
- 与并发无关的业务重构
