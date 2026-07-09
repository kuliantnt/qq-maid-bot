# 任务：Todo/Reminder 业务逻辑向 runtime/tools/todo 收口

## 背景

当前 Todo/Reminder 业务语义散落在 storage、runtime/todo、runtime/respond/todo_flow、chat_flow、tool_runtime、notification/outbox 等多处，导致后续维护和扩展困难。

本轮目标不是做保守的小修，而是进行第一轮架构收口：将 Todo 相关业务实现尽量迁入 `qq-maid-core/src/runtime/tools/todo/`，其他层只保留通用能力、薄适配或存储基础能力。

本任务允许在开发分支中做较大范围迁移；如果部分边缘行为短期变化，可以在完成报告中说明。但不能迁出一个新的屎山，也不能把 Todo 业务继续留在 respond/storage 中。

## 目标

完成后应达到：

1. Todo 创建、编辑、完成、删除、取消、提醒、重复提醒、引用定位、回执生成等业务语义，优先集中到 `runtime/tools/todo` 及其子模块。
2. `storage/todo` 只保留 schema、CRUD、查询、字段归一、无自然语言意图的低层 helper。
3. `runtime/respond/*` 只保留通用编排、入口适配、事件流/展示适配和安全校验。
4. `runtime/todo` 尽量瘦身为过渡 facade 或复用薄层；不要继续新增业务规则。
5. 自然语言“取消这个待办/提醒”默认按删除语义处理，不再优先引入复杂 cancel/cancelled 状态机。
6. 现有 `Cancelled` schema 和历史数据兼容可以暂时保留，不要求本轮做 schema 级删除。

## 实现要求

1. 先阅读仓库根目录的 `AGENTS.md`、`README.md` 和 Todo/Reminder 相关模块。
2. 使用搜索确认当前调用链，不要仅凭文件名猜测。
3. 重点排查并迁移以下业务逻辑：
   - `storage/todo/recurrence.rs` 中依赖 `raw_text/title/detail` 的自然语言意图判断。
   - `runtime/todo/reminder_task.rs` 中属于 Todo 领域的提醒同步、重复推进业务逻辑。
   - `runtime/todo/template.rs` 中属于 Todo 展示/推送领域的模板逻辑。
   - `runtime/respond/todo_flow/receipt.rs` 中对 Todo Tool JSON 的二次解释和回执生成逻辑。
   - `runtime/respond/todo_flow/format.rs` 中 Todo 列表、提醒、重复规则、状态文案等展示逻辑。
   - `runtime/respond/todo_flow/pending.rs` 中 Todo 删除确认、澄清恢复、旧 pending 兼容逻辑。
   - `runtime/respond/chat_flow` 中 Todo visible entity snapshot / quoted snapshot 的特殊处理。
4. 在 `runtime/tools/todo` 下建立更清晰的内部结构，可按仓库实际情况选择命名，例如：
   - `intent.rs`
   - `use_case/create.rs`
   - `use_case/complete.rs`
   - `use_case/delete.rs`
   - `use_case/reminder.rs`
   - `resolver.rs`
   - `snapshot.rs`
   - `receipt.rs`
   - `recurrence.rs`
5. 不要求一次性完美重构，但新增 Todo 业务逻辑不得继续写入 storage/respond/chat_flow。
6. 如果某些逻辑短期无法迁移，应保留清晰 TODO 注释，并在完成报告中说明原因、剩余位置和后续迁移建议。
7. 对“取消”语义做第一步收敛：
   - 用户自然语言表达“取消这个待办”“取消这个提醒”时，默认走删除/删除确认语义。
   - 暂时保留 `Cancelled` 状态读取、恢复、清理兼容。
   - 不要求本轮删除数据库字段或做破坏性 migration。
8. 保持 NotificationWorker 的通用性：
   - notification worker 不应反查 Todo 或解释 Todo 业务。
   - Todo 重复提醒推进应继续通过 Todo 领域 hook / service 完成。

## 禁止事项

- 不要把 Todo 业务逻辑从一个旧位置搬到另一个非 tools/todo 的新位置。
- 不要在 storage 层读取自然语言文本并判断用户意图。
- 不要让 respond 层继续深度解析 Todo Tool JSON 来决定业务结果。
- 不要为了迁移而大规模改无关格式、无关模块或公开接口语义。
- 不要删除仍被历史数据、旧 pending 或兼容路径依赖的 `Cancelled` 相关能力，除非有明确 migration 和兼容方案。
- 不要伪造测试结果或构建结果。

## 最小验证要求

本轮不要求补完整冻结测试，但至少需要做最小冒烟验证：

1. 创建普通 Todo。
2. 创建一次性纯提醒。
3. 创建周期性纯提醒，并确认首次 `reminder_at` 能生成。
4. 完成周期 Todo 时应推进下一周期，而不是直接 completed。
5. 删除 Todo 可正常工作。
6. 自然语言“取消这个待办/提醒”默认进入删除语义。
7. 引用旧 Todo 列表并按编号操作时，仍优先使用 quoted snapshot。
8. Notification outbox 的单次提醒入队和重复提醒推进核心链路不应明显断裂。

如项目已有相关测试，请运行对应测试；如测试大面积失败，请区分：
- 本次迁移引入的问题；
- 原有测试依赖旧结构导致需要更新；
- 暂时接受的行为变化。

## 测试要求

至少执行：

1. `cargo fmt --all`
2. Todo/Reminder 相关单测或集成测试
3. `cargo check --workspace`

如果无法执行完整测试或测试失败，必须在完成报告中说明具体命令、失败原因和是否与本次迁移相关。

## 完成后输出

完成后请说明：

1. 本次迁移的总体思路。
2. 哪些 Todo 业务逻辑已迁入 `runtime/tools/todo`。
3. 哪些逻辑仍留在 storage/respond/runtime/todo，为什么暂时保留。
4. “取消等价删除”做到了哪一层。
5. 修改了哪些文件。
6. 执行了哪些测试和检查。
7. 测试结果。
8. 仍可能存在的风险和下一轮建议。