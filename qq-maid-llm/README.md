# qq-maid-llm — Rust LLM 基础设施 crate

`qq-maid-llm/` 是小女仆机器人的 LLM 基础设施层，负责模型调用协议、Provider 路由、fallback、SSE、usage、健康观测、OpenAI Web Search 协议和模型原生 Tool Loop 协议。本 crate 不依赖 `qq-maid-core`，也不承载任何业务 flow（prompt、session、memory、todo、RSS 翻译、具体 Tool 执行等仍由 core 维护）。

依赖方向固定为：

```text
qq-maid-gateway-rs
        ↓
qq-maid-core
        ↓
qq-maid-llm
        ↓
qq-maid-common / reqwest / serde / tokio
```

禁止 `qq-maid-llm` 反向依赖 `qq-maid-core`，也不要让 core 绕过本 crate 直接维护 Provider 协议实现。

## 当前范围

- OpenAI Responses API 主链路（system/user 用 `input_text`、assistant 用 `output_text`、completed-only 提取、delta/completed/failed/incomplete/error 事件、跨 chunk 与 CRLF 兼容、空流补非流、流式失败后补非流、usage 与 cached token 提取、HTTP 错误正文裁剪、200 但正文为空返回明确错误）。
- OpenAI Chat Completions 兼容实现（基于 `reqwest`，支持流式与非流式、`[DONE]`、usage 与 cached token 提取、空流补非流、401/403/429/timeout/5xx 与非标准错误正文分类）。
- DeepSeek 复用 OpenAI 兼容 Chat Completions adapter，只保留 base URL、认证和模型规则差异。
- 模型候选链路由：按候选顺序调用、成功立即停止、临时错误降级、永久错误终止、全部失败返回聚合错误；OpenAI Responses → Chat Completions fallback 规则不变；`LLM_PROVIDER=auto` 兼容逻辑不变。
- 通用 SSE frame 解析（聊天 Responses 与 Web Search 共用，处理 CRLF、`event:`/`data:`、`[DONE]`）。
- OpenAI Responses + `web_search` 工具协议：请求 payload、HTTP transport、SSE 文本增量、answer 提取、sources 提取。
- Function Tool 基础抽象：`Tool` trait、`ToolRegistry`、工具超时、工具结果大小限制和服务端白名单执行入口。
- OpenAI Responses 原生 Function Tool Loop：解析 `function_call`，执行注册 Tool 后以 `function_call_output` 回传模型；当前只支持串行调用，最大轮数由 core 配置控制。
- Token usage、`LlmMetrics`、`MetricsRecorder` 和 `UpstreamStatus` / `ObservedProvider` 健康观测。
- 仅与模型调用有关的 `LlmError`（Provider 配置、网络/超时、HTTP 上游、SSE/协议、Tool Loop 协议、空回复、候选模型全部失败）。

不在本 crate 范围：

- 标题生成、Todo 解析、记忆压缩、RSS 翻译等业务 flow —— 这些继续留在 core，通过统一 `chat` 入口完成。
- `/查`、`/查询`、`/search` 命令解析、权限判断、回复排版、session 记录 —— 由 core 维护。
- 全仓 `AppError` 体系、天气、火车、RSS、HTTP 路由等非 LLM 错误 —— 由 core 维护。
- 具体 Tool 实现和业务权限 —— 由 core 维护；本 crate 只负责 Tool 协议和执行调度抽象。
- Hosted Code Interpreter、File Search、文件输入/输出、reasoning 事件或完整 LLM 事件总线 —— 当前不实现。

## 统一入口

`LlmService` 是本 crate 的唯一对外服务入口，职责仅限于：创建和复用 HTTP client、Provider 组装、模型候选链执行、fallback、Web Search 和健康状态。Tool Loop 通过 provider-facing `chat_with_tools` 能力开放给 core，具体 Tool 注册和业务执行由 core 完成。

```rust
pub struct LlmService { /* ... */ }

impl LlmService {
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError>;
    pub async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError>;
    pub async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError>;
    pub async fn web_search(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError>;
    pub async fn web_search_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError>;
    pub fn upstream_status(&self) -> UpstreamStatus;
}
```

不要在本 crate 添加 `generate_title`、`parse_todo`、`translate_rss`、`compact_memory`、`get_weather` 等业务方法；这些流程继续留在 core，通过 `chat` 或 provider-facing Tool Loop 入口完成。

## 模块结构

```text
qq-maid-llm/src/
├── lib.rs            # 对外导出 LlmService、LlmError、ErrorInfo
├── config.rs         # LlmConfig、ProviderMode、OpenAiApiMode 等 Provider 基础配置
├── error.rs          # LlmError、ErrorInfo（仅模型调用相关错误）
├── metrics.rs        # LlmMetrics、MetricsRecorder、duration_ms
├── service.rs        # LlmService 统一入口
├── sse.rs            # 通用 SSE frame 解析（聊天与 Web Search 共用）
├── tool.rs           # Tool trait、ToolRegistry、工具超时和输出大小限制
├── web_search.rs     # OpenAI Responses + web_search 协议、WebSearchRequest/Outcome/Source
└── provider/
    ├── mod.rs        # LlmProvider trait、DynLlmProvider、ChatOutcome、ToolChatRequest、候选链路由
    ├── types.rs      # ChatMessage、ChatRole、ChatRequest、ModelId、ModelRoute、ModelProvider、TokenUsage
    ├── status.rs     # UpstreamStatus、ObservedProvider 健康观测
        ├── bigmodel.rs   # 智谱 BigModel（复用 OpenAI 兼容 Chat Completions adapter）
        ├── deepseek.rs   # DeepSeek（复用 OpenAI 兼容 Chat Completions adapter）
        ├── openai_compatible.rs # 配置驱动的 OpenAI-compatible Chat Completions provider
    └── openai/
        ├── mod.rs        # OpenAI provider 组装与 LlmProvider 实现
        ├── responses.rs  # Responses API 主链路
        ├── chat.rs       # Chat Completions 兼容实现（reqwest）
        ├── stream.rs     # 流式解析
        ├── extract.rs    # usage / cached token 提取
        ├── fallback.rs   # Responses → Chat Completions fallback
        ├── payload.rs    # 请求 payload 构造
        ├── tool_loop.rs   # Responses function_call / function_call_output Tool Loop
        └── transport.rs   # HTTP transport
```

## 配置边界

`qq-maid-llm` 的配置只包含 Provider 基础配置，由 core 从环境变量解析后通过 `LlmConfig` 传入：

- `provider`：`openai` / `deepseek` / `bigmodel` / `auto`。
- `model_route`：主模型候选链。
- `configured_model_routes`：`TITLE_MODEL`、`TODO_MODEL`、`MEMORY_MODEL`、`COMPACT_MODEL`、`TRANSLATION_MODEL` 等业务模型候选链（由 core 管理，通过 `ChatRequest.model` 传入）。
- `openai_api_key`、`openai_base_url`、`openai_api_mode`（`auto` 优先 Responses 并在可恢复错误时降级 Chat Completions；`chat_only` 仅用于只实现 Chat Completions 的网关）。
- `deepseek_api_key`、`deepseek_base_url`、`deepseek_model`。
- `bigmodel_api_key`、`bigmodel_base_url`、`bigmodel_model`。
- `openai_compatible_providers`：由 `agent.toml [providers.*]` 声明的自定义 Chat Completions provider，例如 `mimo`；实际 API key 由 core 按 `api_key_env` 从环境变量读取。
- `request_timeout`、`stream`、`max_output_tokens`。
- `search_model`：`/查` 使用的 OpenAI Web Search 模型。

`TOOL_CALLING_ENABLED` 和 `TOOL_CALLING_MAX_ROUNDS` 由 core 解析并决定是否进入 Tool Loop，不作为 `LlmConfig` 的 Provider 基础配置字段；本 crate 只接收已经构造好的 `ToolChatRequest`。

Todo、标题、记忆、Compact、翻译等业务模型配置继续由 core 管理。现有环境变量名称和默认语义保持不变，完整字段以 [runtime/config/.env.example](../runtime/config/.env.example) 为准。

## 调用链

```text
qq-maid-core CoreService
  -> LlmService::chat(ChatRequest)
     -> 候选链路由（按候选顺序）
        -> OpenAI provider（Responses API → Chat Completions fallback）
        -> DeepSeek provider（OpenAI 兼容 Chat Completions adapter）
        -> BigModel provider（OpenAI 兼容 Chat Completions adapter）
        -> 自定义 OpenAI-compatible provider（如 MiMo，Chat Completions adapter）
     -> 成功立即停止；临时错误降级；永久错误终止；全部失败返回聚合错误
  -> ChatOutcome { reply, metrics, usage, fallback_used }

qq-maid-core 私聊普通聊天（Tool Calling 开启时）
  -> provider.chat_with_tools(ToolChatRequest)
     -> ModelRouteProvider 固定首个候选和 Provider
     -> OpenAI Responses function_call
     -> qq-maid-core 注册的 Function Tool 执行
     -> function_call_output 回传模型
     -> ChatOutcome { reply, metrics, usage, fallback_used: false }

qq-maid-core /查
  -> LlmService::web_search(WebSearchRequest)
     -> OpenAI Responses + web_search 工具
     -> WebSearchOutcome { answer, sources, ... }
```

## 运行和检查

从仓库根目录执行：

```bash
make test-llm    # 依次执行 common/llm 的 fmt check、测试和 cargo check
```

修改 Provider 协议、SSE 解析或模型候选链后，需要跑 `qq-maid-llm` 的单测，并确认 core 侧调用链无回归：

```bash
make test-core   # 同时检查 qq-maid-common 和 qq-maid-llm
make test        # 全 workspace
```

完整 CI 四步见仓库根 [AGENTS.md](../AGENTS.md) 的“测试与检查”。

## 开发边界

- 模型协议、Provider、路由、fallback、SSE、usage、健康观测和 Web Search 协议优先在本 crate 演进。
- 不要在 `qq-maid-core` 中重新实现已迁入本 crate 的 Provider 协议、SSE frame 解析、模型候选链或健康观测逻辑；需要这些能力时直接复用 `LlmService` 的公开入口。
- 不要引入 `genai`、`rig-core` 或其他新的第三方 LLM SDK；OpenAI Chat Completions 和 DeepSeek 均基于 `reqwest` 自研实现。
- 不要同时保留两套正式 Provider 实现。
- 不要通过吞掉错误或返回空字符串伪造成功；200 但空回复必须返回明确错误。
- 不要更改公开环境变量或用户侧行为。
- 新增或修改非显而易见的逻辑时，需要添加必要的中文注释，优先说明"为什么这样做"和"需要保持什么约束"。
