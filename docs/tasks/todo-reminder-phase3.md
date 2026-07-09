# 任务：Todo/Reminder 第二阶段语义收尾：引用定位、重复提醒动作、取消删除兼容与默认提醒策略

## 背景

第一阶段已经完成 Todo/Reminder 业务逻辑向 `qq-maid-core/src/runtime/tools/todo/` 的架构收口，respond/storage/runtime/todo 等旧路径已尽量瘦身为通用能力、薄适配或过渡 facade。

本阶段继续推进 #390 的剩余语义收尾，不再做大规模架构搬迁，重点解决用户可见行为：

1. 引用单条 Todo 回执 / 提醒消息时，可以安全定位目标。
2. 重复提醒区分“完成当前周期”“跳过下一次”“暂停”“关闭整个重复提醒”“删除”。
3. 自然语言“取消这个待办/提醒”默认等价删除，同时保留历史 `Cancelled` 兼容。
4. 明确截止型 Todo 的默认 9 点提醒策略，避免重构后语义漂移。
5. 补充必要测试，保证核心行为可验证。

## 目标

完成后应达到：

1. 用户引用 Todo 列表、单条 Todo 回执、提醒消息后，可以按引用上下文定位对应 Todo / Reminder。
2. 如果引用无法唯一定位，不能猜测执行高风险操作，应提示用户选择或补充目标。
3. 重复提醒具备清晰动作语义：
   - “今天已完成 / 我吃过药了”只处理当前周期；
   - “跳过这次 / 今天别提醒了”只跳过下一次或当前周期；
   - “暂停一会 / 暂停到明天”进入暂停语义；
   - “以后别提醒了 / 关掉这个重复提醒”关闭整个重复提醒；
   - “删除这个提醒 / 取消这个提醒”默认删除。
4. 自然语言“取消这个待办”“取消这个提醒”默认进入删除/删除确认语义，不再优先使用 `cancel_todo` 软取消。
5. 历史 `Cancelled` 数据、旧 pending、旧列表入口可以继续安全读取、恢复或清理，不要求本阶段做破坏性 schema migration。
6. 截止型 Todo 的默认提醒策略有明确代码表达和测试覆盖。

## 实现要求

### 1. 先确认第一阶段后的真实结构

开始前请先阅读：

1. 仓库根目录 `AGENTS.md`、`README.md` 和相关开发文档。
2. `qq-maid-core/src/runtime/tools/todo/` 当前结构。
3. 第一阶段迁移后仍保留在以下位置的 Todo 相关逻辑：
   - `qq-maid-core/src/runtime/respond/`
   - `qq-maid-core/src/runtime/todo/`
   - `qq-maid-core/src/storage/todo/`
   - `qq-maid-core/src/runtime/notification.rs`
   - `qq-maid-core/src/storage/notification.rs`

要求：

- 以仓库现状为准，不要假设第一阶段一定按预期完全完成。
- 如果发现第一阶段仍遗留明显业务逻辑在 respond/storage/runtime/todo 中，应优先判断是否影响本阶段任务。
- 不要重新做大搬迁；只处理本阶段需要的最小迁移和适配。

---

## 2. 引用定位完善

### 2.1 支持引用单条 Todo 回执

当前已有列表快照定位能力，但单条新增/编辑/完成/提醒回执不一定绑定单个 Todo 实体。

请实现或完善：

1. Todo 工具域在生成单条 Todo 相关回执时，应能产出可见实体绑定信息。
2. 出站消息应携带足够的 visible entity snapshot / metadata，使用户后续引用该消息时可以恢复目标 Todo。
3. 支持以下用户表达：
   - 引用新增回执后：“删了这个”
   - 引用编辑回执后：“改成明天 8 点”
   - 引用完成回执后：“恢复这个”
   - 引用提醒消息后：“关掉这个提醒”
4. 如果被引用消息只对应一个 Todo，允许直接定位。
5. 如果被引用消息对应多个 Todo，必须要求用户选择编号或更明确目标，不能猜。

### 2.2 支持引用提醒消息定位

提醒 outbox / 推送消息需要能追溯 Todo / Reminder 来源。

请检查：

1. reminder outbox payload 是否包含足够的 source 信息。
2. Gateway / Core 是否能在用户引用提醒消息时恢复 source Todo。
3. 如果现有出站快照机制不能覆盖提醒消息，需补充 Todo 工具域内的 snapshot 生成和解析能力。

要求：

- 引用提醒消息后说“今天已完成”，应处理当前周期。
- 引用提醒消息后说“以后别提醒了”，应关闭整个重复提醒。
- 引用提醒消息后说“删了这个”，应进入删除语义。
- 引用提醒消息后说“跳过这次”，应跳过当前/下一次提醒，不应删除整个 Todo。

### 2.3 安全边界

必须保留：

1. 私聊 / 群聊 / user / scope / owner 隔离。
2. 引用快照 owner、scope、account、platform 不匹配时必须 Blocked。
3. 高风险动作不能 fallback 到当前会话旧列表后误删。
4. 引用列表但未说明编号，且列表有多项时，必须澄清。
5. 引用快照过期时，应提示重新查看或选择，而不是猜测执行。

---

## 3. 重复提醒动作模型

请在 Todo 工具域内建立清晰的动作区分，不要求命名完全一致，但语义必须明确。

### 3.1 完成当前周期

用户表达示例：

- “我今天已经吃过药了”
- “这次完成了”
- “今天这个完成”
- “刚刚那个我做完了”

期望：

1. 如果是一次性 Todo：进入 completed 或删除/完成现有语义，以仓库现状为准。
2. 如果是重复 Todo / 重复提醒：
   - 不关闭整个重复规则；
   - 推进到下一周期；
   - 重新同步下一次 reminder outbox；
   - 回执文案应表达“本次已完成 / 下次将在 xxx”。

### 3.2 跳过下一次 / 跳过当前周期

用户表达示例：

- “跳过这次”
- “今天别提醒了”
- “这次不用了”
- “下一次先别提醒”

期望：

1. 不删除 Todo。
2. 不关闭整个重复规则。
3. 推进或标记跳过当前周期，使下一次提醒按规则继续。
4. 如果现有模型暂时无法表达独立 skip 状态，可以复用“推进下一周期”的实现，但需要在代码注释和完成报告中说明这是阶段性实现。

### 3.3 暂停

用户表达示例：

- “暂停这个提醒”
- “暂停到明天”
- “这几天先别提醒”
- “一小时后再继续提醒”

期望：

1. 如果现有数据模型没有 pause_until，不要硬编码伪状态。
2. 可选择：
   - 增加明确字段；
   - 或暂时澄清/拒绝复杂暂停语义；
   - 或转换为编辑下一次 reminder_at。
3. 不得把“暂停”误处理为删除。
4. 如果本阶段不实现完整暂停，请至少保证不会误删，并在完成报告中说明后续建议。

### 3.4 关闭整个重复提醒

用户表达示例：

- “以后别提醒我吃药了”
- “关掉这个重复提醒”
- “这个提醒不用再重复了”
- “停止每天提醒我”

期望：

1. 关闭 recurrence。
2. 取消未发送 reminder outbox。
3. 不影响其他 Todo。
4. 回执明确表达“已关闭后续重复提醒”。
5. 不应只完成当前周期。

### 3.5 删除

用户表达示例：

- “删除这个提醒”
- “删了这个待办”
- “取消这个提醒”
- “取消这个待办”

期望：

1. 默认走删除/删除确认语义。
2. 删除后取消未发送 reminder outbox。
3. 如果是高风险批量删除，保留确认机制。
4. 不再优先进入 `Cancelled` 软取消状态。

---

## 4. 取消 / Cancelled 兼容清理

本阶段目标是行为收敛，不要求立即删除数据库状态。

请实现：

1. 自然语言“取消这个待办 / 取消这个提醒”默认进入 delete/remove 语义。
2. 如果仍存在 `cancel_todo` 工具：
   - 评估是否下线、隐藏、改为兼容入口，或内部转发到 delete。
   - 不要让模型优先调用它处理普通“取消”。
3. 保留历史 `Cancelled` 数据读取能力：
   - 旧 cancelled 列表可以查看或清理；
   - 旧 cancelled 可以恢复，除非项目决定废弃；
   - 旧 pending 不应卡死会话。
4. 如果发现测试仍大量依赖 cancelled 语义，请不要为了通过测试强行保留旧行为；应更新测试表达新的产品语义，同时保留历史兼容测试。

禁止：

- 不要直接破坏性删除 `Cancelled` 字段和 migration，除非同时提供完整兼容方案。
- 不要把“取消”同时映射成软取消和删除，造成用户不可预期。
- 不要吞掉删除失败或伪造成功。

---

## 5. 默认 9 点提醒策略

需要明确截止型 Todo 的默认提醒策略。

请先排查当前代码实际行为：

1. `TODO_DAILY_REMINDER_ENABLED`
2. `TODO_DAILY_REMINDER_TIME`
3. 每日摘要提醒逻辑
4. 创建期是否派生 `reminder_at`
5. 截止型 Todo 与纯提醒 Todo 的区别

然后按仓库现状和 #390 目标做最小实现。

目标语义：

1. “周五要写周报”
   - 应保存截止日期。
   - 是否生成 `reminder_at` 取决于项目最终采用的默认策略。
   - 代码和测试必须明确，不允许模糊。
2. “周五前写周报，不提醒我”
   - 保存截止日期。
   - 不生成提醒。
3. “周五写周报，早上提醒我”
   - 保存截止日期。
   - reminder_at 应落到截止日早上默认提醒点，当前默认 9 点。
4. “明天 9 点开会，8:30 提醒我”
   - due_at = 9:00。
   - reminder_at = 8:30。
5. “20 分钟后提醒我看锅”
   - 只有 reminder_at。
   - 不强行生成 due_at。

如果发现当前代码只有“每日摘要调度”，没有“创建期默认派生 reminder_at”，请不要擅自混合两套语义。需要：

- 在代码注释中明确当前采用哪一种；
- 在测试中固定当前行为；
- 在完成报告中说明与 #390 描述是否存在差异。

---

## 6. 测试要求

请优先补充或更新以下测试。

### 引用定位

1. 引用单条新增 Todo 回执后删除该 Todo。
2. 引用单条提醒消息后完成当前周期。
3. 引用重复提醒消息后关闭整个重复提醒。
4. 引用列表消息 + 编号时，按被引用列表快照定位。
5. 引用列表消息但未说明编号且列表多项时，要求澄清。
6. 引用快照 owner/scope/account/platform 不匹配时 blocked，不 fallback。

### 重复提醒语义

1. “今天已完成”推进当前周期，不关闭 recurrence。
2. “跳过这次”不删除、不关闭 recurrence。
3. “以后别提醒了”关闭 recurrence，并取消未发送 outbox。
4. 删除重复提醒时取消未发送 outbox。
5. 已发送旧 outbox 不应导致重复推进多轮。

### 取消 / 删除

1. “取消这个待办”默认删除。
2. “取消这个提醒”默认删除。
3. 历史 Cancelled 数据仍可安全读取或清理。
4. 旧 pending cancel/delete 兼容不应卡死。

### 默认提醒策略

1. 纯记录 Todo 不生成 reminder outbox。
2. 一次性纯提醒只有 reminder_at。
3. 截止型 Todo 的默认提醒行为有明确测试。
4. 用户明确“不提醒”时不生成提醒。
5. “截止日早上提醒”落到默认早上提醒点。

---

## 7. 建议排查范围

请根据仓库现状搜索，不要仅限于以下路径：

- `qq-maid-core/src/runtime/tools/todo/`
- `qq-maid-core/src/runtime/respond/todo_flow/`
- `qq-maid-core/src/runtime/respond/chat_flow/`
- `qq-maid-core/src/runtime/respond/agent_outcome.rs`
- `qq-maid-core/src/runtime/respond/tool_runtime.rs`
- `qq-maid-core/src/runtime/todo/`
- `qq-maid-core/src/storage/todo/`
- `qq-maid-core/src/runtime/notification.rs`
- `qq-maid-core/src/storage/notification.rs`
- `qq-maid-core/src/runtime/visible_entity.rs`
- `qq-maid-common/src/time_context/`

关键词：

- `cancel`
- `cancelled`
- `restore`
- `delete`
- `recurrence`
- `reminder_at`
- `due_at`
- `due_date`
- `visible_entity`
- `last_todo_query`
- `quoted`
- `snapshot`
- `NotificationOutbox`
- `source_type`
- `todo_reminder`
- `advance_recurring`

---

## 8. 禁止事项

- 不要重新做一轮无边界大重构。
- 不要把 Todo 业务逻辑重新塞回 respond/storage/runtime 通用层。
- 不要通过硬编码关键词简单绕过工具域模型。
- 不要为了通过测试保留错误产品语义。
- 不要让引用定位在不确定时猜测执行删除、关闭重复提醒等高风险动作。
- 不要破坏 NotificationWorker 的通用性。
- 不要伪造测试结果、构建结果或执行结果。

---

## 9. 验收标准

完成后应满足：

1. Todo/Reminder 第二阶段语义有清晰代码入口，主要落在 `runtime/tools/todo`。
2. 引用单条 Todo 回执、Todo 列表和提醒消息时，能安全恢复或拒绝定位。
3. 重复提醒可以区分当前周期完成、跳过、关闭整个重复提醒和删除。
4. “取消这个待办/提醒”默认按删除处理。
5. 历史 `Cancelled` 兼容路径不阻塞、不误导新语义。
6. 截止型 Todo 默认提醒策略有明确代码注释和测试。
7. 核心测试、格式化和静态检查通过；如有失败，必须说明是否与本次修改相关。

---

## 10. 测试与检查命令

至少执行：

1. `cargo fmt --all -- --check`
2. `cargo check -p qq-maid-core --all-features`
3. Todo 相关测试，例如：
   - `cargo test -p qq-maid-core runtime::tools::todo --all-features`
   - `cargo test -p qq-maid-core runtime::respond::tests::todo --all-features`
   - `cargo test -p qq-maid-core runtime::todo --all-features`
   - `cargo test -p qq-maid-core storage::todo --all-features`
4. 如改动 notification/outbox：
   - `cargo test -p qq-maid-core runtime::notification --all-features`
   - `cargo test -p qq-maid-core storage::notification --all-features`
5. `cargo clippy -p qq-maid-core --all-targets --all-features -- -D warnings`
6. `git diff --check`

如果测试名称与仓库实际不一致，请使用搜索找到对应测试模块后运行等价范围。

---

## 11. 完成后输出

完成后请汇报：

1. 第二阶段解决了哪些 #390 剩余问题。
2. 引用定位现在支持哪些消息类型。
3. 重复提醒动作模型如何区分：
   - 完成当前周期；
   - 跳过；
   - 暂停；
   - 关闭整个重复提醒；
   - 删除。
4. “取消=删除”做到了哪一层，是否仍保留 `Cancelled` 兼容。
5. 默认 9 点提醒策略最终采用了哪种实现：
   - 每日摘要；
   - 创建期派生 reminder_at；
   - 或两者并存但边界明确。
6. 修改了哪些文件。
7. 执行了哪些测试和检查。
8. 测试结果。
9. 尚未解决的问题、风险和建议拆出的后续 PR。