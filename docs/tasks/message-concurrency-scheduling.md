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

将消息处理模型从「所有消息全局串行」改为「同会话严格串行、不同会话并发、重型响应受全局上限控制」。

完成后应满足：
1. 群聊长响应不阻塞其他私聊/群聊。
2. 同一会话（私聊用户或群）内消息严格按接收顺序处理。
3. 不出现会话历史乱序、pending 串线、重复发送或状态覆盖。
4. 不允许无限创建任务或无限积压消息。
5. 调度器具有明确的启动、关闭、回收和异常处理机制。

## 总体设计

```text
run_gateway_once WS 读循环
        │
        ▼
handle_envelope 解析与路由
        │
        ▼
计算 ConversationKey（复用 CoreRequest::scope_key）
        │
        ▼
MessageDispatcher（新增）
        │
        ├── scope"private:u1" → 有界 mpsc 队列 → 单 worker 串行
        ├── scope"group:g1"   → 有界 mpsc 队列 → 单 worker 串行
        ├── scope"group:g2"   → 有界 mpsc 队列 → 单 worker 串行
        └── scope"private:u2" → 有界 mpsc 队列 → 单 worker 串行
                                      │
                                      ▼
                             全局 Semaphore（重型响应上限）
```

Tokio 多线程 Runtime 已提供异步调度能力，不自行实现线程池，也不使用 `spawn_blocking` 承载完整异步消息链路。

## 实现要求

### 1. 解耦 WebSocket 读取与业务处理

**目标文件**：`qq-maid-gateway-rs/src/gateway/protocol.rs`

`run_gateway_once` 的 `tokio::select!` 中 `read.next()` 分支当前直接 await `handle_envelope`。改造后：

1. 解析 WS frame → 提取 `GatewayEnvelope`。
2. 解析业务事件 → 提取 `C2cMessage` 或 `GroupMessage`。
3. 将消息投递给统一调度器（见第 2 节）。
4. 立即回到 `tokio::select!` 继续读取下一条 WS 消息。

不得在 WS 读取循环中等待 LLM、Core 响应流、QQ 发送、数据库操作。

心跳（`OP_HEARTBEAT`）、重连（`OP_RECONNECT`）、关闭帧（`Message::Close`）仍留在主循环中处理，不受业务并发改造影响。

### 2. 引入统一消息调度器

**目标文件**：新建 `qq-maid-gateway-rs/src/gateway/dispatcher.rs`

新增 `MessageDispatcher` 组件，统一负责：

- 根据 `ConversationKey`（即 `CoreRequest::scope_key()`）查找或创建会话队列
- 将消息放入对应会话队列
- 创建和管理会话 worker（每个 scope 一个 `tokio::spawn`）
- 控制队列容量
- 控制全局重型响应并发数（`tokio::sync::Semaphore`）
- 管理 worker 空闲回收
- 处理 Gateway 关闭（接收 `CancellationToken`）
- 记录调度日志

不要将零散的 `tokio::spawn` 放在 `handle_c2c_message` / `handle_group_message` 中。

C2C 和 Group 两种消息类型应转换为统一的内部消息后进入同一个调度层。当前 C2C 和 Group 处理函数在 `mod.rs` 中结构相似（解析 → 调用 `respond.respond_xxx` → 消费流 → 发送），可在 Dispatcher 内部用 enum 统一。

### 3. ConversationKey：直接复用 scope_key

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

`scope_key` 是 `SessionStore` 使用的会话边界（`sessions.scope_key` 列），直接复用可保证调度语义与存储语义一致，不会出现调度和 session 两套身份规则。

Gateway 在 `respond.rs` 中调用 `CoreRequest::from(C2cMessage/GroupMessage)` 后即可获得 `scope_key`，不需要在 Gateway 层重新定义。

### 4. 每个会话使用有界消息队列

每会话维护一个 `tokio::sync::mpsc::channel(capacity)` 队列。

每会话同时只允许一个 worker 消费消息（单 `tokio::spawn`，循环 recv → 处理 → recv）。

要求：
- 同会话严格按入队顺序处理（FIFO mpsc）
- 当前消息完成（业务处理 + LLM + QQ 发送 + 错误处理）后才处理下一条
- 不同会话的 worker 可以并发运行
- 不允许单会话无限积压
- 队列满时返回明确错误（如「当前消息较多，请稍后再试」），不静默丢弃

### 5. 不同会话并发处理

可并发场景：
- 群聊 A 与私聊 B 并发
- 群聊 A 与群聊 B 并发
- 私聊 A 与私聊 B 并发
- 普通聊天与另一会话的本地命令并发

不因等待 LLM、消费流式响应、网络发送、摘要、自动标题而阻塞其他会话。

### 6. 增加重型响应并发池

使用 `tokio::sync::Semaphore` 对重型 LLM 调用增加全局并发限制。

重型响应包括：
- 普通 LLM 对话（`service.respond` / `respond_stream`）
- `/查` 联网搜索（`search_flow.rs` 经 LLM）
- 翻译模型调用（`translation.rs`）
- 自动标题生成（`chat_flow.rs:323` 的 `tokio::spawn`）
- 记忆草稿提取（`memory_flow.rs`）
- 会话压缩（`compact`）
- Todo 解析 LLM 调用
- RSS 翻译 LLM 调用

**配置建议**（新增到 `runtime/config/.env.example`）：

```env
# 重型 LLM 响应全局最大并发数
MAX_CONCURRENT_RESPONSES=4
```

Semaphore permit 应在进入 LLM 调用前申请，在 LLM 调用后释放。不要让同一会话排队的多条消息提前占用多个 permit。

### 7. 轻型命令不得被 LLM 并发池阻塞

不调用 LLM 的本地命令不占用重型响应 permit：

- `/ping` — 本地诊断，在 `handle_c2c_message` 中提前返回（mod.rs:358），不进入 LLM
- `/todo` 本地查询、列表、完成、删除 — 纯 SQLite 操作
- `/记忆` 不带参数查看列表 — 纯 SQLite 操作
- `/恢复` / `/resume` 无参数列表 — 纯 SQLite 操作
- `/help` — 本地帮助文本
- `/rss` 订阅列表/删除 — 本地操作
- `/state` — 本地状态查询

**实现策略**：将 Semaphore 放在 LLM 调用的公共入口（`CoreHandle::respond` 内 `service.respond(req)` 调用之前或 `provider.chat()` 入口），而不是包住整个 `handle_c2c_message`。这样轻型命令即使消息进了队列，也不会因等待 permit 而阻塞。

### 8. 实现消息背压和满载策略

控制边界：
- 单会话队列容量（建议默认 16，新增配置 `CONVERSATION_QUEUE_CAPACITY`）
- 全局重型响应并发数（`MAX_CONCURRENT_RESPONSES`，默认 4）

队列满时：
- 对 QQ 平台发「当前消息较多，请稍后再试」
- 日志记录：scope_key、队列长度、容量、拒绝原因
- 不伪造成功、不静默丢弃

### 9. worker 生命周期管理

- 首条消息到来时创建 worker（`tokio::spawn`）
- 后续同会话消息复用已有 worker
- worker 空闲一段时间后允许回收（建议新增配置 `CONVERSATION_WORKER_IDLE_TIMEOUT_SECS`，默认 300 秒）
- worker 退出后从调度器注册表（`HashMap<String, mpsc::Sender>`）安全移除
- 新消息到来时重新创建
- worker panic 后不应导致该会话永久无法处理（在 `tokio::spawn` 外层 catch unwind）
- 不出现旧 worker 和新 worker 同时消费同一会话队列

### 10. Gateway 关闭与重连

**现状**：`main.rs` 中 `gateway_handle.abort()` 在 Ctrl+C 时直接中止 Gateway task。

**改造后**：
- `MessageDispatcher` 持有一个 `CancellationToken`，Gateway 关闭时触发
- WebSocket 重连（`run` 中 loop 重连）复用同一个 Dispatcher，不重复创建
- 关闭时：停止接收新消息 → 已入队消息可选择处理完或记录丢弃 → 通知 worker 退出
- 短时间断线（`run_gateway_once` 返回后 `run` 中 `sleep + 重连`）期间：队列中未处理的消息在重连后继续处理

### 11. 保持流式响应能力

**现状**：已打通 `CoreHandle::respond` 流式路径（`service.rs:168-206`），Gateway 通过 `consume_respond_stream`（`mod.rs:301`）消费 `CoreResponseStream`。

**要求**：
- 不同会话可同时消费各自的响应流
- 同一会话仍只消费一条响应流（单 worker 串行保证）
- 一流失败不影响其他会话
- 流结束后才允许同会话下一条消息开始
- 跨会话流事件不混合
- 不退回非流式实现

### 12. 排查共享状态安全性

**关键共享状态及当前锁模型**：

| 状态 | 位置 | 锁模型 | 并发风险 |
|------|------|--------|---------|
| `SessionStore` | `storage/session.rs` | `Mutex<Connection>` (std) | ✅ std Mutex，不能跨 .await 持有 |
| `MemoryStore` | `storage/memory.rs` | `Mutex<Connection>` (std) | ✅ 同上 |
| `TodoStore` | `storage/todo.rs` | `Mutex<Connection>` (std) | ✅ 同上 |
| `RssStore` | `storage/rss.rs` | `Mutex<Connection>` (std) | ✅ 同上 |
| `KnowledgeStore` | `storage/knowledge.rs` | `Mutex<Connection>` (std) | ✅ 同上 |
| `reply_cache` | `mod.rs` 局部 `HashMap` | 无锁，单线程持有 | ⚠️ 并发后需改为 `Mutex<HashMap>` |
| `group_outbound_cache` | `mod.rs` | `Arc<Mutex<BotOutboundCache>>` | ✅ 已有锁 |
| `GatewayRuntimeStatus` | `ping/mod.rs` | 内部 `Mutex` | ✅ 需确认所有字段 |
| `MessageDedupe` | `dedupe.rs` | 内部 `Mutex<HashMap>` | ✅ 已有锁 |
| `GroupCooldowns` | `group_filter.rs` | 内部 `Mutex` | ✅ 已有锁 |
| `ResumeState` | `protocol.rs` 局部 | 无锁，单线程持有 | ⚠️ 需确认只在主循环中修改 |
| `UpstreamStatus` | `qq-maid-llm` | 内部 `RwLock` | ✅ 已有锁 |

**特别排查**：

- **自动标题后台任务**（`chat_flow.rs:323`）：已经通过 `tokio::spawn` 独立运行，调用 `session_store.update_title_if_current` 使用条件更新。并发改造后这个已存在的后台任务不会引入新的竞态——同一会话的后续消息会排在 worker 队列中，但后台标题任务仍可能与之并发。需确认 `update_title_if_current` 的条件更新能防止覆盖手工标题。

- **session history 读写**：当前 `SessionStore` 的 `Mutex<Connection>` 保证单次 SQL 操作的原子性，但业务层存在「读取旧快照 → 修改 → 写回」模式。同会话内部由于 worker 串行，不会出现快照覆盖问题。后台自动标题与同会话消息的并发已有条件更新保护。

- **SQLite 锁范围**：所有 `Mutex<Connection>` 都是 `std::sync::Mutex`，不能跨 `.await` 持有。当前代码中数据库锁只在同步 SQL 操作期间持有，改造时需保持不变。

### 13. 排查 SQLite 锁范围

当前 SQLite 使用 `std::sync::Mutex<Connection>`（`storage/database.rs:33`），改造不要求变更连接池。

检查要点（`qq-maid-core/src/storage/` 下所有 `connection()` 调用点）：
- session、memory、todo、rss、knowledge 的 `connection()` 返回 `MutexGuard` 后，都在同步代码块内使用
- 不存在跨 `.await` 持有 `MutexGuard` 的情况
- 如果改造后引入新的并发路径导致数据库锁成为瓶颈，应记录而非擅自重构

### 14. 增加可观测性

在 `MessageDispatcher` 中增加结构化日志（使用 `tracing`），至少记录：
- 活跃会话 worker 数量
- 重型响应正在使用的 permit 数
- 单会话队列长度
- 队列满拒绝次数
- worker 创建、退出和空闲回收
- worker panic 或异常退出

日志中不输出完整消息正文或用户 openid（复用 `mask_openid` 脱敏）。

## 配置要求

新增配置项（添加到 `qq-maid-gateway-rs/src/config/mod.rs` 的 `AppConfig`）：

```env
# 重型 LLM 响应全局最大并发数
MAX_CONCURRENT_RESPONSES=4
# 单会话消息队列容量
CONVERSATION_QUEUE_CAPACITY=16
# 会话 worker 空闲回收超时（秒）
CONVERSATION_WORKER_IDLE_TIMEOUT_SECS=300
```

要求：
- 提供安全默认值
- 零值和过大值需校验（0 视为禁用并发限制）
- 更新 `runtime/config/.env.example`
- 配置缺失时使用默认值，不阻止启动

## 必须保持的现有行为

- 同会话消息顺序不变
- 会话历史顺序不变（`SessionStore` 的 `history` 追加写入）
- pending 只能由正确 scope 和正确用户继续（`pending.rs` 的 scope 校验）
- C2C 和群聊身份识别逻辑不退化（`respond.rs` 的 `core_request_from_*`）
- 流式响应继续工作（`CoreResponseStream` / `consume_respond_stream`）
- 自动标题不覆盖手工标题（`update_title_if_current` 条件更新）
- reply cache 不串会话（每个 scope 有独立队列）
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
- 不要进行无关大规模重构
- 不要伪造测试结果

## 关键源码索引

| 组件 | 文件 | 关键函数/结构 |
|------|------|--------------|
| 主入口 | `src/main.rs` | `main`, `gateway_handle` |
| Gateway 启动 | `qq-maid-gateway-rs/src/app/mod.rs` | `run_with_config` |
| 主循环 | `qq-maid-gateway-rs/src/gateway/mod.rs` | `run` (line 94) |
| WS 循环 | `qq-maid-gateway-rs/src/gateway/protocol.rs` | `run_gateway_once` (line 88) |
| 事件分发 | `qq-maid-gateway-rs/src/gateway/protocol.rs` | `handle_envelope` (line 199) |
| 私聊处理 | `qq-maid-gateway-rs/src/gateway/mod.rs` | `handle_c2c_message` (line 333) |
| 群聊处理 | `qq-maid-gateway-rs/src/gateway/mod.rs` | `handle_group_message` (line 163) |
| 流消费 | `qq-maid-gateway-rs/src/gateway/mod.rs` | `consume_respond_stream` (line 301) |
| Gateway→Core | `qq-maid-gateway-rs/src/respond.rs` | `RespondClient`, `core_request_from_*` |
| Core 入口 | `qq-maid-core/src/service.rs` | `CoreHandle::respond` (line 168) |
| 流式起点 | `qq-maid-core/src/service.rs` | `start_core_response_stream` (line 319) |
| scope_key | `qq-maid-core/src/service.rs` | `CoreRequest::scope_key` (line 429) |
| Gateway 配置 | `qq-maid-gateway-rs/src/config/mod.rs` | `AppConfig` |
| Core 配置 | `qq-maid-core/src/config.rs` | `AppConfig` (line 120) |
| 全局状态 | `qq-maid-core/src/http/routes.rs` | `AppState` (line 38) |
| SQLite 封装 | `qq-maid-core/src/storage/database.rs` | `SqliteDatabase`, `Mutex<Connection>` |
| Session 存储 | `qq-maid-core/src/storage/session.rs` | `SessionStore` |
| 自动标题 | `qq-maid-core/src/runtime/respond/chat_flow.rs` | `tokio::spawn` (line 323) |
| 去重 | `qq-maid-gateway-rs/src/gateway/dedupe.rs` | `MessageDedupe` |
| 冷却 | `qq-maid-gateway-rs/src/gateway/group_filter.rs` | `GroupCooldowns` |
| 推送 | `qq-maid-gateway-rs/src/gateway/push.rs` | `GatewayPushSink` |
| 运行时状态 | `qq-maid-gateway-rs/src/gateway/ping/mod.rs` | `GatewayRuntimeStatus` |
| 环境变量模板 | `runtime/config/.env.example` | — |

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
4. reply cache 不返回其他 scope 消息。

### 并发限制

1. 同时运行的重型响应不超过 `MAX_CONCURRENT_RESPONSES`。
2. 等待 permit 不阻塞 WS 继续读取消息。
3. 同一 scope 积压消息不提前占用多个 permit。
4. 轻型本地命令不被 LLM 并发池长期阻塞。

### 背压与资源

1. 单会话队列不超过 `CONVERSATION_QUEUE_CAPACITY`。
2. 队列满时行为明确且可观察。
3. 空闲 worker 能在 `CONVERSATION_WORKER_IDLE_TIMEOUT_SECS` 后回收。
4. 不会因大量消息创建无限数量的长期 task。
5. Gateway 重连不重复创建失控调度器。

### 数据一致性

1. 不出旧 session 快照覆盖新历史。
2. 自动标题不覆盖手工标题。
3. 失败请求不被记为成功。
4. 消息不重复回复。
5. 数据库锁不跨 `.await` 持有。

## 测试要求

**必测场景**（优先使用 mock provider / channel / barrier，不依赖真实 LLM）：

- 同 scope 消息严格串行
- 不同 scope 消息并发
- 群聊长响应不阻塞私聊 `/ping`
- 全局重型响应并发上限生效
- 同 scope 积压不占用多个 permit
- 单会话队列满载策略
- worker 空闲回收
- worker panic 后恢复
- Gateway 关闭时停止接收和清理
- session history 不乱序
- pending 不串线
- 流式响应跨 scope 不混合

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
