# AGENTS.md

本文件约束 `qq-maid-core/src/runtime/tools/` 下业务工具的新增和维护。更通用的项目规则仍以仓库根目录 `AGENTS.md` 为准。

## 业务域边界

新增业务工具时，优先在 `qq-maid-core/src/runtime/tools/<domain>/` 下建立独立业务域。已经存在单文件工具时，小改动可沿用现状；一旦出现多步流程、持久化、可见编号、回执或澄清逻辑，应逐步收敛到独立业务域目录。

推荐结构：

* `<tool>.rs`：Tool 入口，只负责参数解析、上下文校验、调用业务操作、返回结构化结果。
* `ops.rs`：多步业务流程，例如同时更新业务状态、取消 outbox、生成下一次任务。
* `storage.rs` / `storage/`：底层持久化读写和事务边界。
* `receipt.rs`：确定性回执渲染。
* `visible_entity.rs`：列表编号、引用消息定位、可见实体快照。
* `tests.rs`：工具、引用、澄清、状态变化和回执测试。

禁止把具体业务规则散落到 `respond/chat_flow/session/prompt/gateway/llm` 中。Respond 层只负责通用编排、工具调用、结果投影和必要上下文维护；Gateway 只负责平台消息收发；LLM crate 只负责模型协议和 Tool Loop 协议。

## 开发自己的业务工具

新增一个独立业务能力时，先把它当作一个 `<domain>` 设计，而不是只写一个模型函数。可以参考 `todo/` 的拆分方式，但不要照搬 Todo 的所有复杂度；按业务是否需要持久化、用户后续引用、主动通知、确认/澄清来决定交付面。

最小交付面：

* 定义工具目标、可调用场景和风险等级：查询类、写入类、删除类、外部副作用类要分开命名，避免一个 Tool 同时承担过多行为。
* 实现 `qq_maid_llm::tool::Tool`：`metadata()` 给出稳定 `name`、清晰中文 `description` 和严格 JSON Schema；`execute()` 只信任本地校验后的参数。
* 参数解析必须本地校验：字符串长度、数组数量、枚举、时间格式、URL、编号范围和互斥字段都要在 Tool 内或 domain helper 中校验，不能只依赖模型或上游 schema。
* 返回结构化 `ToolOutput::json`：至少包含 `ok`、关键结果字段和 `message`；失败时返回明确 `error_code` 和可展示原因。不得用模型文案伪造成功。
* 明确上下文来源：从 `ToolContext`、`scope_key`、`user_id`、`account_id`、`platform` 派生业务 owner 或投递目标；不要让 Tool 接收或猜测平台 raw id、QQ 字段、内部数据库 ID。
* 对用户可见结果做脱敏和裁剪：输出不包含 token、完整 openid、群 ID、聊天正文、原始请求 envelope、内部异常堆栈或过长外部响应。
* 补充测试：至少覆盖参数合法/非法、成功路径、业务失败路径和输出 JSON 形状；有持久化时覆盖状态变化。

复杂业务交付面：

* 有多步状态变更时，把流程放进 `ops.rs`，Tool 只负责参数解析、选择解析、调用 `ops` 和返回结果。
* 有持久化时，把 SQL 和事务边界放进 `storage.rs` / `storage/`；schema 变化必须走 migration，并考虑历史 `APP_DB_FILE`。
* 有用户后续操作时，实现类似 Todo 的 `visible_entity.rs`：保存用户实际看到的列表快照或最近操作对象，让“第一条”“刚刚那个”只解析到同一 scope / owner 下的可见实体。
* 有自然语言选择、编号、引用或澄清时，选择解析要先产生结构化失败或澄清，不得猜测执行；prepare/execute 需要共享同一套解析边界，避免重试或直接调用时行为不一致。
* 有确定性展示时，把回执渲染收敛到 `receipt.rs` 或 domain format 模块；模型最终回答只能基于真实 Tool 输出总结。
* 有主动推送时，业务模块只生成内容快照并写入 Notification Outbox；投递、重试和失败终态由通知 Worker 负责，业务层不要绕过 outbox 直接发平台消息。
* 有外部网络或高成本调用时，要设置超时、数量上限、摘要长度上限和错误分类；日志只记录脱敏后的工具名、错误码和阶段。

## 可参考 Todo 的做法

`todo/` 是当前最完整的业务工具样板：

* `create.rs` / `list.rs` / `get.rs` / `complete.rs` / `edit.rs` / `restore.rs` / `delete.rs` / `merge.rs` / `recurring.rs` 是 Tool 入口，负责 JSON 参数、上下文、选择解析和结构化输出。
* `common.rs` 收敛工具名、输入上限、错误码、schema helper 和纯参数解析。
* `scope.rs` / `selection.rs` 处理 owner、session、可见编号、最近对象引用、prepare 预解析和重试去重。
* `ops.rs` 统一“存储状态变更 + session 快照维护”的不变量，避免 slash flow 和 Tool Loop 各写一套。
* `visible_entity.rs` 把 Todo 的 domain/kind、列表快照、单条回执快照和引用消息定位适配到通用 visible entity。
* `receipt.rs`、`format.rs`、`template.rs` 保持用户可见文案确定，不把展示规则散落到 Tool 和 respond 层。
* `reminder.rs` / `reminder_worker.rs` 只负责 Todo 提醒 outbox 和每日提醒调度，不直接理解平台发送细节。
* `storage/` 负责 Todo 底层读写、排序、查询、schema、ID 和类型转换。

新业务不需要一开始就拥有这些文件，但一旦出现同类问题，应优先按这个方向拆分，而不是继续向单个 Tool 文件堆逻辑。

## 工具注册

新增工具通常需要同步注册：

* `qq-maid-core/src/runtime/tools/mod.rs`
* `qq-maid-core/src/runtime/respond/tool_runtime.rs`
* `qq-maid-core/src/config/agent.rs`
* `runtime/config/agent.toml`
* 必要时更新 `qq-maid-core/src/runtime/respond/help.rs`

涉及用户可继续引用或按编号操作的工具，必须提供 visible entity 快照，并保证私聊、群聊、用户、scope、account_id 隔离；无法唯一定位时必须澄清，不得猜测执行。

注册检查清单：

* 在 domain `mod.rs` 中导出 Tool 类型和必要的 domain helper；跨模块只暴露稳定门面，不暴露临时解析细节。
* 在 `runtime/tools/mod.rs` 中 `mod` / `pub use` 新 Tool，保持 Core 其他层只依赖工具门面。
* 在 `respond/tool_runtime.rs` 的服务端 `ToolRegistry` 注册实例，并注入所需 store、executor、session 或 notification 依赖。
* 在 `config/agent.rs` 的默认白名单中决定私聊和群聊是否开放；写入类或持久化类默认只考虑私聊，群聊必须显式评估 owner 语义。
* 在 `runtime/config/agent.toml` 更新可版本管理的工具白名单示例和注释，保持默认策略与代码默认值一致。
* 如果用户需要知道显式命令或能力边界，更新 `respond/help.rs` 或对应 README；文档不要复制大段实现细节。
* 如果 Tool 会产生用户可见列表或单条对象，接入通用 visible entity 快照，并在 Respond 请求进入 Tool Runtime 时替换为带作用域限制的 Tool。

## 测试要求

新增或修改业务工具时，按风险选择测试层级：

* 查询类无持久化 Tool：覆盖参数校验、executor/store 调用结果映射、输出裁剪和错误映射。
* 写入类 Tool：覆盖成功写入、失败不伪造成功、重复调用/重试边界、session 或最近对象快照维护。
* 编号/引用类 Tool：覆盖可见编号、`last` / 最近对象、跨 scope / owner / account 隔离、目标不存在和歧义澄清。
* 通知类 Tool：覆盖 outbox upsert/cancel、发送成功 hook、失败或过期任务不重复推进业务状态。
* 配置或注册变更：覆盖默认白名单、场景裁剪和未启用工具不可见。

文档-only 变更至少执行 `git diff --check`；代码变更按根 `AGENTS.md` 的本地检查要求执行。
