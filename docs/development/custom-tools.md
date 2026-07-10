# 自定义 Tool 二开接入指南

本项目的 Tool Calling 不是让模型直接执行任意代码，而是由服务端注册一组明确、受控、可裁剪的工具。模型只能看到当前聊天场景白名单里的 Tool，也只能调用服务端已经注册到 `ToolRegistry` 的 Tool。

这篇文档面向二次开发：先把一个只读查询类 Tool 跑通，再考虑写入、删除、定时任务或本地命令执行。

## 基本接入流程

新增一个 Tool 通常需要 6 步：

1. 在 `qq-maid-core/src/runtime/tools/` 下新增工具文件或业务域目录。
2. 实现 `qq_maid_llm::tool::Tool` trait。
3. 在 `qq-maid-core/src/runtime/tools/mod.rs` 中导出 Tool 类型。
4. 在 `qq-maid-core/src/runtime/respond/tool_runtime.rs` 中注册到服务端 `ToolRegistry`。
5. 在 `qq-maid-core/src/config/agent.rs` 中加入工具名校验集合，并决定默认私聊 / 群聊白名单。
6. 在 `runtime/config/agent.toml` 的对应场景 `enabled_tools` 中开放。

如果工具需要自然语言路由、确定性展示、可见实体、确认/澄清或写入后诊断，还需要补充后文的 Tool Loop 接入文件；不要只注册 Tool 就把业务判断写进 `respond/chat_flow` 或 `respond/tool_route`。

第 5 步容易漏：当前 `agent.toml` 会校验工具名，未加入 `qq-maid-core/src/config/agent.rs` 的工具即使写进配置也会启动失败。当前实现还没有单独的 `ALL_TOOLS` 常量，校验逻辑复用 `DEFAULT_PRIVATE_ENABLED_TOOLS` 作为允许集合；所以新增工具至少要进入这个常量。若工具不适合私聊默认开放，需要先把“全量允许工具集合”和“私聊默认工具集合”拆开，再接入该工具。

这 6 步只是最小注册链路。工具若包含持久化、确认、用户可见编号、主动通知或跨存储副作用，还必须补充领域操作、pending、回执和可见实体等接入，不能把真实业务完成状态交给模型自由描述。

## 当前调用链

实际运行时不是“写了 Tool 模型就能直接调”，而是下面这条链路：

1. `qq-maid-core/src/runtime/respond/tool_runtime.rs` 启动时构造服务端全量 `ToolRegistry`。
2. 每次普通纯文本 Agent Chat 调用前，`ToolRuntime::registry_for_chat()` 根据当前场景策略读取 `policy.enabled_tools`。
3. `ToolRegistry::subset()` 只保留本场景允许的工具；未注册或未在白名单里的工具对模型不可见。
4. Todo 这类需要用户可见编号和引用恢复的工具，会在 `replace_scoped_tools_from_request()` 中替换成带当前请求快照的受限实例。
5. LLM crate 只负责 Tool Loop 协议和执行注册表里的 Tool，不知道 Todo、RSS 或服务器命令的业务规则。

默认策略也按这个链路生效：私聊 `tool_calling_enabled = true`，默认开放 `DEFAULT_PRIVATE_ENABLED_TOOLS`；群聊 `tool_calling_enabled = false`，即使开启也默认只开放 `DEFAULT_GROUP_ENABLED_TOOLS` 里的查询类工具。

## Agent Chat 语义提示和后处理接入

Tool 注册和场景 policy 决定“模型能不能调用”。普通消息通过能力约束后统一进入 Agent Chat，由模型原生响应决定直接回答还是 Tool Call；领域语义提示、工具结果的确定性展示、写入验真和后续引用仍需要在 tools 层补齐。

按能力选择接入面：

- 自然语言语义提示：在 `qq-maid-core/src/runtime/tools/<domain>/route.rs` 暴露轻量分类门面。`respond/agent_route.rs` 只做能力约束和通用调度；分类结果只能用于 status/diagnostics，不能决定是否向模型暴露 Tool Schema。
- 普通聊天弱语义：通用闲聊、创作、解释、本地长文本整理放在 `respond/plain_chat_route.rs`。不要把具体工具关键词放进这个文件，也不要把它重新变成前置 Router。
- 只读结果展示：先在 `qq-maid-core/src/runtime/tools/agent_presenters.rs` 增加 `ToolExecutionResult -> ToolExecutionOutcome` 适配。
- 多步写入或跨存储副作用：在 `qq-maid-core/src/runtime/tools/<domain>/ops.rs` 提供领域操作门面，Tool 文件只做参数解析、上下文校验和结构化结果返回。
- 确认和澄清：把领域 payload、状态机和恢复执行放在 `runtime/tools/<domain>/pending.rs` 或 `runtime/tools/<domain>/flow/pending.rs`；`respond/pending.rs` 只保留跨域 envelope / 会话写入 helper。
- 写入、删除、可见实体、主动通知、可信回执或复杂诊断：在 `qq-maid-core/src/runtime/tools/<domain>/agent_turn.rs` 增加 domain adapter，并按需拆出 `receipt.rs` / `visible_entity.rs`；`runtime/tools/agent_turn.rs` 只消费抽象 outcome / diagnostics。
- Todo 类可见编号或“刚才那个”引用：实现 visible entity 快照，并保证 private / group / actor / account / owner 隔离；不要暴露数据库内部 ID。
- 查询快照新鲜度：通用时间窗口使用 `runtime/freshness.rs`，业务专属判断放在 `runtime/tools/<domain>/freshness.rs`。

当前 Todo 的完整样板包括：

```text
qq-maid-core/src/runtime/tools/todo/
  route.rs              # Todo 自然语言语义与状态提示候选
  ops.rs                # 多步写入与通知 outbox 等领域操作门面
  pending.rs            # Todo 专属 pending payload
  flow/pending.rs       # 确认/澄清状态机与受限 Tool Loop 恢复
  receipt.rs            # Tool 结果聚合、状态判断和确定性回执
  agent_turn.rs         # 接入通用后处理、成功验真和诊断适配
  visible_entity.rs     # 可见编号与最近操作对象快照
  success_guard.rs      # Todo 成功文案守卫
  interaction_state.rs  # Todo 最近交互状态摘要
  freshness.rs          # Todo 查询快照新鲜度
```

新增工具域不必一开始照搬全部文件；只有出现同类复杂度时才拆。原则是：具体业务判断留在 `runtime/tools/<domain>/`，`runtime/respond/` 只做跨域调度和响应拼装。

## 最小 Tool 示例

下面示例新增一个服务器状态检查工具。它只返回固定 JSON，用来说明最小接入面；真实检查磁盘、进程或服务状态时，请参考后文“本地命令类 Tool 的安全要求”。

```rust
use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::error::LlmError;

const TOOL_NAME: &str = "server_healthcheck";

#[derive(Clone)]
pub struct ServerHealthcheckTool;

impl ServerHealthcheckTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ServerHealthcheckTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: TOOL_NAME.to_owned(),
            description: "检查服务器当前运行状态，例如机器人进程、磁盘、内存等。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        Ok(ToolOutput::json(json!({
            "ok": true,
            "status": "ok",
            "message": "server is healthy"
        })))
    }
}
```

建议保存为：

```text
qq-maid-core/src/runtime/tools/server_healthcheck.rs
```

如果工具后续会变复杂，例如包含多种状态检查、持久化记录、主动通知或用户可继续引用的对象，优先改成目录：

```text
qq-maid-core/src/runtime/tools/server_healthcheck/
  mod.rs
  ops.rs
  tests.rs
```

## 导出 Tool

在 `qq-maid-core/src/runtime/tools/mod.rs` 中加入：

```rust
mod server_healthcheck;

pub use server_healthcheck::ServerHealthcheckTool;
```

如果使用业务域目录，则在该目录的 `mod.rs` 中导出稳定门面，再由 `runtime/tools/mod.rs` 对外 `pub use`。不要把临时解析 helper 暴露给 Respond、Gateway 或 LLM crate。

## 注册 Tool

在 `qq-maid-core/src/runtime/respond/tool_runtime.rs` 中引入：

```rust
use crate::runtime::tools::ServerHealthcheckTool;
```

然后加入 `ToolRegistry` 注册列表：

```rust
Arc::new(ServerHealthcheckTool::new()),
```

注册后，服务端才知道这个工具存在。模型实际能不能看到，还要看当前聊天场景的 `enabled_tools` 白名单。

## 更新配置校验集合

在 `qq-maid-core/src/config/agent.rs` 中，把工具名加入 `DEFAULT_PRIVATE_ENABLED_TOOLS`。这是当前配置校验使用的允许集合，同时也是私聊缺省开放工具列表。

私聊默认开放示例：

```rust
const DEFAULT_PRIVATE_ENABLED_TOOLS: &[&str] = &[
    "get_weather",
    "server_healthcheck",
];
```

群聊默认列表由 `DEFAULT_GROUP_ENABLED_TOOLS` 控制，只应放低风险查询类工具。写入类、删除类、本地命令类和外部副作用类工具不要默认加入群聊白名单。

如果新增的是高风险工具，而且也不希望私聊缺省开放，先重构 `agent.rs`：新增类似 `ALL_ENABLED_TOOL_NAMES` 的全量允许集合，让 `validate_scene_enabled_tools()` 校验该集合，再把默认白名单继续保留为按场景的策略集合。

## 在配置中开放 Tool

在 `runtime/config/agent.toml` 中，把工具名加入对应场景的 `enabled_tools`。

私聊示例：

```toml
[scenes.private]
tool_calling_enabled = true
enabled_tools = [
  "get_weather",
  "web_search",
  "server_healthcheck",
]
```

群聊默认不建议开放有副作用的工具。查询类工具可以按需开放；写入类、执行类、删除类工具应先确认 owner 语义、权限、日志脱敏和误触发风险。

## 参数设计建议

Tool 的参数应该尽量小、明确、可校验。JSON Schema 只是模型侧提示和基础约束，`execute()` 或 `prepare()` 里仍然必须重新校验。

`prepare()` 适合在副作用发生前完成本地预绑定，例如把用户可见编号解析成稳定内部对象，并通过 `ToolCallDependency` 声明同轮调用是否依赖前一项成功；真正的权限校验和写入仍由 Tool / 领域操作层负责。无此需求的只读 Tool 可以沿用 trait 的默认实现。

推荐：

```json
{
  "action": "healthcheck"
}
```

不推荐：

```json
{
  "command": "bash -c 'curl xxx | sh'"
}
```

常见校验项：

- 字符串是否为空、是否超长。
- 枚举值是否属于允许集合。
- 数组数量是否超过上限。
- 时间、URL、编号和互斥字段是否合法。
- 当前 `ToolContext` 是否具备需要的用户、scope 或群成员角色。

对于需要把“第一条”“刚才那个”这类用户可见引用绑定到真实对象的 Tool，优先参考 Todo 的 visible entity 快照机制，不要把数据库内部 ID 暴露给模型或用户。

请求级身份和作用域必须从服务端生成的 `ToolContext` 读取。不要在 JSON Schema 中让模型提交 `user_id`、`scope_id`、角色或 `tool_call_id`，也不要用模型参数覆盖这些字段。

如果 Tool 需要普通聊天的自然语言路由、上下文续指、成功文案验真或工具失败回退文案，优先在对应 `runtime/tools/<domain>/` 下提供小门面，让 `runtime/respond/` 只调用这些门面并适配聊天输出结构。不要把具体工具关键词、状态字段、成功判断和失败文案长期堆在 respond/chat_flow 或 respond/tool_route 里。

## 输出设计建议

返回值优先使用 `ToolOutput::json`，并保持结构稳定。成功结果至少包含：

```json
{
  "ok": true,
  "message": "已完成检查"
}
```

业务失败也要返回明确错误，或者返回 `LlmError`：

```json
{
  "ok": false,
  "error_code": "server_status_unavailable",
  "message": "暂时无法读取服务状态"
}
```

不要用模型最终回答伪造成功。Todo、Memory、RSS、命令执行、通知推送等结果必须以真实工具、存储或外部调用返回为准。

输出必须做脱敏和裁剪，不返回 token、环境变量、数据库路径、私有配置、完整 openid、群 ID、聊天正文、原始平台 event envelope、内部异常堆栈或过长日志。

## 本地命令类 Tool 的安全要求

如果 Tool 需要执行服务器本地命令，必须遵守以下原则：

1. 不允许模型传入完整 shell 命令。
2. 只允许调用服务端预定义的白名单动作。
3. 不拼接 shell 字符串。
4. 优先使用固定 argv。
5. 设置超时时间。
6. 限制输出长度。
7. 不返回 token、环境变量、数据库路径、私有配置等敏感信息。
8. 高风险操作需要用户确认。
9. 群聊默认禁用。

推荐设计：

```rust
match action {
    "healthcheck" => run_healthcheck().await,
    "disk_usage" => run_disk_usage().await,
    "bot_status" => run_bot_status().await,
    _ => reject_unknown_action(),
}
```

不要这样做：

```rust
Command::new("sh")
    .arg("-c")
    .arg(model_input)
    .output()
    .await;
```

## 定时任务和主动推送

对于定时执行、本地命令执行、自动推送等能力，建议拆成两层：

1. Tool 层：负责创建、查询、修改任务，并写入持久化状态或通知 outbox。
2. Worker 层：负责按时间触发任务、执行动作、处理重试和失败终态。

不要把定时循环、命令执行、消息推送全部塞进 Tool 的 `execute()`。Tool Loop 是一次请求内的受控调用，不适合长期驻留任务。

同一次用户操作需要同时更新业务记录、取消旧 outbox、创建下一次提醒等多步动作时，优先放入 `<domain>/ops.rs`；只有需要新增底层读写或事务语义时才扩展 storage。

## 测试要求

新增 Tool 后至少补充以下测试：

1. 合法参数可以正常执行。
2. 缺少必要参数会失败。
3. 非法参数会失败。
4. 未注册工具不能被调用。
5. 输出结果是合法 JSON。
6. 输出不会泄漏敏感信息。
7. 场景白名单会正确裁剪模型可见工具。
8. 群聊场景不会默认开放高风险工具。

常用验证命令：

```bash
cargo fmt --all -- --check
cargo check -p qq-maid-core
cargo test -p qq-maid-core runtime::tools
cargo test -p qq-maid-core runtime::respond::tests::chat
```

如果改了配置解析或默认白名单，补充：

```bash
cargo test -p qq-maid-core config::tests
```

如果只修改本文档，至少执行：

```bash
git diff --check
```

## 推荐开发顺序

建议先写一个只读查询类 Tool，例如：

- `server_healthcheck`
- `disk_usage`
- `bot_status`
- `recent_logs_summary`

确认 Tool 注册、调用、返回、配置白名单和测试都跑通后，再开发写入类、删除类、本地命令类或主动推送类 Tool。

已有实现可参考：

- `qq-maid-core/src/runtime/tools/weather.rs`：简单查询类 Tool，包含参数校验和输出整理。
- `qq-maid-core/src/runtime/tools/search.rs`：复用执行器的联网查询 Tool，包含输入长度限制和错误映射。
- `qq-maid-core/src/runtime/tools/todo/`：复杂业务域样板，包含领域操作、pending、持久化、可见编号、提醒、可信回执和测试。
- `qq-maid-core/src/runtime/tools/AGENTS.md`：维护者级约束，适合改复杂业务前阅读。
