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

需要增加一个短暂、可配置的消息聚合窗口：机器人先等待用户停止输入，再把同一发送者连续发出的普通聊天消息作为一个逻辑用户回合处理。该功能不是给所有请求增加固定延迟；只有符合聚合条件的普通私聊才会增加最多一个静默窗口的等待，用于实现可控的“防抢答”。

---

## 任务目标

完成后应达到：

1. 短时间内连续到达的普通聊天消息可以合并为一个逻辑用户回合。
2. 首期仅在私聊启用，群聊聚合暂不支持。
3. 每条新消息重置静默等待时间，但不能无限延长。
4. 聚合等待不能占用 Dispatcher Worker、Worker Slot 或 LLM Permit。
5. 命令、Pending 操作和其他即时消息不能被普通聊天聚合延迟。
6. 保持现有同 scope 严格串行语义；正常投递路径不丢消息、不重复处理、不改变消息顺序。若 Dispatcher 交付失败，Aggregator 回滚 reservation 并给出一次明确提示，由用户稍后重发，不在内存中自动恢复。
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
MESSAGE_AGGREGATION_GROUP_ENABLED=false # 未来保留；v0.10.0 首期不得设为 true
MESSAGE_AGGREGATION_QUIET_MS=1200
MESSAGE_AGGREGATION_MAX_WAIT_MS=3000
MESSAGE_AGGREGATION_MAX_MESSAGES=10
MESSAGE_AGGREGATION_MAX_CHARS=12000
MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS=1024
```

默认语义：

| 配置 | 默认值 | 说明 |
| --- | --- | --- |
| 私聊聚合 | `true` | 私聊普通聊天默认开启 |
| 群聊聚合 | `false` | 未来保留配置；v0.10.0 首期暂不支持开启，群聊消息始终立即进入 Dispatcher |
| 静默窗口 | `1200ms` | 最后一条消息到达后等待多久 |
| 最大等待 | `3000ms` | 从本批第一条消息开始计算的硬上限 |
| 最大消息数 | `10` | 达到后立即封口并提交 |
| 最大字符数 | `12000` | 达到后立即封口并提交 |
| 最大活跃键数 | `1024` | 同时存在的聚合键数量上限，达到后新键退化为立即调度 |

配置约束：

* `quiet_ms` 和 `max_wait_ms` 必须大于 `0`；
* `quiet_ms` 不得大于 `max_wait_ms`；
* 消息数、字符数和活跃键数上限必须大于 `0`；
* `MESSAGE_AGGREGATION_GROUP_ENABLED` 在首期只能为 `false` 或缺省；如配置为 `true`，必须启动失败并提示群聊聚合尚未支持；
* 非法配置应在启动阶段明确报错，不应静默修正；
* 未提供配置时使用上述默认值。

---

## 2. 术语与逻辑模型

* 物理消息：平台实际推送的一条用户消息，拥有独立的平台消息 ID、事件 ID、时间和发送者信息。
* 聚合批次：同一发送者在聚合窗口内连续发送的一组物理消息。
* 逻辑用户回合：聚合批次封口后提交给现有 Dispatcher、Core 和可选 Harness 的一次用户输入。

多个物理消息只产生一个逻辑用户回合和一个 LLM 用户回合。对于 LLM 和会话历史，该批次应表现为一个用户回合。

统一入站链路应保持为：

```text
物理入站消息
→ 命令 / Pending / 控制消息分类
→ 普通文本聚合
→ 逻辑用户回合
→ Dispatcher
→ Core 普通聊天 / Harness
```

Aggregator 只处理物理消息，并在封口后生成逻辑用户回合；Harness 只消费聚合后的逻辑用户回合。命令、Pending 和控制消息不进入通用 Harness。聚合关闭时，单条普通消息直接形成逻辑用户回合。建议先完成共同入站接口或消息聚合，再接入 Harness 私聊入口，避免 Harness 重复实现物理消息分类。

---

## 3. 可聚合与不可聚合消息

一条消息只有同时满足以下条件时，才可以进入聚合批次：

* 来源是用户，而不是机器人自身或系统事件；
* 属于当前允许聚合的聊天类型，首期仅限普通私聊；
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
* 用户发送“取消”时，只有命中现有 Pending 取消、显式命令或已定义控制消息状态，才作为边界消息立即进入现有处理链路；
* 没有 Pending 或控制状态时，普通文本“取消”不得被聚合模块硬编码为取消聚合，应按普通聊天文本处理，避免误吞用户表达；
* 如果未来要支持“取消当前未封口聚合批次”，必须先定义显式命令或统一控制消息类型，并明确回复、去重和审计语义；
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

v0.10.0 首期群聊聚合不是“默认关闭但可以开启”，而是暂不支持。群聊消息始终按现有逻辑立即进入 Dispatcher，不允许通过配置开启聚合。

原因是群聊若按不同发送者维护独立聚合批次，不同批次的封口顺序可能与群级 dispatch scope 中物理消息的原始到达顺序不一致，从而破坏现有 Dispatcher 的严格串行目标。

后续若开放群聊聚合，必须先设计 dispatch scope 内的全局序号或顺序屏障，再定义群聊聚合键和提交顺序；在该设计完成前，不得实现“不同发送者分别聚合后直接提交”的群聊方案。

聚合完成后，逻辑用户回合仍使用现有 Dispatcher ScopeKey 进入调度，继续遵守会话级串行规则。

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

1. 每次追加前计算 `projected_message_count = current_message_count + 1`；
2. 每次追加前计算 `projected_chars = current_total_chars + new_message_chars`；
3. 若 projected 值超过上限，先原子封口当前非空批次，再将新消息作为下一批首条重新处理；
4. 若 projected 值等于上限，按到达顺序追加消息，随后立即封口；
5. 若 projected 值低于上限，按到达顺序追加消息并继续等待；
6. 更新 `last_received_at`；
7. 将静默截止时间更新为 `min(now + quiet_ms, hard_deadline)`；
8. `hard_deadline` 不得重置。

批次上限越界语义：

* projected message count 或 projected chars 等于上限时，本条消息必须进入当前批次，追加后立即封口；
* projected message count 或 projected chars 超过上限时，当前非空批次先封口提交，新消息不得丢弃，也不得强行塞入已满批次；
* 作为下一批首条重新处理的新消息，需要重新经过单条超大、可聚合性和活跃键上限检查；
* 单条消息自身超过字符上限时不得截断或丢弃，应绕过聚合并进入现有立即处理链路，或使用项目已有的大消息错误处理；
* 上限处理必须保证不丢失、不重复，且不改变同一 scope 内物理消息的可见顺序。

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

逻辑用户回合至少需要保留：

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
2. 将普通聊天逻辑用户回合提交到原有 Dispatcher；
3. 再提交 `/todo`；
4. 两者沿用同一调度 scope 的顺序保证。

不可为了“命令立即处理”让后到的命令越过先到的普通消息，否则会破坏用户可见顺序。这里的“立即”是指不再等待剩余聚合时间，而不是允许越序执行。

---

## 9. 与 Dispatcher 的集成要求

聚合必须发生在正式占用 Worker 之前。

推荐链路应与 Harness 入口共享同一抽象：

```text
物理入站消息
→ 命令 / Pending / 控制消息分类
→ 普通文本聚合
→ 逻辑用户回合
→ Dispatcher
→ Core 普通聊天 / Harness
```

实现上可映射为：平台事件标准化后先进入统一入站分类，再由 Message Aggregator 生成逻辑 InboundEnvelope，最后进入现有 Message Dispatcher、Scope Worker、Core 和 LLM / Tool。

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
* 一个聚合批次只产生一个逻辑用户回合。

可以使用 generation / token 识别过期计时事件。不建议为每条消息无限创建独立 detached sleep task；若使用独立任务，必须有明确取消和过期机制，且测试不存在任务泄漏。

聚合状态位于内存中，因此至少限制：

* 同时活跃的聚合键数量，可通过 `MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS` 或等价配置限制；
* 单批最大消息数；
* 单批最大字符数；
* 单批最大等待时间。

达到单批上限时应按 projected overflow 语义封口并重新处理新消息，而不是静默丢弃新消息。如果达到全局活跃键上限，建议让新聚合键退化为当前立即调度行为，并输出限频日志；不得静默丢消息。若正常运行期封口结果交付 Dispatcher 失败，应回滚相关 reservation 并向用户提示一次，允许用户稍后重发；不得为此恢复 deferred 队列、retry timer 或其他后台自动恢复状态机。已有批次封口、超时或 shutdown flush 后必须释放活跃键配额，避免长期退化。

日志不得输出完整用户正文、平台原始事件或未经脱敏的用户 ID。

---

## 11. 关闭与异常行为

正常关闭顺序必须明确：

1. Gateway 停止接收新入站消息；
2. Aggregator 停止创建新批次；
3. Aggregator 原子封口全部已有批次；
4. 等待 Dispatcher 确认接收所有封口结果；
5. 关闭 Aggregator；
6. 再关闭 Dispatcher intake，并等待 worker drain；
7. 最后关闭 Core / LLM 相关资源。

约束：

* Dispatcher 不得先于 Aggregator flush 关闭入口，否则封口结果可能无处投递；
* 正常运行期 flush 失败不得静默丢消息，必须返回错误、记录原因、回滚 reservation，并确保用户最多收到一条明确失败提示；shutdown flush 失败只回滚和记录，不再发送用户提示；
* shutdown deadline 到期时，需要记录剩余批次数量、脱敏后的 scope 标识和失败原因；
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
* `aggregation.active_keys`。

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
2. 多条私聊消息被合并为一个逻辑用户回合；
3. 后续消息重置 quiet deadline；
4. 后续消息不重置 hard deadline；
5. `max_wait` 强制封口；
6. `max_messages` 等于上限时追加后立即封口；
7. `max_chars` 等于上限时追加后立即封口；
8. projected message count 超过上限时先封口当前批次，再把新消息作为下一批首条处理；
9. projected chars 超过上限时先封口当前批次，再把新消息作为下一批首条处理；
10. 单条超大消息不会被截断、丢弃或重复处理；
11. projected overflow 正常投递路径不丢消息、不重复消息，交付失败时回滚并提示用户重发；
12. 两个私聊用户并发输入时分别聚合；
13. 同一用户的不同会话不会合并；
14. 不同机器人实例不会合并；
15. 群聊消息首期始终立即进入现有 Dispatcher，配置为开启时启动失败；
16. 命令作为边界消息触发已有批次封口；
17. 命令不会被拼入普通聊天正文；
18. Active Pending 状态下“取消”立即进入现有 Pending 处理流程；
19. 无 Pending 或控制状态时，普通文本“取消”不会被聚合模块误当成取消指令；
20. 重复平台事件不会重复追加；
21. 相同正文、不同消息 ID 会保留两条；
22. quiet timer 与新消息竞争时不会重复提交；
23. hard timer 与新消息竞争时不会重新打开旧批次；
24. timer、flush 与 Dispatcher shutdown 并发竞态不会丢失或重复提交；
25. Dispatcher Retiring 切换期间聚合结果不会丢失；
26. 等待期间 Worker Slot 和 LLM Permit 均未被占用；
27. 活跃键达到上限时新键退化为立即调度；正常投递成功时不丢消息，交付失败时回滚并提示用户重发；
28. 批次封口、超时和 shutdown 后释放活跃键配额；
29. 正常关闭时已有批次被封口、Dispatcher 已确认接收且任务能够退出。

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
* 群聊消息聚合首期暂不支持开启；
* 群聊消息始终按当前版本逻辑立即进入 Dispatcher；
* 单条私聊普通消息在静默窗口结束后正常提交；
* 同一私聊用户连续发送多条消息时只产生一次逻辑用户回合；
* 合并后的正文顺序与原始消息到达顺序一致；
* 每条新消息只重置静默时间，不重置最大等待时间；
* 达到最大等待时间后一定提交，不会无限等待；
* 达到消息数或字符数上限后立即提交；
* projected overflow 和单条超大消息行为确定，不截断、不丢失、不重复；
* 不同用户、不同会话和不同机器人实例严格隔离；
* 相同正文但不同消息 ID 的消息不会被错误去重；
* 相同消息 ID 的平台重试不会被重复追加；
* 显式命令不进入普通聊天批次；
* Pending 输入依据真实 Pending 状态绕过聚合；
* “取消”只在 Pending、显式命令或已定义控制状态下作为边界消息；
* 无 Pending 或控制状态时，普通文本“取消”不会被误判为取消聚合；
* 边界消息到达时先封口已有批次，并保持原始顺序；
* 聚合等待不占用 Worker Slot；
* 聚合等待不占用 LLM Permit；
* 每个批次只提交一次；
* 定时器竞态不会导致消息丢失或重复回复；
* 聚合回复使用批次最后一条物理消息作为回复目标；
* 正常关闭时 Aggregator 先 flush 且 Dispatcher 确认接收后，才关闭 Dispatcher intake；
* shutdown deadline 到期时记录剩余批次和失败原因；
* 正常关闭时不会 panic 或遗留后台任务；
* 日志不包含完整用户正文或未经脱敏的身份信息。

---

## 15. 首期建议拆分

### Phase 1：调查与配置骨架

* 输出 Gateway / Dispatcher / Core 聚合插入点调查；
* 增加默认配置和启动期校验；
* 明确群聊聚合配置首期不可开启；
* 不改变用户可见行为。

### Phase 2：入站分类与聚合核心

* 复用现有命令解析和 Pending 状态判断；
* 实现私聊普通文本聚合；
* 完成计时、projected overflow、封口、去重和资源限制测试。

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
