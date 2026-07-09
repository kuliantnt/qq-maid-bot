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

## 工具注册

新增工具通常需要同步注册：

* `qq-maid-core/src/runtime/tools/mod.rs`
* `qq-maid-core/src/runtime/respond/tool_runtime.rs`
* `qq-maid-core/src/config/agent.rs`
* `runtime/config/agent.toml`
* 必要时更新 `qq-maid-core/src/runtime/respond/help.rs`

涉及用户可继续引用或按编号操作的工具，必须提供 visible entity 快照，并保证私聊、群聊、用户、scope、account_id 隔离；无法唯一定位时必须澄清，不得猜测执行。
