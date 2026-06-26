# 任务：可配置上下文与请求修整层 V1（精简版）

## 状态

Planned

本文档基于 2026-06-25 `master` 分支代码现状重写，对应原始设计稿《可配置上下文与请求修整层 V1》。
落地范围以现有代码边界为准，不臆造尚未存在的供应商实现。

> 说明：本任务文件落在 `master` 分支的 `tasks/` 下，但实际实现工作在 `feat/context-modules`
> 分支上继续推进，因为该分支已落地 `context_modules.rs`，是本任务的代码基线。

## 0. 范围说明

本版为原设计稿的精简版（约 35% 范围），只保留有真实收益的部分：

* 把稳定 Prompt 真正放到前缀，让 Prompt Cache 能命中；
* 把请求时间从头部移到动态区域，避免破坏缓存前缀；
* 扩展 `TokenUsage` 采集 `cached_tokens`，拿到真实缓存命中指标；
* 给未来 Provider 留一个正确的扩展点（patcher 在具体 Provider 调用前生效）；
* 保留一个最小化的历史清洗函数，只处理已确认出现过的错误格式。

明确暂缓（不在 V1 做）：

* Anthropic / Gemini 空占位 patcher 文件（仓库尚无对应 provider，留空文件无收益）；
* `ContextSensitivity`（public/private/sensitive/system_only）分类；
* `ContextSource` 全量配置化（file/builtin/memory/session/runtime）；
* 字符数除以 3 的伪精确 `estimated_tokens` 统计（日志只用 `char_count`，不伪装成 token）；
* 广泛的 reasoning 正则清洗（只处理已确认出现过的 `` / `<reasoning>` 文本块）；
* `LLM_PROMPT_CACHE_ENABLED` 空开关（V1 无供应商需要该开关，加了也是空逻辑）；
* 六七个 patcher 文件的完整目录体系（V1 只需一个模块文件 + trait + 两个实现）。

## 1. 现状评估

### 1.1 已实现

* `qq-maid-core/src/runtime/prompt/context_modules.rs`：可配置上下文模块加载，支持
  `always` 常驻模块、`keywords` 动态命中、`priority` 排序、`max_dynamic_modules` /
  `max_total_chars` 预算、路径逃逸校验、debug 日志。
* `PromptConfig::load_chat_system_prompts(user_text)` 已在普通聊天链路接入上下文模块，
  顺序为：固定 prompt → WORLD_FILE → 上下文模块 → 成员编号映射。
* `runtime/respond/llm_service.rs::build_chat_messages` 负责组装：
  system_prompts → memory_context → session_context → history → user_text，
  并由 `with_request_time_context` 在**头部**注入请求时间上下文。
* `provider/mod.rs::ModelRouteProvider` 已实现模型候选链与跨候选降级，
  每个候选执行 `candidate_req = req.clone()` 后设置各自 model。
* `provider/openai/`、`provider/deepseek/` 已实现各自请求构造与流式/非流式兼容。

### 1.2 尚未实现（本任务目标）

1. 上下文内容没有稳定性分层，请求时间被放在最前面，破坏 Prompt Cache 前缀；
2. `TokenUsage` 没有 `cached_tokens`，无法观测缓存命中；
3. 没有 Provider 请求修整点，provider 专属逻辑只能散落在 `provider/openai/`、`provider/deepseek/` 内部；
4. 历史消息中偶发的 `` / `<reasoning>` 文本块没有统一清理入口；
5. 没有结构化的上下文注入日志。

### 1.3 与现有任务的关系

* `tasks/configurable-context-modules.md`：已落地，本任务在其基础上扩展稳定性分层，不推翻其
  `always` / `keywords` / `priority` / 预算语义。
* `tasks/llm-rag-v1.md`：独立任务，本任务不引入向量检索或知识库。

## 2. 目标

V1 在不改变现有业务行为的前提下：

1. 引入轻量 `ContextEntry`，在消息拍扁前保留 `id` / `source` / `cache_policy` / `content`；
2. 构建消息时按 **Stable → Session → Dynamic → History → User** 排列，
   把请求时间从头部移到 Dynamic 区域；
3. 扩展 `TokenUsage`，采集并记录 `cached_tokens`（来自上游 `input_tokens_details.cached_tokens`
   或等价字段）；
4. 增加一条结构化日志，记录各类上下文 `char_count`、上游返回的 input/output/cached tokens；
5. 保留一个简单的历史清洗函数，只处理已确认出现过的 `` / `<reasoning>` 文本块；
6. 引入 `ProviderRequestPatcher` trait，Provider 专属 patch 在**具体 Provider 调用前**生效，
   不只按主候选执行；
7. 保持现有 `ModelRoute` 候选链、自动降级、流式/非流式行为不变；
8. 未启用新配置时，行为与当前版本完全一致。

## 3. 非目标

V1 不做：

* 不引入 Anthropic / Gemini provider 实现，不预留空占位 patcher；
* 不引入 `ContextSensitivity`、`ContextSource` 全量配置化；
* 不做字符数除以 3 的伪精确 token 估算；
* 不做广泛的 reasoning 正则清洗（只处理 `` / `<reasoning>` 文本块）；
* 不重写长期记忆系统、不引入向量数据库、不实现 RAG；
* 不修改 QQ Gateway、OneBot 11 接口、`/v1/respond` schema；
* 不修改 Todo / RSS / 天气 / 翻译 / 查询命令业务；
* 不改变 `ModelRouteProvider` 的候选链与降级决策；
* 不把历史清洗扩展到当前轮 assistant 输出（只清理历史回放）。

## 4. 上下文稳定性模型

### 4.1 三类策略

```rust
pub enum CachePolicy {
    /// 长期稳定，适合作为缓存前缀。典型：maid_system、mode_rules、world、member_mapping。
    Stable,
    /// 会话内相对稳定。典型：session_summary、当前会话已确认成员身份。
    Session,
    /// 每轮都可能变化。典型：请求时间、最近历史消息、本轮 user_text、本轮召回记忆、动态命中模块。
    Dynamic,
}
```

### 4.2 当前内容映射与排列

V1 消息排列顺序（**Stable → Session → Dynamic → History → User**）：

| 排列区 | 当前来源 | cache_policy | 说明 |
| --- | --- | --- | --- |
| Stable | `PROMPT_DIR` 三固定文件 | Stable | 跨请求不变 |
| Stable | `WORLD_FILE` | Stable | 跨请求不变 |
| Stable | `context_modules` 中 `always=true` | Stable | 常驻 |
| Stable | `MEMBER_ID_MAPPING_FILE` 提示 | Stable | 文件不变则稳定 |
| Session | 本轮成员身份提示（`build_member_identity_context`） | Session | 会话内稳定 |
| Session | `session_context`（含 summary） | Session | 会话内稳定 |
| Dynamic | `context_modules` 中动态命中 | Dynamic | 每轮按 user_text 重选 |
| Dynamic | `memory_context` | Dynamic | 每轮重新读取最近 12 条 |
| Dynamic | **请求时间上下文** | Dynamic | 每轮变化，从头部移到此处 |
| History | 历史消息 | Dynamic | 每轮增长 |
| User | 本轮 `user_text` | Dynamic | 每轮变化 |

**关键变更：请求时间上下文不再注入到消息列表头部，而是放到 Dynamic 区域。**
现有 `with_request_time_context` 把请求时间放在最前面，会导致每轮请求前缀都变，
Prompt Cache 无法命中稳定前缀。V1 把它移到 Stable / Session 之后。

V1 默认 `cache_policy` 由 `ContextAssembler` 内部硬编码，不要求配置文件声明。

## 5. 数据结构

### 5.1 轻量 `ContextEntry`

新增 `runtime/respond/context_assembler.rs`，定义轻量 `ContextEntry`，
**不扩展 `ContextModule` 本身**（保持 TOML 向后兼容，不新增配置字段）：

```rust
pub enum CachePolicy {
    Stable,
    Session,
    Dynamic,
}

/// 轻量上下文条目，在消息拍扁前保留分类信息。
/// 只用于组装顺序和日志，不参与裁剪决策（裁剪仍由 context_modules.rs 负责）。
pub struct ContextEntry {
    pub id: String,
    pub source: &'static str,        // "prompt_dir" / "world" / "context_module" / "memory" / "session" / "request_time" / "member_mapping" / "member_identity"
    pub cache_policy: CachePolicy,
    pub content: String,
    pub char_count: usize,           // content.chars().count()，仅用于日志，不伪装成 token
}
```

`source` 是硬编码的静态字符串标签，**不做全量配置化**，只用于日志区分来源。

### 5.2 `AssembledContext`

```rust
pub struct AssembledContext {
    pub stable: Vec<ContextEntry>,
    pub session: Vec<ContextEntry>,
    pub dynamic: Vec<ContextEntry>,
    pub history: Vec<ChatMessage>,
    pub user_text: String,
    pub total_chars: usize,          // 各区 char_count 之和，仅用于日志
}
```

`AssembledContext` 不引入 `total_estimated_tokens`，避免伪精确统计。

## 6. ContextAssembler

### 6.1 职责

* 接收 `PromptConfig` 产出的 `system_prompts`、`memory_context`、`session_context`、
  `history_messages`、`user_text`、请求时间上下文；
* 把每段 system prompt 包装成 `ContextEntry`，按 4.2 表格赋予默认 `cache_policy` 和 `source`；
* 按 stable → session → dynamic 分组；
* **把请求时间上下文包装成 Dynamic 的 `ContextEntry`，不再注入头部**；
* 不重复实现模块选择逻辑（仍由 `context_modules.rs` 负责），只做分类包装；
* 输出 `AssembledContext` 供 `LlmChatService` 使用；
* 输出 debug 级别结构化日志（模块 id、source、char_count、cache_policy）。

### 6.2 不负责

* 不加载世界观、不召回长期记忆、不判断当前成员；
* 不执行业务命令、不选择 fallback 模型；
* 不添加供应商专用字段、不修改认证 Header；
* 不做 token 估算、不做裁剪。

### 6.3 落点

* 新增 `qq-maid-core/src/runtime/respond/context_assembler.rs`；
* `LlmChatService::respond` 在 `build_respond_messages` 之后调用 assembler，得到
  `AssembledContext`，再传给 patcher；
* `build_respond_messages` 保持现有签名不变，assembler 在其输出基础上包装。
  请求时间上下文的注入位置由 assembler 接管，`with_request_time_context` 的头部注入
  逻辑在 assembler 启用时不再生效（见 §11 兼容性）。

## 7. ProviderRequestPatcher

### 7.1 接口与落点

新增 `qq-maid-core/src/llm/request_patch.rs`（**单文件**，不建目录体系）：

```rust
pub trait ProviderRequestPatcher: Send + Sync {
    fn patch(
        &self,
        request: &ChatRequest,
        context: &AssembledContext,
    ) -> Result<PatchedLlmRequest>;
}

pub struct PatchReport {
    pub provider: String,
    pub stripped_reasoning_blocks: usize,
    pub removed_empty_messages: usize,
    pub warnings: Vec<String>,
}

pub struct PatchedLlmRequest {
    pub request: ChatRequest,
    pub report: PatchReport,
}
```

V1 只实现两个 patcher：`GenericPatcher`、`OpenAiPatcher`（DeepSeek 复用 OpenAI 兼容逻辑）。
**不实现 Anthropic / Gemini 占位。**

### 7.2 patcher 在具体 Provider 调用前生效

**关键约束：每次候选切换都基于统一请求重新生成 provider 请求，不复用上一个供应商已修整过的请求体。**

patcher 下沉到 `ModelRouteProvider::chat` 的候选循环内，按当前候选 `ModelProvider` 选择 patcher：

```rust
// provider/mod.rs::ModelRouteProvider::chat 候选循环内
for (index, candidate) in route.candidates().iter().enumerate() {
    let provider = provider_for(candidate, &self.providers)?;
    let patcher = patcher_for(candidate.provider);
    let patched = patcher.patch(&req, &context)?;
    let mut candidate_req = patched.request;
    candidate_req.model = Some(candidate.to_request_model());
    match provider.chat(candidate_req).await { ... }
}
```

这样每个候选都用对应 patcher 修整，降级到 DeepSeek 时不会复用 OpenAI 的 patched request。
`patcher_for` 返回 `Box<dyn ProviderRequestPatcher>`：

```rust
fn patcher_for(provider: Option<ModelProvider>) -> Box<dyn ProviderRequestPatcher> {
    match provider {
        Some(ModelProvider::OpenAi) | Some(ModelProvider::DeepSeek) | None => Box::new(OpenAiPatcher::new()),
    }
}
```

> 落地注意：`ModelRouteProvider::chat` 当前签名只接收 `ChatRequest`，不接收 `AssembledContext`。
> V1 需要扩展 `LlmChatService` 把 `AssembledContext` 透传到 `ModelRouteProvider::chat`，
> 或在 `ChatRequest.metadata` 中携带 `AssembledContext` 的序列化句柄。
> 优先方案：给 `ModelRouteProvider::chat` 增加一个 `context: &AssembledContext` 参数，
> 这是 V1 唯一需要改动 `ModelRouteProvider` 接口的地方，且只新增参数、不改降级决策。

### 7.3 各 patcher 职责

#### GenericPatcher

* 调用 `strip_reasoning_from_history` 清理历史消息中的 `` / `<reasoning>` 文本块；
* 处理清理后的空消息（见 §8.3）；
* 不插入任何供应商专有字段。

#### OpenAiPatcher（DeepSeek 共用）

* 继承 GenericPatcher 清理；
* V1 不主动设置缓存断点（OpenAI 兼容供应商缓存由前缀自动命中，无需显式断点）；
* 稳定前缀由 `ContextAssembler` 的 Stable → Session → Dynamic 排列保证。

## 8. 历史清洗

### 8.1 清理范围（只处理已确认出现过的格式）

`request_patch.rs` 提供共享函数：

```rust
pub fn strip_reasoning_from_history(messages: &mut Vec<ChatMessage>) -> ReasoningStripReport;
```

只清理历史消息（非本轮 user_text）`content` 内嵌的：

* `` 文本块；
* `<reasoning>...</reasoning>` 文本块。

**不做广泛正则清洗**：不处理 `reasoning_content:` 前缀段、不处理 `reasoning_details` /
`redacted_thinking` 字段（当前 `ChatMessage` 只有 `content: String`，这些字段若被上游返回
会在 `extract` 阶段被丢弃，不会进入 history）。后续若确认出现新格式，再增量添加清理规则。

### 8.2 不清理

* 普通 assistant 文本正文；
* 用户主动发送的普通文本；
* 本轮 user_text。

### 8.3 空消息处理

清理后若某条历史 assistant 消息 `content.trim().is_empty()`：

* 删除该消息；
* 删除后检查角色顺序合法性（不允许连续 user-user 或以 assistant 开头）；
* 若删除导致首条消息为 assistant，则一并删除；
* 不允许留下空 content 导致供应商拒绝请求。

`ReasoningStripReport` 记录清理数量和删除空消息数量，供 `PatchReport` 使用。

## 9. TokenUsage 扩展

### 9.1 新增 `cached_tokens`

扩展 `provider/types.rs::TokenUsage`：

```rust
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// 缓存命中的输入 token 数（来自上游 input_tokens_details.cached_tokens 或等价字段）。
    /// None 表示上游未返回该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
}
```

### 9.2 采集点

* `provider/openai/extract.rs::extract_response_usage`：读取
  `usage.input_tokens_details.cached_tokens`（OpenAI Responses API 字段）；
* `provider/openai/chat.rs::token_usage`：rig-core `Usage` 若暴露 cached 字段则读取，
  否则保持 `None`；
* DeepSeek 复用 OpenAI 兼容路径，字段缺失时 `None`。

**不伪造缓存指标**：上游不返回 `cached_tokens` 时记 `None`，不填 0。

## 10. 可观测性

### 10.1 日志事件

每轮 LLM 请求在 `LlmChatService::respond` 中输出一条 debug 级别结构化日志：

```json
{
  "event": "llm_context_assembled",
  "request_id": "<session_id>",
  "provider": "openai",
  "model": "gpt-5.5",
  "stable_chars": 2350,
  "session_chars": 180,
  "dynamic_chars": 420,
  "history_messages": 6,
  "total_chars": 2950,
  "input_tokens": 1200,
  "output_tokens": 80,
  "cached_tokens": 1100
}
```

`cached_tokens` 来自上游 `TokenUsage`，可能为 `null`。`*_chars` 来自 `AssembledContext`，
**不伪装成 token**。

### 10.2 脱敏要求

日志中不得包含：

* API Key、Authorization Header、AppSecret、token；
* 完整 prompt 正文、完整长期记忆原文、完整世界观；
* 用户敏感内容、openid、群 ID。

日志可包含：

* 各区 char_count、history_messages 数量、total_chars；
* provider 名称、模型名称；
* input_tokens、output_tokens、cached_tokens；
* reasoning 清理数量、删除空消息数量。

复用现有 `redact_sensitive_text`（`runtime/session.rs`）对任何可能进入日志的文本做脱敏。

## 11. 配置与开关

### 11.1 新增环境变量

```env
# 请求修整层总开关，缺省 1（启用）。关闭后 LlmChatService 直接走旧路径，不调用 assembler 和 patcher。
LLM_REQUEST_PATCH_ENABLED=1
```

**不引入 `LLM_PROMPT_CACHE_ENABLED`**：V1 无供应商需要该开关，加了也是空逻辑。
后续 Anthropic provider 落地、需要显式缓存断点时再引入。

### 11.2 兼容性

* `LLM_REQUEST_PATCH_ENABLED=0` 时，`LlmChatService::respond` 跳过 assembler 和 patcher，
  直接走 `build_respond_messages` + `with_request_time_context`（请求时间仍注入头部）+
  `provider.chat`，行为与当前版本一致；
* `LLM_REQUEST_PATCH_ENABLED=1` 时，请求时间由 assembler 放到 Dynamic 区域，
  `with_request_time_context` 的头部注入不再生效；
* `CONTEXT_MODULES_FILE` 未配置时，`ContextAssembler` 仍可工作（上下文模块为空）；
* 现有 `PROMPT_DIR`、`WORLD_FILE`、`MEMBER_ID_MAPPING_FILE` 不改名、不迁移；
* 现有 `context_modules.toml` 不要求声明任何新字段（V1 不扩展 TOML）。

## 12. 与现有 ModelRoute 的关系

* `ModelRouteProvider` 的候选链与降级决策完全不变；
* V1 只给 `ModelRouteProvider::chat` 新增一个 `context: &AssembledContext` 参数，
  用于在候选循环内按候选 provider 选择 patcher；
* patcher 不决定候选切换、不持有 fallback 逻辑；
* 每个候选独立 patch，不复用上一个候选的 patched request。

## 13. 实施阶段

### 阶段一：上下文统一结构与排列

* 新增 `runtime/respond/context_assembler.rs`，实现 `ContextEntry` / `AssembledContext` / `ContextAssembler`；
* 把请求时间上下文从头部注入改为 Dynamic 区域；
* `LlmChatService::respond` 在 `LLM_REQUEST_PATCH_ENABLED=1` 时调用 assembler；
* 单测：分类正确性、排列顺序（Stable → Session → Dynamic → History → User）、
  请求时间位置、空模块处理。

### 阶段二：TokenUsage 扩展

* 扩展 `TokenUsage` 增加 `cached_tokens`；
* `extract_response_usage` 读取 `input_tokens_details.cached_tokens`；
* 单测：字段缺失时返回 `None`、字段存在时正确读取。

### 阶段三：请求修整层

* 新增 `src/llm/request_patch.rs`（单文件），定义 trait 与 `GenericPatcher` / `OpenAiPatcher`；
* 实现 `strip_reasoning_from_history` 与空消息处理；
* 给 `ModelRouteProvider::chat` 增加 `context: &AssembledContext` 参数，在候选循环内按候选 patch；
* 单测：`` / `<reasoning>` 块清理、空消息删除、角色顺序合法性、
  provider 切换后请求独立（不复用 patched request）。

### 阶段四：可观测性与文档

* 在 `LlmChatService::respond` 输出 `llm_context_assembled` 结构化日志；
* 验证日志脱敏；
* 更新 `runtime/.env.example` 增加 `LLM_REQUEST_PATCH_ENABLED`；
* 更新 `qq-maid-core/README.md`、`AGENTS.md` 记录新模块边界。

## 14. 测试要求

### 14.1 单元测试

* `context_assembler`：stable / session / dynamic 分类、排列顺序、
  请求时间在 Dynamic 区域、空模块；
* `TokenUsage`：`cached_tokens` 字段缺失返回 `None`、存在时正确读取；
* `strip_reasoning_from_history`：
  * `` 块清理；
  * `<reasoning>` 块清理；
  * 清理后空 assistant 消息删除；
  * 删除后角色顺序合法性（首条非 assistant、无连续 user）；
  * 不误删普通 assistant 文本、不清理本轮 user_text；
* `GenericPatcher`：不插入任何供应商字段；
* `OpenAiPatcher`：不设置缓存断点；
* `LlmChatService`：`LLM_REQUEST_PATCH_ENABLED=0` 时走旧路径（请求时间在头部）、
  `=1` 时走 assembler（请求时间在 Dynamic）；
* `ModelRouteProvider`：候选切换时各候选独立 patch，不复用 patched request。

### 14.2 集成测试

* 普通聊天请求经 patcher 后回复内容与旧路径一致（回归）；
* 历史含 `` 块时请求仍成功；
* 主模型失败后候选降级仍正常（patcher 不破坏 `ModelRouteProvider`）；
* 流式与非流式均正常。

### 14.3 回归测试

确认以下功能无变化：

* 普通聊天、成员编号识别、长期记忆注入、session context；
* Todo、RSS、天气、翻译、Markdown 输出；
* OneBot 11、QQ Gateway；
* `context_modules` 现有 `always` / `keywords` / 预算语义。

## 15. 验收标准

V1 完成需满足：

1. 消息排列为 Stable → Session → Dynamic → History → User，请求时间在 Dynamic 区域；
2. `ContextAssembler` 输出 stable / session / dynamic 三组；
3. `TokenUsage` 包含 `cached_tokens`，上游返回时正确采集，不返回时为 `None`；
4. `ProviderRequestPatcher` 在 `ModelRouteProvider` 候选循环内按候选 provider 生效，
   不只按主候选执行；
5. 历史 `` / `<reasoning>` 块被清理，清理后无空消息；
6. 每轮请求可查看各区 char_count 与真实 input/output/cached tokens；
7. 日志不泄露 prompt、记忆和密钥原文；
8. `LLM_REQUEST_PATCH_ENABLED=0` 时行为与当前版本一致（请求时间在头部）；
9. 现有自动降级和流式响应测试通过；
10. `context_modules.toml` 旧格式（无任何新字段）仍可加载。

## 16. 常用验证

```bash
# 1. 格式化检查
cargo fmt --all -- --check

# 2. Clippy
cargo clippy --workspace --all-targets --all-features -- -D warnings

# 3. 全 workspace 测试
cargo test --workspace --all-features

# 4. release 构建
cargo build --workspace --release --all-features
```

最低要求：提交前至少跑完 1–3 步；改动涉及启动、配置、依赖或发布时再跑第 4 步。

## 17. 后续规划

V1 完成后可在独立版本继续扩展：

### V2

* 把 patcher 下沉到 `ModelRouteProvider` 内部按候选分别执行（V1 已做）→ 后续优化
  patcher 与候选降级的错误处理协同；
* stable / session / dynamic 分层预算；
* 记忆来源标记、置信度。

### V3

* Anthropic Prompt Cache 断点落地（待 Anthropic provider 实现）；
* Prompt Cache 命中率统计与告警；
* 上下文调试命令。

## 18. 最终原则

该模块只负责：

```text
什么内容进入上下文
这些内容以什么顺序进入（稳定内容在前，动态内容在后）
哪些内容可以稳定缓存
发送给不同供应商前需要怎样修整
```

不负责：

```text
业务命令
模型降级决策
角色剧情
记忆内容生成
网关协议
消息发送
```

通过保持这些边界，避免上下文系统、供应商兼容层和聊天业务继续相互耦合。
