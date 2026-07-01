# 私聊 Agent Loop 语义基线

## 背景

本记录对应 #137，是后续统一 Agent Loop 的前置基线。当前任务只梳理并收敛私聊普通聊天的完整工具循环语义，不修改 QQ 输出方式，不实现流式 Agent。

## 当前调用链

私聊普通聊天的入口由 Core 统一决策：

```text
CoreService::respond
  -> RustRespondService::plan_core_respond
  -> RustRespondService::respond_with_plan
  -> RustRespondService::handle_chat
  -> LlmChatService::respond_with_tools
  -> LlmProvider::chat_with_tools
  -> OpenAI Responses 或 Chat Completions Tool Loop
  -> ToolRegistry 白名单工具
  -> CoreRespondOutput::Complete
```

`RespondPlan::CompleteToolLoop` 只用于私聊普通消息。pending 确认、slash 命令、确定性 Todo 查询、群聊和 provider 不支持工具调用的请求不会进入完整工具循环。

## 行为归属

| 行为 | 当前归属 |
|------|----------|
| 直接回答 | Provider Tool Loop 返回无工具调用的最终文本；Core 只做会话保存、标题调度和响应包装 |
| 单轮 / 多轮工具调用 | `qq-maid-llm` 的 provider Tool Loop 控制轮次和协议回填 |
| 工具结果回填 | Provider 适配层按各自协议写回 `function_call_output` 或 `role=tool` 消息 |
| 最大轮数 | Core 从配置注入 `TOOL_CALLING_MAX_ROUNDS`；LLM Tool Loop 统一在超过轮数时返回 `tool_loop_limit` |
| 工具业务失败 | Tool 输出 `{"ok":false,...}` 视为失败，仍回填给模型，由最终回复解释真实失败 |
| 工具执行异常 | LLM Tool Loop 转成 `{"ok":false,"error":...}` 回填，不伪造成功 |
| 同轮工具依赖 | `ToolCallDependency::PreviousCallSuccess` 由 LLM 内部统一执行；前项失败时后项跳过 |
| Todo 成功文案验真 | Core `chat_flow::todo_guard` 只看本轮真实 `tool_results`，不再根据用户文本强制 required-tool |
| Session/history 保存 | Core `handle_chat` 保存最终回复；Tool 写入期间产生的 session pending / Todo 快照先读取最新 session 再合并聊天状态 |
| diagnostics | Core 生成 `tool_loop_executed_tools`、`todo_success_claimed`、`todo_success_verified` 等诊断字段 |
| 工具副作用幂等 | 具体业务 Tool 负责；Todo Tool 使用 `tool_call_id` 和 session / store 边界保护重复调用 |

## 本次收敛

Responses 和 Chat Completions 两套协议仍保留各自 payload、解析和回填格式，但工具执行语义已收敛到 `qq-maid-llm/src/provider/tool_loop.rs`：

- 工具调用稳定 ID 生成；
- JSON 参数准备；
- 白名单工具执行；
- `ok:false` 业务失败识别；
- 执行异常转结构化工具输出；
- 前项失败后的依赖跳过；
- `executed_tools` 与 `tool_results` 轨迹记录。

Core 与 Provider 的边界保持不变：Provider 不理解 Todo 是否写入成功，只记录通用工具轨迹；Todo 成功文案是否可透传仍由 Core 根据业务工具输出字段判断。

## 保留风险

- `task_id` 当前仍优先复用入站 `message_id`，多轮多工具场景下只适合作为本轮消息级关联，不是独立工具调用生命周期 ID。
- Tool Loop 仍是内部完整闭环，未提供 `ToolStarted` / `ToolFinished` 等 Core 事件；后续接入流式 Agent 前需要新增事件模型和 QQ 发送所有权设计。
- Provider 协议差异仍存在，例如 Responses 可在最后一轮显式设置 `tool_choice=none`，Chat Completions 维持兼容交集 `tool_choice=auto`。

## 验证范围

本基线应持续覆盖：

- 无工具回答；
- 单工具、多工具、多轮工具调用；
- 工具业务失败和执行异常；
- 最大轮数；
- Todo 写操作成功验真；
- `/查`、群聊、slash、确定性 Todo 查询不进入完整 Tool Loop。

## Prompt 边界

随着工具执行、失败处理、依赖关系、轮次限制和业务成功验真下沉到代码，私聊 Agent 不再依赖大段 Prompt 维持工具调用正确性。

### 职责划分

| 职责 | 归属 | 位置 |
|------|------|------|
| 工具使用原则（何时调用、禁止编造、失败处理基调） | Prompt | `maid_system.md`（仅通用原则，不含具体工具描述） |
| 工具能力、参数 schema、适用范围 | Tool Schema | 各 Tool 的 `ToolMetadata` |
| JSON 参数准备与校验 | 代码 | `tool_loop.rs` `prepare_json` |
| 白名单工具执行 | 代码 | `ToolRegistry` + `tool_loop.rs` |
| `ok:false` 业务失败识别与回填 | 代码 | `tool_loop.rs` `execute_prepared_call` |
| 异常转结构化错误输出 | 代码 | `tool_loop.rs`（catch 后生成 `{"ok":false,"error":...}`） |
| 前项失败后的依赖跳过 | 代码 | `tool_loop.rs` `ToolCallDependency::PreviousCallSuccess` |
| 最大轮数限制 | 代码 | Core 注入 `TOOL_CALLING_MAX_ROUNDS`，LLM Tool Loop 超限返回 `tool_loop_limit` |
| 工具调用 ID 生成与稳定 | 代码 | `tool_loop.rs`（优先 model 下发的 call_id，无则本地生成） |
| Todo 成功文案验真 | 代码 | Core `chat_flow::todo_guard`，依赖真实 `tool_results` |
| 协议回填（`function_call_output` / `role=tool`） | 代码 | 各 Provider 适配层 |

### 系统 Prompt 中的工具内容

当前 `maid_system.md` 默认内置 prompt 中包含一条通用工具调用优先级规则，核心约束是三条：

1. **依赖外部状态时优先使用工具** — 待办、日程、记忆、文件、知识库、项目状态、实时信息等需要外部状态时必须依赖工具或已注入资料。
2. **禁止假执行** — 没有收到工具成功结果时，不得使用“已添加”“已记录”“已删除”“已更新”等表述。
3. **回复前判断** — 区分闲聊/解释/创作 vs 读外部状态 vs 写/改/删外部状态，后两类必须先依赖工具再回复。

这三个原则是模型的“行为底线”，不包含任何具体工具名称、参数或返回格式。

### 不再需要的 Prompt 内容

以下内容已由代码接管，不应出现在系统 Prompt 或业务 Prompt 中：

- ❌ 具体工具的调用顺序或组合规则 → 由 Tool Schema 和依赖链处理
- ❌ 工具失败的 JSON 格式要求 → `tool_loop.rs` 统一生成 `{"ok":false,"error":...}`
- ❌ 多轮调用轮次限制 → `TOOL_CALLING_MAX_ROUNDS` 运行时控制
- ❌ 依赖工具的前后关系 → `ToolCallDependency` 声明式控制
- ❌ Todo 编号绑定规则 → 代码保持 prepare-before-execute 保证编号稳定
- ❌ 成功/失败状态的具体判断逻辑 → 代码检查 `tool_results` 而非模型文案

### 未来演进方向

后续统一 Agent Loop 应继续维持这个边界：新增工具行为（如并发执行、重试策略、超时处理）优先在 `tool_loop.rs` 实现，只在模型需要理解新行为语义时才更新 `maid_system.md` 的通用原则段。
