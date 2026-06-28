# TASKS：连续消息防抢答 / 短窗口消息聚合

> 来源：GitHub Issue #59「feat：连续消息防抢答：按发送者聚合短时间内的普通聊天消息」
>
> 规划版本：v0.10.0。
>
> 本文只做任务拆解和实施边界定义，不表示能力已经完成。实现时应按小 PR 拆分，避免把消息聚合、Dispatcher 重构和 Agent Harness 能力一次性混在同一个实现 PR 中。

## 背景

用户在即时聊天中经常把一段完整表达拆成多条消息连续发送，例如：

```text
我今天去开会了
然后甲方又改需求
真的烦死了
```

当前机器人收到第一条消息后可能立即开始处理，导致机器人抢答、同一语义被拆成多次 LLM 请求、后续消息等待上一轮回复结束、模型只能看到部分上下文，并增加会话历史噪声。

需要增加一个短暂、可配置的消息聚合窗口：机器人先等待用户停止输入，再把同一发送者连续发出的普通聊天消息作为一个逻辑用户回合处理。该功能不是给所有请求增加固定延迟，而是实现可控的“防抢答”。

---

## 任务目标

完成后应达到：

1. 短时间内连续到达的普通聊天消息可以合并为一个逻辑请求。
2. 默认仅在私聊启用，群聊默认关闭。
3. 每条新消息重置静默等待时间，但不能无限延长。
4. 聚合等待不能占用 Dispatcher Worker、Worker Slot 或 LLM Permit。
5. 命令、Pending 操作和其他即时消息不能被普通聊天聚合延迟。
6. 保持现有同 scope 严格串行语义，不丢消息、不重复处理、不改变消息顺序。
7. 不在 Gateway 中复制一套命令或 Pending 关键词判断规则。
8. 为 v0.10.0 私聊轻量 Agent / Harness 提供更自然的输入体验，但不依赖 Harness 才能生效。

---

## 0. 前置现状确认

实现前先确认当前仓库中的：

* `qq-maid-gateway-rs` 的 QQ 私聊 / 群聊事件标准化、消息去重、回复目标选择和 Dispatcher 入口；
* `qq-maid-core` 的命令解析、pending 查询与确认分发、`CoreService::respond` 普通聊天入口；
* 当前 Dispatcher / scope worker 的排队、串行、Retiring 和 shutdown 语义；
* `runtime/config/.env.example` 中现有会话队列、活跃 worker、LLM 并发和超时配置；
* 现有测试中是否已有可复用的 mock dispatcher、mock core 或 tokio 时间控制方式。

输出一份简短调查结果，至少说明：

* 聚合应放在哪个层级，才能同时访问入站分类能力且不占用 worker；
* 哪些命令 / pending 判断可复用现有实现；
* 哪些消息类型首期不能聚合；
* 现有 reply cache、入站去重和回复目标选择如何保持兼容。

---

## 1. 配置与默认语义

建议新增配置：

```env
MESSAGE_AGGREGATION_PRIVATE_ENABLED=true
MESSAGE_AGGREGATION_GROUP_ENABLED=false
MESSAGE_AGGREGATION_QUIET_MS=1200
MESSAGE_AGGREGATION_MAX_WAIT_MS=3000
MESSAGE_AGGREGATION_MAX_MESSAGES=10
MESSAGE_AGGREGATION_MAX_CHARS=12000
```

默认语义：

| 配置 | 默认值 | 说明 |
| --- | --- | --- |
| 私聊聚合 | `true` | 私聊普通聊天默认开启 |
| 群聊聚合 | `false` | 群聊保持当前立即调度行为 |
| 静默窗口 | `1200ms` | 最后一条消息到达后等待多久 |
| 最大等待 | `3000ms` | 从本批第一条消息开始计算的硬上限 |
| 最大消息数 | `10` | 达到后立即封口并提交 |
| 最大字符数 | `12000` | 达到后立即封口并提交 |

配置约束：

* `quiet_ms` 和 `max_wait_ms` 必须大于 `0`；
* `quiet_ms` 不得大于 `max_wait_ms`；
* 消息数和字符数上限必须大于 `0`；
* 非法配置应在启动阶段明确报错，不应静默修正；
* 未提供配置时使用上述默认值。

---

## 2. 术语与逻辑模型

* 物理消息：平台实际推送的一条用户消息，拥有独立的平台消息 ID、事件 ID、时间和发送者信息。
* 聚合批次：同一发送者在聚合窗口内连续发送的一组物理消息。
* 逻辑请求：聚合批次封口后提交给现有 Dispatcher 和 Core 的一次请求。

多个物理消息只产生一个逻辑请求和一个 LLM 用户回合。对于 LLM 和会话历史，该批次应表现为一个用户回合。

---

## 3. 可聚合与不可聚合消息

一条消息只有同时满足以下条件时，才可以进入聚合批次：

* 来源是用户，而不是机器人自身或系统事件；
* 属于当前允许聚合的聊天类型；
* 是普通聊天输入；
* 不属于命令、管理操作或控制消息；
* 当前会话没有需要立即处理的 `PendingOperation`；
* 没有超过单批消息数或字符数限制；
* 能够安全地转换为普通用户文本内容。

第一阶段建议只聚合普通文本聊天。图片、文件、语音以及无法完整保留结构的复杂消息应作为聚合边界，保持现有处理方式。

以下消息必须立即进入现有处理链路：

* `/todo`、`#todo`、`/memory`、`/ping` 等显式命令；
* `/new`、`/compact` 等会话控制命令；
* 管理员命令；
* 当前存在 `PendingOperation` 时用户发出的后续输入；
* 确认、取消、选择候选等 Pending 交互；
* 图片、文件、语音等第一阶段未支持的消息；
* 系统事件和平台控制事件。

分类要求：

* 不得在聚合模块中重新硬编码“确认”“可以”“好的”“取消”“不要”“算了”等 pending 关键词；
* 不得单独维护另一份命令名称或命令前缀列表；
* 命令判断必须复用现有命令解析能力；
* Pending 判断必须以实际 Pending 状态为准，不能只根据文本关键词猜测；
* 如果 Gateway 当前无法取得这些信息，应增加轻量统一入站分类接口，或把分类放在能够访问现有命令解析和 Pending 状态的位置。

---

## 4. 聚合键与调度键

聚合键和 Dispatcher 的调度 scope 不应被视为完全相同的概念。

私聊聚合键至少包含：

* bot instance；
* platform；
* chat type；
* conversation / session identity；
* sender user identity。

同一个用户在不同机器人实例、不同平台或不同会话中的消息不得合并。

群聊默认关闭，但实现必须避免未来开启后把不同群成员的消息合并。群聊聚合键至少包含：

* existing dispatch scope；
* sender user identity。

聚合完成后，逻辑请求仍使用现有 Dispatcher ScopeKey 进入调度，继续遵守群级或会话级串行规则。

---

## 5. 状态与计时语义

每个活跃聚合批次至少记录等价语义：

```rust
struct PendingAggregation {
    first_received_at: Instant,
    last_received_at: Instant,
    quiet_deadline: Instant,
    hard_deadline: Instant,
    generation: u64,
    messages: Vec<InboundEnvelope>,
    total_chars: usize,
}
```

第一条可聚合消息到达时：

1. 创建新的聚合批次；
2. `first_received_at` 和 `last_received_at` 设置为当前时间；
3. `quiet_deadline = now + quiet_ms`；
4. `hard_deadline = now + max_wait_ms`。

同一聚合键收到后续可聚合消息时：

1. 按到达顺序追加消息；
2. 更新 `last_received_at`；
3. 将静默截止时间更新为 `min(now + quiet_ms, hard_deadline)`；
4. `hard_deadline` 不得重置。

满足任意条件时，当前批次立即封口：

* 到达静默截止时间；
* 到达最大等待时间；
* 达到最大消息数；
* 达到最大字符数；
* 收到不可聚合的边界消息；
* 组件进入正常关闭流程。

封口后的批次不可继续追加消息。之后到达的消息创建新批次。

---

## 6. 消息合并格式

第一阶段使用换行连接文本：

```rust
messages
    .iter()
    .map(|message| message.text.as_str())
    .collect::<Vec<_>>()
    .join("\n")
```

要求：

* 保留原始到达顺序；
* 不对相同文本去重；
* 不自动 trim 用户正文；
* 不插入“消息 1”“消息 2”等人工标签；
* 不自行改写标点；
* 不把命令文本拼进普通聊天正文。

---

## 7. 消息 ID、回复目标与去重

聚合批次必须保留全部来源消息的 ID，不能只留下合并后的字符串。

逻辑请求至少需要保留：

* source message ids；
* source event ids；
* first message timestamp；
* last message timestamp；
* canonical reply target。

聚合回复默认引用批次中的最后一条物理消息。最后一条消息最接近用户完成表达的时刻，也通常是当前平台上最合适的回复目标。

平台重试导致同一个消息 ID 或事件 ID 再次到达时，不应重复追加。内容相同不代表重复事件，去重只能依据稳定的平台消息标识，不能依据正文。

现有 reply cache 和入站去重逻辑不得因聚合被绕过。

---

## 8. 边界消息与顺序

如果聚合期间收到不可聚合消息，例如：

```text
普通聊天 A
普通聊天 B
/todo
```

应执行：

1. 原子封口当前普通聊天批次；
2. 将普通聊天逻辑请求提交到原有 Dispatcher；
3. 再提交 `/todo`；
4. 两者沿用同一调度 scope 的顺序保证。

不可为了“命令立即处理”让后到的命令越过先到的普通消息，否则会破坏用户可见顺序。这里的“立即”是指不再等待剩余聚合时间，而不是允许越序执行。

---

## 9. 与 Dispatcher 的集成要求

聚合必须发生在正式占用 Worker 之前。

推荐链路：

```text
平台事件
→ 事件标准化
→ 入站类型分类
→ Message Aggregator
→ 生成逻辑 InboundEnvelope
→ 现有 Message Dispatcher
→ Scope Worker
→ Core
→ LLM / Tool
```

禁止采用以下实现：

```rust
async fn handle_message(...) {
    tokio::time::sleep(Duration::from_millis(1200)).await;
    // 然后继续处理
}
```

原因是简单地在现有 handler 或 worker 中 sleep 会：

* 占用同 scope Worker；
* 可能占用全局 Worker Slot；
* 阻塞该 scope 后续消息进入聚合；
* 让“等待用户说完”退化为普通延迟；
* 增加 Dispatcher 退出和 Retiring 状态的竞态。

等待中的聚合批次不得：

* 占用 Worker Slot semaphore；
* 占用 LLM concurrency permit；
* 创建已进入 Core 的半完成请求；
* 阻塞其他 scope 正常处理。

---

## 10. 并发、竞态与资源限制

聚合状态建议由单一 actor 所有，或使用具备同等串行语义的结构维护。

必须保证：

* 同一批次最多提交一次；
* 旧定时器不能提交新一代批次；
* 定时器触发和新消息同时到达时，不会重复提交；
* 封口与追加操作是原子的；
* 达到 hard deadline 后不能被新消息重新打开；
* Dispatcher 进入 Active、Retiring 或 successor 切换时，不会丢失聚合结果；
* 一个聚合批次只产生一个逻辑入站请求。

可以使用 generation / token 识别过期计时事件。不建议为每条消息无限创建独立 detached sleep task；若使用独立任务，必须有明确取消和过期机制，且测试不存在任务泄漏。

聚合状态位于内存中，因此至少限制：

* 同时活跃的聚合 scope 数量；
* 单批最大消息数；
* 单批最大字符数；
* 单批最大等待时间。

达到单批上限时应立即封口并提交，而不是丢弃新消息。如果达到全局活跃 scope 上限，建议让新 scope 退化为当前立即调度行为，并输出限频日志；不得静默丢消息。

日志不得输出完整用户正文、平台原始事件或未经脱敏的用户 ID。

---

## 11. 关闭与异常行为

正常关闭时：

* 停止接收新的聚合消息；
* 封口当前已有批次；
* 在现有关闭期限内交给 Dispatcher；
* 不得 panic；
* 不得留下 detached task。

第一阶段不要求持久化尚未封口的内存批次。进程异常退出时，未提交批次不保证恢复；这是本任务明确接受的限制，不应为此引入磁盘队列或数据库迁移。

---

## 12. 可观测性

建议增加结构化日志或指标：

* `aggregation.batch_size`；
* `aggregation.total_chars`；
* `aggregation.wait_ms`；
* `aggregation.flush_reason`；
* `aggregation.active_scopes`。

`flush_reason` 至少区分：

* `quiet_timeout`；
* `max_wait`；
* `max_messages`；
* `max_chars`；
* `barrier`；
* `shutdown`。

日志只记录脱敏后的 scope 标识，不记录完整消息正文。

---

## 13. 测试计划

建议使用可暂停的 Tokio 时间测试，避免依赖真实 sleep。

至少覆盖：

1. 单条私聊消息在 `quiet_ms` 后提交；
2. 多条私聊消息被合并为一个逻辑请求；
3. 后续消息重置 quiet deadline；
4. 后续消息不重置 hard deadline；
5. `max_wait` 强制封口；
6. `max_messages` 强制封口；
7. `max_chars` 强制封口；
8. 两个私聊用户并发输入时分别聚合；
9. 同一用户的不同会话不会合并；
10. 不同机器人实例不会合并；
11. 群聊默认关闭时立即进入现有 Dispatcher；
12. 群聊开启后不同发送者不会被合并；
13. 命令作为边界消息触发已有批次封口；
14. 命令不会被拼入普通聊天正文；
15. Active Pending 状态下消息立即进入现有流程；
16. 重复平台事件不会重复追加；
17. 相同正文、不同消息 ID 会保留两条；
18. quiet timer 与新消息竞争时不会重复提交；
19. hard timer 与新消息竞争时不会重新打开旧批次；
20. Dispatcher Retiring 切换期间聚合结果不会丢失；
21. 等待期间 Worker Slot 和 LLM Permit 均未被占用；
22. 正常关闭时已有批次被封口且任务能够退出。

涉及 Gateway / Dispatcher / Core 调用链时，提交前按影响范围执行：

```bash
cargo fmt --all -- --check
cargo test -p qq-maid-gateway-rs --all-features
cargo test -p qq-maid-core --all-features
cargo test --workspace --all-features
```

如改动影响配置、启动或并发调度，还需要执行对应 clippy、构建和本地启动验证。

---

## 14. 验收标准

* 私聊消息聚合默认开启；
* 群聊消息聚合默认关闭；
* 群聊关闭时行为与当前版本一致；
* 单条私聊普通消息在静默窗口结束后正常提交；
* 同一私聊用户连续发送多条消息时只产生一次逻辑请求；
* 合并后的正文顺序与原始消息到达顺序一致；
* 每条新消息只重置静默时间，不重置最大等待时间；
* 达到最大等待时间后一定提交，不会无限等待；
* 达到消息数或字符数上限后立即提交；
* 不同用户、不同会话和不同机器人实例严格隔离；
* 相同正文但不同消息 ID 的消息不会被错误去重；
* 相同消息 ID 的平台重试不会被重复追加；
* 显式命令不进入普通聊天批次；
* Pending 输入依据真实 Pending 状态绕过聚合；
* 边界消息到达时先封口已有批次，并保持原始顺序；
* 聚合等待不占用 Worker Slot；
* 聚合等待不占用 LLM Permit；
* 每个批次只提交一次；
* 定时器竞态不会导致消息丢失或重复回复；
* 聚合回复使用批次最后一条物理消息作为回复目标；
* 正常关闭时不会 panic 或遗留后台任务；
* 日志不包含完整用户正文或未经脱敏的身份信息。

---

## 15. 首期建议拆分

### Phase 1：调查与配置骨架

* 输出 Gateway / Dispatcher / Core 聚合插入点调查；
* 增加默认配置和启动期校验；
* 不改变用户可见行为。

### Phase 2：入站分类与聚合核心

* 复用现有命令解析和 Pending 状态判断；
* 实现私聊普通文本聚合；
* 完成计时、封口、去重和资源限制测试。

### Phase 3：Dispatcher 集成与回复目标

* 聚合封口后进入现有 Dispatcher；
* 保持同 scope 顺序、Retiring 语义和 reply cache 兼容；
* 聚合回复默认引用最后一条物理消息。

### Phase 4：观测与回归验证

* 增加脱敏日志或指标；
* 补齐关闭流程和竞态测试；
* 本地验证私聊、群聊、命令、Pending 和普通聊天不回归。

---

## 暂不包含

首期不做：

* 使用 LLM 判断用户是否已经说完；
* 根据语义自动决定等待时间；
* 修改或追加已经开始执行的 LLM 请求；
* 自动取消正在生成的上一轮回复；
* 跨进程重启恢复未提交批次；
* 将不同群成员的消息合并成一次请求；
* 长时间“正在输入”状态同步；
* 图片、文件、语音等复杂消息聚合；
* 根据消息内容动态提高或降低模型并发额度。
