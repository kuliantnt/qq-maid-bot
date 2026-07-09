# Todo/Reminder 第一阶段审计报告

关联 issue：[#390](https://github.com/kuliantnt/qq-maid-bot/issues/390)

审计日期：2026-07-09

## 结论摘要

当前 Todo/Reminder 已经具备待办、单次提醒、重复提醒、最近列表快照、引用列表快照、澄清恢复和 Notification Outbox 投递能力，但业务语义仍分散在多个层级：

- 数据模型和大量状态变更在 `qq-maid-core/src/storage/todo/` 与 `qq-maid-core/src/runtime/todo/`。
- 工具入参、目标解析、创建、编辑、完成、取消、恢复、删除、合并在 `qq-maid-core/src/runtime/tools/todo/`。
- 用户可见列表、回执、相关列表快照、pending 确认、旧兼容状态机在 `qq-maid-core/src/runtime/respond/todo_flow/`。
- 普通聊天 Tool Loop 的 Todo 成功验真、引用可见实体快照恢复、确定性结果聚合仍在 `runtime/respond/chat_flow`、`runtime/respond/agent_outcome.rs` 与 `runtime/respond/tool_runtime.rs`。
- 单次提醒与重复提醒推进跨越 `runtime/todo/reminder_task.rs`、`runtime/notification.rs`、`storage/notification.rs`、`runtime/todo_reminder.rs` 和 `storage/todo/recurrence.rs`。

第一阶段不建议直接大迁移。更稳妥的顺序是先补冻结测试，再把“Todo 业务用例”和“respond 展示/快照适配”逐步拆开。

## 架构边界约束

本次 Todo/Reminder 重构的长期目标是收口业务边界：Todo 相关业务实现应集中在 `qq-maid-core/src/runtime/tools/todo/` 及其子模块中。

- `runtime/tools/todo` 是 Todo 业务域边界，应承载自然语言意图判断、创建、编辑、删除、完成、取消、提醒、重复提醒、引用定位、列表快照、结果建模和回执生成等业务语义。
- `storage/todo` 只保留 schema、CRUD、查询、低层字段归一和无业务意图的时间推进 helper。它可以提供纯字段校验和时间推进能力，但不应读取 `raw_text/title/detail` 判断用户意图。
- `runtime/respond/*` 只保留通用编排、入口适配、事件流/展示适配和安全校验。Todo 专用状态机、业务 fallback、目标解析、回执文案和快照修补不应长期留在 respond。
- `runtime/todo` 只能作为过渡 facade 或通用复用薄层，不能继续新增 Todo 业务规则；已有业务规则应逐步迁入 `runtime/tools/todo`。

迁移策略必须小步进行：发现 storage、respond、chat_flow 或 `runtime/todo` 中散落的 Todo 业务语义时，先补冻结测试，再迁入 `runtime/tools/todo` 对应子模块，不做一次性大搬迁。

## 现有数据模型

`todos` 表由 `qq-maid-core/src/storage/todo/mod.rs` 中的 migration 管理：

- V1：`id`、`owner_key`、`user_id`、`scope_key`、`title`、`detail`、`raw_text`、`due_date`、`due_at`、`time_precision`、`status`、`completed`、`created_at`、`updated_at`、`completed_at`、`cancelled_at`。
- V2：新增 `reminder_at`。
- V3：新增 `recurrence_kind`、`recurrence_interval_days`。
- V4：新增 `recurrence_interval`、`recurrence_unit`。

运行时结构：

- `TodoStatus`：`Pending`、`Completed`、`Cancelled`。注意 `Cancelled` 仍是正式持久化状态，和 #390 目标中的“取消默认等价删除”不一致。
- `TodoTimePrecision`：`None`、`Date`、`DateTime`、`Inferred`。
- `TodoItem` / `TodoItemDraft`：同时承载 `due_date`、`due_at`、`reminder_at`、`recurrence_*`。
- `TodoOwner`：由 `TodoStore::owner(user_id, scope_key)` 生成，当前仍要求 `owner_key + scope_key` 匹配，群聊中 Todo owner 是发言人的个人 Todo，不是群共享 Todo。

语义现状：

- `due_at` / `due_date` 表示截止或事件时间。
- `reminder_at` 表示下一次单次提醒触发时间。纯提醒允许只有 `reminder_at`，不再回填 `due_at`。
- `recurrence_*` 表示重复规则。重复任务完成或提醒发送成功后，通过推进原记录的时间字段表达下一周期，不预生成未来实例。
- `Cancelled` 表示软取消，可恢复，可被列表展示和物理删除。

## 创建调用链

Tool Loop 创建链路：

1. `runtime/tools/todo/create.rs::CreateTodoTool::execute`
2. `create_draft_from_value`
3. `runtime/todo::enrich_draft_time_from_text`
4. `storage/todo/normalize.rs::normalize_draft`
5. `storage/todo/recurrence.rs::normalize_todo_recurrence_input`
6. `runtime/todo/ops.rs::create_many`
7. `TodoStore::create_many`
8. `runtime/todo/reminder_task.rs::sync_reminder_task`
9. `NotificationOutboxStore::upsert`

旧 pending 创建链路仍兼容：

- `PendingOperation::TodoAdd` 在 `runtime/respond/todo_flow/pending.rs` 中确认后调用 `runtime/todo/ops.rs::create_one`。
- 注释明确新版本 `create_todo` 已直接写库，`TodoAdd` pending 只为旧 session 兼容。

当前创建输入已经在工具 schema 中区分 `due_date`、`due_at`、`reminder_at`、`recurrence_kind`、`recurrence_interval` 和 `recurrence_unit`。但“创建意图分类”仍主要靠模型 schema、common 时间 helper 和 storage recurrence 兜底，并未形成 Todo 工具域内部的独立 intent/use case。

## 默认 9 点提醒现状

配置层默认值：

- `qq-maid-core/src/config/mod.rs`：`DEFAULT_TODO_DAILY_REMINDER_TIME = "09:00"`。
- `TODO_DAILY_REMINDER_ENABLED` 默认 `false`。
- `TODO_DAILY_REMINDER_TIME` 按 `HH:MM` 严格解析，固定 Asia/Shanghai。

实际实现：

- `runtime/todo_reminder.rs::TodoReminderScheduler` 在每天配置时间运行。
- 它扫描可验证私聊 target 的 pending Todo owner。
- `format_reminder_message` 按今天、逾期、无日期分组生成每日待办提醒 outbox。

重要差异：

- 当前“默认 9 点”更像“每日待办摘要调度时间”，不是在创建截止型 Todo 时自动生成一条 `reminder_at = 截止日 09:00` 的单次提醒。
- #390 验收描述要求“有截止时间但无显式提醒时间时，按现有默认策略在截止日早上 9 点提醒，除非用户明确不提醒”。后续重构前需要先确认产品语义：是保留每日摘要模型，还是改成创建期派生单次 `reminder_at`。这两者会影响 outbox、重复提醒和用户“不提醒”的字段表达。

## 一次性纯提醒和周期性纯提醒

一次性纯提醒：

- `qq-maid-common/src/time_context/todo_time.rs::enrich_todo_time_fields_from_text` 对“N 分钟/小时后提醒”写入 `reminder_at`，不回填 `due_at`。
- `storage/todo/normalize.rs::normalize_draft` 明确注释“到期时间与提醒时间解耦”，纯提醒保持 `due_at` / `due_date` 为空。
- `runtime/todo/reminder_task.rs::sync_reminder_task` 只看 `reminder_at` 是否存在和是否晚于当前时间，存在则入 Notification Outbox。

周期性纯提醒：

- `storage/todo/recurrence.rs::parse_recurrence_from_text` 支持每分钟、每小时、每天、每周、每月、每年，以及“每隔 N 分钟/小时/天/周/月/年”。
- #391 临时逻辑位于 `storage/todo/recurrence.rs::normalize_todo_recurrence_input`：
  - 如果有 recurrence rule；
  - 且 `due_date`、`due_at`、`reminder_at` 都为空；
  - 且 `is_recurring_reminder_intent` 命中“提醒/通知我/提示我/叫我/喊我/闹钟”；
  - 则 `next_reminder_from_now(rule)` 生成首次 `reminder_at`。
- 该逻辑使用 `Utc::now().with_timezone(shanghai_offset())`，从创建时刻推进一个周期。

收敛建议：

- `is_recurring_reminder_intent` 和首次 `reminder_at` 推导属于业务意图判断，应迁到 `runtime/tools/todo` 下的 create use case 或 reminder/recurrence service。
- `storage/todo/recurrence.rs` 后续只保留重复规则字段归一、合法性校验和时间推进 helper，不再读取 `raw_text/title/detail` 判断用户意图。

## Outbox 与重复提醒推进

单次提醒入队：

- `runtime/todo/reminder_task.rs::sync_reminder_task`：
  - 无 `reminder_at`：取消该 Todo 未发送提醒任务。
  - `reminder_at <= now`：取消未发送提醒任务。
  - 有未来 `reminder_at`：根据 `owner.scope_key` 解析 `PushTarget`，取消旧未发送任务，再 upsert 新 outbox。
  - `dedupe_key = todo:{id}:reminder:{scheduled_at}`，同一 Todo 改到新时间会生成新事件。

通知投递：

- `runtime/notification.rs::NotificationWorker` 只负责 claim due、投递、mark sent/failed 和 retry。
- Worker 不反查 Todo，不解释业务语义。

发送成功后的重复推进：

- `runtime/todo/reminder_task.rs::TodoReminderSentHook::after_sent` 只处理 `source_type = todo` 且 `kind = todo_reminder`。
- 调用 `TodoStore::advance_recurring_reminder_by_id(source_id, scheduled_at)`。
- `TodoStore::advance_recurring_reminder_by_id` 只有在 Todo 仍是 pending、仍是 recurring、当前 `reminder_at` 与刚发送 outbox 的 `scheduled_at` 相同才推进，避免旧 outbox 或重复 hook 导致连跳。
- 推进后重新 `sync_reminder_task`，生成下一次 outbox。

错过触发策略：

- `storage/todo/recurrence.rs::recurrence_advance_cycles` 会根据当前时间计算需要推进的周期数，目标是推进到下一次未来时间，不补发离线期间每个历史周期。

## 完成、删除、取消、关闭现状

完成：

- `complete_todos` 调用 `runtime/todo/ops.rs::complete_many_with_recurrence`。
- 一次性 Todo：`status -> Completed`，`completed_at` 写入。
- 重复 Todo：保持 `Pending`，推进 `due_date/due_at/reminder_at` 到下一周期，代表“完成本次”。
- Tool 输出同时区分 `completed` 和 `advanced`。

取消：

- `cancel_todo` 当前是软取消：pending -> `Cancelled`，写 `cancelled_at`，取消未发送提醒任务。
- `TodoStore::cancel_by_ids` 与 `TodoStore::cancel` 都执行 UPDATE，而不是 DELETE。
- `restore_todos` 可以恢复 completed 和 cancelled。
- `/todo cancelled` 和 `list_todos(status=cancelled)` 会展示已取消列表。

永久删除：

- `delete_todos` 只发起 pending confirmation，确认后物理删除。
- `TodoBulkDelete` 带 `status`，确认时使用 `delete_pending_by_ids` / `delete_completed_by_ids` / `delete_cancelled_by_ids`，避免确认期间状态变化越权。
- 旧 `TodoDelete + Pending` 曾表示确认后软取消；当前 pending 处理遇到这种旧状态会提示“旧版待确认操作已失效”，要求重新发起。

关闭整个重复提醒：

- 当前没有独立的 `disable recurring reminder` / `skip next reminder` / `pause` 工具语义。
- 用户若说“以后别提醒了”，目前大概率会走 `cancel_todo` 或 `edit_todo(recurrence_kind=none/reminder_at="")`，取决于模型工具调用；代码层没有明确 use case。

## cancel / cancelled / 取消清单

仍存在的取消语义：

- 数据模型：`TodoStatus::Cancelled`、`cancelled_at`。
- 存储接口：`list_cancelled`、`restore_cancelled_by_ids`、`cancel_by_ids`、`cancel_completed_by_ids`、`delete_cancelled_by_ids`。
- Tool：`cancel_todo`、`restore_todos` 同时恢复 cancelled。
- Respond：`format_todo_cancelled_list_reply`、`receipt_after_deleted` 按 cancelled 生成相关列表、pending 中“回复取消”表示放弃待确认操作。
- 测试：大量断言已取消列表、取消恢复、取消后删除、引用快照取消。

建议：

- 不应在第一轮直接删除 `Cancelled`，因为它涉及 schema、历史数据、列表展示、恢复、pending 兼容和测试。
- 若产品目标确定为“用户说取消等价删除”，建议先做行为映射：自然语言“取消这个待办/提醒”路由到 `delete_todos` 或新的 delete use case，但保留历史 `Cancelled` 读取和清理能力。
- 第二步再决定是否长期废弃 `Cancelled` 状态；如果废弃，需要 migration / 兼容策略和 UI 文案更新。

## 引用定位、回复引用和列表快照

三类语义现状：

1. 用户引用消息定位 Todo：
   - 入站引用上下文是 `QuotedMessageContext`，包含 `reference_id/ref_msg_idx`、`lookup_found`、`from_bot`、`text_summary`、`input_parts`、`sender`。
   - Core 不直接从引用文本解析 Todo ID。
   - 对机器人列表消息的引用，依赖 Gateway 回填 `visible_entity_snapshot`。

2. 机器人回复是否引用原消息：
   - 属于 Gateway 出站能力，例如 `ReplyTarget` 和 `supports_quote_original`。
   - 这与 Todo 目标定位是两条链路，当前代码总体已分离。

3. 列表快照定位：
   - `SessionRecord::last_todo_query` 保存 `owner_key/query_type/condition/result_ids/created_at`。
   - `valid_last_visible_todo_query` 校验 owner、query type 和 TTL。
   - `runtime/visible_entity.rs::todo_visible_entity_snapshot` 把 `last_todo_query` 投影为出站 `VisibleEntitySnapshot`。
   - 用户引用机器人列表消息时，`chat_flow::todo_selection_scope_from_visible_entity_snapshot` 将 snapshot 转成 `SelectionScope::Scoped(ids)` 注入 Todo Tool。
   - snapshot 缺失、过期、scope/account/owner 不匹配时返回 `SelectionScope::Blocked`，禁止 fallback 到当前 session 的旧 `last_todo_query`。

缺口：

- 引用旧 Todo 列表并指定编号：当前已有基础能力。
- 引用列表但未说明编号：当前仍需要模型/工具澄清，不能自动猜。
- 引用单条 Todo 回执或提醒消息直接说“删了这个/关掉这个”：当前没有明确的“单条回执绑定 Todo ID”实体类型。出站 snapshot 只来自 `last_todo_query`，而单条新增/编辑回执不一定绑定单个 Todo item。
- 引用提醒消息定位 recurring 实例：当前 outbox payload 没有对入站引用恢复提供可见实体绑定。

## respond / core 中 Todo 相关逻辑

合理通用编排：

- `runtime/respond/agent_outcome.rs` 的通用状态、效果、可信响应块模型基本是领域无关的。
- `runtime/notification.rs` 的 Worker 和 `NotificationSentHook` 抽象是通用 outbox 编排。
- `runtime/respond/tool_runtime.rs` 注册白名单工具、按 policy 裁剪工具是通用编排；但 scoped Todo tool replacement 是 Todo 特例。
- `runtime/respond/chat_flow/todo_guard.rs` 只做 Todo 成功文案验真，不创建业务副作用，短期可保留在 chat flow 安全边界。

明显 Todo 业务泄漏：

- `runtime/respond/todo_flow/receipt.rs`
  - 聚合 Todo 工具结果；
  - 解析 Todo Tool JSON 字段；
  - 渲染新增、修改、完成本次、取消、恢复、合并、删除确认；
  - 刷新相关列表快照；
  - 决定 mutation 后应该刷新 pending/completed/cancelled/all 哪类列表。
- `runtime/respond/todo_flow/format.rs`
  - Todo 列表折叠、提醒展示、重复规则摘要、状态列表文案。
- `runtime/respond/todo_flow/pending.rs`
  - 永久删除确认；
  - TodoClarify 受限 Tool Loop；
  - 旧 pending 兼容。
- `runtime/respond/todo_flow/mod.rs`
  - 自然语言 Todo 查询启发式、slash 查询分派、`remember_todo_query`。
- `runtime/respond/chat_flow/mod.rs`
  - Todo visible entity snapshot 的引用恢复、群聊 interaction session 特判。

这些不是本阶段要立即搬迁的代码，但后续应把“Todo 工具结果 -> 领域 receipt/snapshot plan”迁入 `runtime/tools/todo` 或其子模块，respond 只消费通用 presentation plan。

## runtime/tools/todo 内部现状

当前工具域模块：

- `common.rs`：工具名、参数 schema helper、选择/引用类型、错误输出。
- `scope.rs`：owner/session 加载、last query、last action、task 内部 query、dedup、clarification。
- `selection.rs`：prepare/execute 共用选择预解析和结果映射。
- `create.rs`：创建 draft、时间推断、提醒校验和 outbox 同步。
- `complete.rs`：完成或推进重复待办。
- `edit.rs`：应用编辑补丁、提醒校验和 outbox 同步。
- `cancel.rs`：软取消。
- `delete.rs`：永久删除 pending 创建和目标搜索。
- `restore.rs`：恢复 completed/cancelled。
- `merge.rs`：合并并物理删除 source。
- `list.rs` / `get.rs` / `json.rs`：查询和模型输出。

工具域已经承载大量业务规则，但还缺少更清晰的内部边界：

- create intent / reminder intent / recurrence intent 尚未独立建模。
- 引用定位和列表快照解析混在 `scope.rs`，但 single receipt/reference 绑定能力不足。
- 删除、取消、关闭重复提醒语义没有统一 action model。
- 结果建模主要是面向模型 JSON，respond 再二次解释 JSON 生成用户回执。

建议目标结构：

- `intent.rs`：创建/编辑/删除/关闭/跳过/暂停等自然语言意图中间结构。
- `use_case/create.rs`、`use_case/complete.rs`、`use_case/delete.rs`、`use_case/reminder.rs`。
- `resolver.rs`：编号、last、quoted snapshot、未来单条回执绑定的目标解析。
- `snapshot.rs`：Todo 可见实体快照和引用绑定生成/消费。
- `receipt.rs`：领域结果到可展示 receipt/snapshot plan。
- `recurrence.rs`：业务层 recurrence use case，调用 storage helper 做字段推进。

## 旧路径定位

`runtime/todo`：

- 当前是 `storage::todo` 的重导出 + `ops` + `edit_patch` + `reminder_task` + `template`。
- `ops.rs` 聚合存储变更和 session 副作用，避免 Tool 与 slash 各自实现；短期可保留。
- `reminder_task.rs` 是 Todo 单次提醒和 outbox 衔接，业务性较强，长期可迁到 `runtime/tools/todo/reminder.rs` 或 Todo domain service。
- `template.rs` 是 Todo 推送/卡片渲染，长期应归入 Todo domain presentation，而不是 respond。
- 后续不应在 `runtime/todo` 新增自然语言意图判断、提醒策略、引用定位或回执生成等业务规则；需要新增时应直接放到 `runtime/tools/todo`。

`storage/todo`：

- schema、CRUD、排序、查询、字段归一、重复规则 helper 混在一起。
- 存储层可继续保留 schema/CRUD/查询/低层推进 helper。
- 业务意图判断，如 #391 的 `is_recurring_reminder_intent`，应迁出。

`runtime/respond/todo_flow`：

- 目前承载用户入口和展示兼容。
- 后续目标是只保留“接收 Todo 工具域给出的展示 plan 并写入 response/session”的薄适配。

## 风险和兼容行为

必须保留：

- 内部 ID 不暴露给模型或用户；用户可见编号依赖 session/snapshot。
- 群聊 Todo owner 按 actor 隔离，不回退到群共享。
- 引用快照 owner/scope/account/platform 不匹配时必须 Blocked，不能 fallback。
- Tool 成功文案必须以真实工具结果为准。
- 单次提醒 outbox 只在未来时间入队，编辑提醒取消旧未发送任务。
- 发送成功后重复提醒通过 scheduled_at 锚点推进，避免旧 outbox 连跳。
- 删除确认期间状态变化应 skipped，不越权删除。
- 旧 pending 需要可安全清理或兼容，不能卡死会话。

主要风险：

- “默认 9 点”语义有歧义：每日摘要调度 vs 创建期单次提醒派生。
- “取消等价删除”与现有 `Cancelled` schema/列表/恢复能力冲突。
- “关闭整个重复提醒”当前没有一等动作，模型可能把它误路由为取消 Todo。
- 单条回执和提醒消息缺少可见实体绑定，引用“这个”仍不稳定。
- `respond/todo_flow/receipt.rs` 二次解析 Tool JSON，后续修改工具输出容易造成展示回归。

## 建议补充测试

先补冻结测试：

- 创建纯记录 Todo：无 `due_*`、无 `reminder_at`、无 outbox。
- 截止型 Todo：明确当前“默认 9 点”到底是每日摘要还是创建期 `reminder_at`。
- “周五前写周报，不提醒我”：需要先定义 no-reminder 字段或意图表达。
- 一次性纯提醒：只有 `reminder_at`，无 `due_at`。
- 周期性纯提醒：无 due 时首次 `reminder_at` 推导；后续迁移前后行为一致。
- 完成重复 Todo：advanced 而非 completed，outbox 重排。
- 删除 pending：确认期间状态变化 skipped。
- 引用旧列表 + 编号优先使用 quoted snapshot。
- 引用快照 blocked 时不 fallback。
- 单条回执引用定位：当前缺口，后续实现时新增。
- “取消这个待办/提醒”目标行为：在改动前先冻结旧行为或新增目标行为测试。

## 后续 PR 顺序建议

1. **PR 1：补测试和文档冻结**
   - 冻结 due/reminder/recurrence/outbox/quoted snapshot/cancel/delete 现状。
   - 明确默认 9 点策略的当前实现和目标差异。
   - 对 storage/respond/chat_flow/runtime/todo 中待迁移的业务语义先补回归测试，避免迁移时改变行为。

2. **PR 2：Todo 工具域结果模型**
   - 在 `runtime/tools/todo` 增加领域结果/receipt/snapshot plan。
   - respond 先适配新结构，减少直接解析 Tool JSON。

3. **PR 3：创建意图与提醒策略收口**
   - 拆出 create use case。
   - 把 #391 的周期性纯提醒意图判断和首次 `reminder_at` 推导迁入工具域。
   - 定义 no-reminder / default reminder 的结构化表达。

4. **PR 4：引用定位完善**
   - 为单条 Todo 回执、Todo 列表、提醒消息分别生成可见实体绑定。
   - resolver 统一处理 quoted snapshot、last action、last list、clarification candidates。

5. **PR 5：完成、跳过、关闭重复提醒**
   - 增加 `complete current occurrence`、`skip next reminder`、`disable recurring reminder` 的明确 use case。
   - 保证“今天吃过药了”和“以后别提醒了”走不同动作。

6. **PR 6：取消语义收敛**
   - 先把自然语言“取消”映射到目标删除语义或删除确认语义。
   - 保留历史 `Cancelled` 读取/恢复/清理兼容。
   - 后续再评估是否做 schema 级废弃。

7. **PR 7：respond/todo_flow 瘦身**
   - respond 只保留命令入口、通用事件流和展示编排。
   - Todo 业务规则、目标解析和回执文案迁入工具域。

## 本阶段未修改内容

本报告只做审计，不修改 Rust 业务代码、SQLite schema、测试快照或运行配置。

未执行 Rust 测试。原因：本阶段是纯文档审计，未改变代码行为；按仓库规则，纯文档变更至少执行 `git diff --check`。
