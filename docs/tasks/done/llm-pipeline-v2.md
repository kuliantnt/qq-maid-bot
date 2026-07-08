# 任务：新建 `qq-maid-llm` crate，重构完整 LLM 调用链路

## 背景

当前项目的 LLM 基础设施仍位于 `qq-maid-core`，主要包括：

- OpenAI Responses API；
- OpenAI Chat Completions fallback；
- DeepSeek；
- 模型候选链与自动降级；
- SSE 解析；
- Token usage 和健康状态；
- `/查` 使用的 OpenAI Web Search。

经现状审计：

1. OpenAI Responses 主链路已经使用 `reqwest` 自研实现，不依赖 `rig-core`。
2. `rig-core` 主要残留在 OpenAI Chat Completions fallback 和 DeepSeek 实现中。
3. 聊天 Responses 与 `/查` 各自维护了一套 SSE frame 解析，存在重复。
4. Provider、路由、协议处理和业务 Flow 都在 `qq-maid-core`，缺少明确的 crate 边界。

本次需要新建独立的 `qq-maid-llm` library crate，将完整 LLM 基础设施从 core 中分离，并彻底移除 `rig-core`。

本次以迁移现有能力、保持行为兼容为主，不借机重新设计整套业务架构。

## 目标

完成后应达到：

1. 新增独立的 `qq-maid-llm` library crate。
2. 所有模型协议、Provider、路由、fallback、SSE、usage 和健康状态由该 crate 管理。
3. `qq-maid-core` 只负责业务 Prompt、会话、记忆、Todo、翻译和回复排版。
4. `qq-maid-llm` 不依赖 `qq-maid-core`。
5. 使用 `reqwest` 替换现有 `rig-core` Chat Completions 和 DeepSeek 实现。
6. 保持普通聊天、专用模型调用、自动降级和 `/查` 的现有行为。
7. 从依赖和代码中彻底移除 `rig-core`。

## 实现要求

### 1. 新增独立 crate

在 workspace 中新增 `qq-maid-llm` library crate。

依赖方向应保持为：

```text
qq-maid-gateway
        ↓
qq-maid-core
        ↓
qq-maid-llm
        ↓
qq-maid-common / reqwest / serde / tokio
```

禁止 `qq-maid-llm` 反向依赖 `qq-maid-core`。

目录结构应根据实际代码职责设计，不要为了匹配预设结构创建大量只有少量代码的文件。

### 2. 迁移 LLM 公共类型和基础设施

将以下内容迁入 `qq-maid-llm`，具体名称可根据现有实现适当调整：

- `LlmProvider` trait 和动态 Provider 类型；
- `ChatMessage`、`ChatRole`、`ChatRequest`、`ChatOutcome`；
- `ModelId`、`ModelRoute`、`ModelProvider`；
- `TokenUsage`；
- `LlmMetrics`、`MetricsRecorder`；
- 模型候选链路由和 fallback 判定；
- `UpstreamStatus`、`ObservedProvider` 等健康观测能力；
- 仅与模型调用有关的 `LlmError`。

core 应直接使用 `qq-maid-llm` 导出的类型，不要在两个 crate 中保留两套等价结构。

### 3. 控制错误模型改动范围

`qq-maid-llm::LlmError` 只负责模型调用相关错误，例如：

- Provider 配置错误；
- 网络和超时；
- HTTP 上游错误；
- SSE 或响应协议错误；
- 空回复；
- 候选模型全部失败。

当前 core 中天气、火车、Prompt 配置、RSS、HTTP 路由等非 LLM 代码对旧 `LlmError` 的使用，只进行维持职责边界和编译所需的最小调整。

本次不要展开全仓 `AppError` 体系重构。core 只需在 LLM 调用边界完成错误转换，并保持现有用户侧错误格式和文案兼容。

### 4. 迁移 OpenAI Responses

将现有 OpenAI Responses 实现及其测试迁入 `qq-maid-llm`，保持行为不变。

必须保留：

- system/user 历史使用 `input_text`；
- assistant 历史使用 `output_text`；
- completed-only 响应提取；
- delta、completed、failed、incomplete、error 等 SSE 事件处理；
- SSE 跨 chunk、CRLF 和未知非关键事件兼容；
- 空流补非流；
- 流式失败后按现有规则补非流；
- usage 和 cached token 提取；
- HTTP 错误正文裁剪；
- 200 响应但正文为空时返回明确错误，不伪造成功。

### 5. 使用 `reqwest` 替换 `rig-core`

不要引入 `genai` 或其他新的 LLM SDK。

使用 `reqwest` 实现 OpenAI Chat Completions：

- 支持流式和非流式；
- 保持当前消息角色和内容语义；
- 支持自定义 endpoint、API key、timeout 和输出 token 限制；
- 流式解析 `choices[].delta.content`；
- 支持 `[DONE]`；
- 提取 prompt、completion、total 和 cached token usage；
- 保持空流补非流行为；
- 认证失败、限流、超时、5xx 和非标准错误正文应正确分类；
- 空回复应返回明确错误。

DeepSeek 使用 OpenAI 兼容的 Chat Completions adapter，不要再维护一套独立 SDK 封装。只保留其 base URL、认证和模型规则差异。

### 6. 保持模型候选链行为

迁移现有模型路由和自动降级逻辑，保持：

- 按候选顺序调用；
- 成功后立即停止；
- timeout、限流、上游不可用等临时错误可以尝试下一候选；
- 配置错误、非法请求等永久错误立即终止；
- 全部候选失败时返回聚合错误；
- OpenAI Responses → Chat Completions fallback 规则不变；
- 旧 `LLM_PROVIDER=auto` 等兼容逻辑不变；
- `TITLE_MODEL`、`TODO_MODEL`、`MEMORY_MODEL`、`COMPACT_MODEL`、`TRANSLATION_MODEL` 等业务模型路由互不干扰。

### 7. 合并 SSE frame 解析

将当前聊天 Responses 和 `/查` 重复的 SSE frame 解析合并到 `qq-maid-llm` 的单一实现中。

只合并通用能力，例如：

- 从字节流中提取完整 SSE frame；
- 处理 CRLF；
- 解析 `event:` 和 `data:`；
- 识别 `[DONE]`。

聊天 Responses 和 Web Search 的具体事件处理逻辑可以继续分开，不要为了统一而建立过度泛化的事件体系。

### 8. 迁移 `/查` 的模型协议实现

将 `/查` 当前使用的 OpenAI Responses + `web_search` 工具协议实现迁入 `qq-maid-llm`，包括：

- 请求 payload；
- HTTP transport；
- SSE 处理；
- answer 提取；
- sources 提取；
- 流式文本增量。

core 继续保留：

- `/查`、`/查询`、`/search` 命令解析；
- 权限和业务判断；
- 回复排版；
- session 记录；
- 用户侧错误提示。

命名应明确体现 Web Search 能力，避免继续使用过于宽泛的 Query 类命名。

保留 `/查` 所需的最小流接口即可，例如文本增量和最终完成结果。不要在本次设计通用 tool call、reasoning 或完整 LLM 事件总线。

### 9. 配置边界

`qq-maid-llm` 的配置只包含 Provider 基础配置，例如：

- Provider 模式；
- 默认模型候选链；
- OpenAI / DeepSeek API key 和 base URL；
- OpenAI API 模式；
- Web Search 模型；
- timeout；
- stream 开关；
- max output tokens。

Todo、标题、记忆、Compact、翻译等业务模型配置继续由 core 管理。core 将对应配置解析成 `ModelRoute`，通过 `ChatRequest` 传给 LLM 层。

现有环境变量名称和默认语义必须保持不变。

### 10. 提供最小统一入口

`qq-maid-llm` 应提供一个统一服务入口，其职责仅限于：

- 创建和复用 HTTP client；
- Provider 组装；
- 模型候选链执行；
- fallback；
- 健康状态。

公开能力控制在类似以下范围：

```rust
chat(...)
web_search(...)
web_search_stream(...)
upstream_status(...)
```

不要添加以下业务方法：

```text
generate_title
parse_todo
translate_rss
compact_memory
```

这些业务流程继续留在 core，通过统一的 `chat` 入口完成。

### 11. 切换 core 调用链

将普通聊天、标题、Todo、记忆、Compact、翻译和 `/查` 切换到新的 `qq-maid-llm`。

必须保持：

- Prompt 组装位置和顺序不变；
- 模型选择和 fallback 顺序不变；
- 用户侧回复内容和格式不变；
- 健康检查和 `/ping` 展示所依赖的数据不变；
- 现有 metadata 兼容契约可以暂时保留，不要求本次全部类型化；
- mock Provider 和业务测试仍可方便注入。

### 12. 删除旧实现

完成切换并确认测试通过后：

- 删除 core 中已迁移的 Provider 实现；
- 删除重复的 SSE 和 LLM metrics 实现；
- 删除 `/查` 中已迁移的模型协议代码；
- 从 `Cargo.toml` 中移除 `rig-core`；
- 确认 `Cargo.lock` 不再包含 `rig-core`；
- 全仓搜索确认没有 `rig_core`、`rig-core` 或旧 Provider 残留引用。

## 不在本次范围

本次不要实施：

- 引入 `genai` 或其他新的 LLM SDK；
- tool call 通用抽象；
- reasoning 事件；
- 通用 `LlmStreamEvent` 总线；
- 全仓错误体系重构；
- Todo、记忆、翻译等业务 Flow 重写；
- Prompt 系统重构；
- 独立 LLM 微服务或额外运行进程；
- 与本任务无关的格式调整和代码重构。

如迁移过程中发现这些问题，只在完成报告中记录，不要擅自扩大本次修改范围。

## 建议排查范围

请先检查以下区域，路径以仓库实际结构为准：

- workspace 根目录 `Cargo.toml`；
- `qq-maid-llm/src/provider/`；
- OpenAI Responses、Chat Completions、DeepSeek 实现；
- 模型路由和 fallback；
- LLM metrics 和健康状态；
- 通用 SSE 工具；
- `/查` 对应的 query/search flow；
- 普通聊天、标题、Todo、记忆、Compact 和翻译调用点；
- core 配置加载与 Provider 构建入口；
- `rig-core` 的全部依赖和引用。

要求：

1. 先阅读仓库根目录及相关目录中的 `AGENTS.md`、`README.md` 和项目约定。
2. 使用搜索确认实际调用链，不要只根据本文中的文件名猜测。
3. 以仓库当前实现为准。
4. 优先迁移和复用已有测试，不要重新实现已经稳定的逻辑。
5. 若本文描述与仓库现状不一致，在不偏离总体目标的前提下按仓库事实调整，并在完成报告中说明。

## 禁止事项

- 不要进行与 LLM 拆分无关的大规模重构。
- 不要更改公开命令、环境变量或用户侧行为。
- 不要通过吞掉错误或返回空字符串伪造成功。
- 不要删除现有 fallback 和兼容逻辑。
- 不要同时保留两套正式 Provider 实现。
- 不要让 core 继续直接依赖 `rig-core` 或新的第三方 LLM SDK。
- 不要伪造测试、构建或运行结果。

## 验收标准

1. workspace 中存在独立的 `qq-maid-llm` library crate。
2. `qq-maid-llm` 不依赖 `qq-maid-core`。
3. core 通过新 crate 的公开接口调用模型。
4. OpenAI Responses 行为和现有测试无回归。
5. OpenAI Chat Completions 已改为 `reqwest` 实现。
6. DeepSeek 已复用 OpenAI 兼容 Chat Completions adapter。
7. 聊天和 Web Search 共用同一套 SSE frame 解析。
8. `/查` 仍支持流式增量、最终答案和来源列表。
9. 普通聊天、标题、Todo、记忆、Compact 和翻译仍可使用各自模型路由。
10. 模型候选链顺序、永久错误终止和临时错误降级语义不变。
11. usage、cached tokens、fallback 和健康状态无明显回归。
12. core 中不再保留已迁移的 Provider 协议实现。
13. `rig-core` 已从 `Cargo.toml`、`Cargo.lock` 和源码中完全移除。
14. 没有引入循环依赖或新的独立服务。
15. 相关测试、构建、格式化和静态检查通过。

## 测试要求

迁移并运行现有测试，同时为新的 Chat Completions `reqwest` 实现补充最小必要测试：

- 正常流式响应；
- 正常非流式响应；
- 空流补非流；
- 200 但空回复；
- usage 和 cached token 提取；
- 自定义 endpoint；
- 401/403；
- 429；
- timeout；
- 5xx；
- 非标准错误正文；
- DeepSeek 兼容调用；
- 候选链自动降级；
- `/查` 流式文本和来源提取。

运行项目已有的：

- 单元测试；
- 集成测试；
- `cargo fmt --check`；
- `cargo clippy`；
- workspace 构建或检查。

如受真实 API、网络或环境变量限制无法执行某些验证，必须说明未执行的项目和原因，不得声称已经通过。

## 完成后输出

完成后请提供：

1. 原调用链和新调用链概述。
2. 最终 crate 职责边界。
3. 修改和删除了哪些文件。
4. OpenAI Chat Completions 与 DeepSeek 如何替换 `rig-core`。
5. SSE 重复实现如何合并。
6. `/查` 的模型能力和业务 Flow 如何分离。
7. core 中哪些业务调用已完成切换。
8. 执行了哪些测试、构建、格式化和静态检查。
9. 每项检查的实际结果。
10. 全仓搜索 `rig_core` / `rig-core` 的结果。
11. 是否存在尚未解决的风险或后续建议。
