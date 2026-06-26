# 任务：可配置上下文模块（Prompt 扩展层）V1

## 状态

Planned

本文档基于 2026-06-25 当前仓库实现重写，用于替换旧版较抽象、与现状边界不完全贴合的草稿。

## 0. 评估结论

先说明当前 `qq-maid-core` 普通聊天链路里，真正会进入 LLM system message 层的内容来源：

1. `PROMPT_DIR` 下固定三份 prompt：
   * `maid_system.md`
   * `mode_rules.md`
   * `session_context.md`
2. `WORLD_FILE` 指向的可选世界观文件；
3. `MEMBER_ID_MAPPING_FILE` 生成的成员编号映射提示；
4. `llm_service.rs` 统一注入的请求时间上下文。

另外，当前聊天链路还会在 system message 层追加：

* 长期记忆上下文 `memory_context`；
* 会话上下文 `session_context`；
* 命中成员编号后的本轮身份上下文；
* 最近若干轮历史消息。

这些内容并不都属于“prompt 文件加载器”，也不都适合纳入同一种“上下文模块”机制。

旧版草稿的主要问题：

* 把“固定 prompt”“世界观文件”“成员映射提示”“session / memory / time 上下文”混成一类描述，容易误导实现范围；
* 没有明确首版只改普通聊天链路，容易让人误以为 `/todo`、`/memory` 草稿、`/compact` 也要一起接入；
* 目录和配置示例没有说明与 `PROMPT_DIR`、`WORLD_FILE`、`MEMBER_ID_MAPPING_FILE` 的关系；
* 与 `tasks/llm-rag-v1.md` 的边界不清，容易把“可配置 prompt 模块”做成半套知识库。

因此，这里将任务重写为：**在保留当前 prompt 组装边界的前提下，为普通聊天链路增加一层确定性的、配置驱动的可选上下文模块加载能力。**

## 1. 背景

当前普通聊天链路的 prompt 结构已经分成几层：

* 固定行为规则：`PROMPT_DIR` 三个固定文件；
* 可选世界观：`WORLD_FILE`；
* 成员编号规则：`MEMBER_ID_MAPPING_FILE` 生成的系统提示；
* 运行时上下文：时间、长期记忆、会话状态、最近历史消息。

这种结构对小规模配置已经够用，但当部署方希望追加更多“只在某些话题下需要”的补充资料时，当前方案只有两个不理想的选择：

1. 把内容继续塞进 `maid_system.md` / `mode_rules.md` / `session_context.md`；
2. 把大量补充资料合并到单个 `WORLD_FILE`。

这会带来几个问题：

* 无关内容在每次普通聊天都重复注入；
* 单个 prompt 文件越来越长，维护和审查困难；
* 某个领域资料改动时，影响范围过大；
* 未来若要升级到更复杂的本地检索能力，缺少一层独立、清晰的“可选补充上下文”抽象。

因此，需要为普通聊天链路增加一套**确定性、配置驱动、按需加载**的上下文模块机制。

## 2. 目标

V1 目标：

1. 保留当前 `PROMPT_DIR`、`WORLD_FILE`、`MEMBER_ID_MAPPING_FILE` 语义不变；
2. 新增一层“可选上下文模块”，只在普通聊天链路中按需加载；
3. 允许部署方把附加资料拆成多个独立 Markdown 模块维护；
4. 使用简单、确定性的匹配规则决定本轮加载哪些模块；
5. 控制单次加载的模块数量和总字符预算；
6. 保持模块命中结果可观测、可排障；
7. 不修改对外 HTTP 接口，不新增 `/v1/respond` 字段；
8. 未配置模块索引时，运行行为与当前版本保持一致。

## 3. 非目标

V1 明确不做：

* 向量数据库；
* embedding；
* SQLite FTS / 本地全文检索知识库；
* 自动从聊天记录生成模块；
* Web 配置界面；
* 多级模块依赖；
* 概率触发、复杂规则引擎或模型判定选模块；
* `/todo`、`/memory` 草稿、`/compact`、翻译、查询链路的上下文模块接入；
* 替换现有 `WORLD_FILE` 或 `MEMBER_ID_MAPPING_FILE` 语义。

如果需求已经接近“文档问答 / 本地知识库检索”，应看 `tasks/llm-rag-v1.md`，不要把本任务扩展成半套 RAG。

## 4. 作用范围与边界

V1 只影响 `qq-maid-core` 内部普通聊天路径：

```text
handle_chat
  -> PromptConfig 加载固定 prompt / world / 模块 / 成员映射
  -> 组装 memory_context / session_context / history
  -> LlmChatService 注入时间上下文
  -> provider.chat
```

边界要求：

* 改动优先落在 `qq-maid-core/src/runtime/prompt/`；
* `runtime/respond/chat_flow.rs` 只负责把当前用户输入传给 prompt 选择层，不要把模块匹配逻辑散落到聊天 flow；
* `runtime/respond/llm_service.rs` 保持“统一时间上下文注入”和消息组装职责，不要把文件扫描、TOML 解析塞进去；
* gateway、`/v1/respond` schema、pending、todo、memory 持久化都不因本任务改变。

## 5. 当前与目标加载顺序

当前普通聊天消息组装顺序是：

```text
请求时间上下文
→ system_prompts
→ memory_context
→ session_context
→ 历史消息
→ 当前用户消息
```

V1 只调整 `system_prompts` 这一层的内部来源顺序，建议变为：

```text
1. PROMPT_DIR 固定三文件
2. WORLD_FILE（如配置）
3. 命中的上下文模块
4. MEMBER_ID_MAPPING_FILE 生成的成员编号映射提示
```

说明：

* 请求时间上下文仍由 `llm_service.rs` 统一注入，不并入模块机制；
* `memory_context`、`session_context` 仍按现有顺序追加，不与模块合并；
* 本轮命中的成员身份提示仍留在 `session_context` 侧，不挪到模块层。

这样可以保证：

* 固定行为规则仍是最稳定的底层约束；
* 世界观仍保留独立入口；
* 可选模块只补充额外领域信息；
* 成员编号规则仍作为靠后的显式约束出现，不被通用模块轻易覆盖。

## 6. 配置入口与目录建议

建议新增一个可选环境变量：

```env
CONTEXT_MODULES_FILE=
```

语义：

* 留空：关闭该能力，完全保持当前行为；
* 非空：指向模块索引文件。

路径风格应与现有 `PROMPT_DIR`、`WORLD_FILE`、`MEMBER_ID_MAPPING_FILE` 保持一致：

* 支持外部绝对路径；
* 支持按 `runtime/` 工作目录解析的相对路径；
* 不要求把真实模块索引和模块文件放在仓库内。

建议目录结构：

```text
runtime/config/
├── prompts/
│   ├── maid_system.md
│   ├── mode_rules.md
│   └── session_context.md
├── world.md
├── member_id_mapping.json
├── context_modules.example.toml
├── context_modules.toml
└── context/
    ├── deploy.md
    ├── ops.md
    └── domain_x.md
```

说明：

* `PROMPT_DIR`、`WORLD_FILE`、`MEMBER_ID_MAPPING_FILE` 继续按现有方式工作；
* `context_modules.toml` 只是新增索引，不替代现有三种配置；
* 真实模块内容默认仍应视为私有配置，不提交仓库；
* `.example` 文件只提供公开示例，不进入运行时加载。

## 7. 配置格式建议

建议索引文件采用 TOML：

```toml
version = 1

[limits]
max_dynamic_modules = 2
max_total_chars = 6000

[[modules]]
id = "deploy"
file = "context/deploy.md"
always = true
priority = 100

[[modules]]
id = "ops"
file = "context/ops.md"
keywords = ["部署", "上线", "重启", "回滚"]
priority = 80

[[modules]]
id = "domain_x"
file = "context/domain_x.md"
keywords = ["项目X", "domain-x"]
priority = 60
```

字段说明：

* `version`：索引版本，V1 固定为 `1`；
* `limits.max_dynamic_modules`：单次最多加载多少个“动态命中模块”，不含 `always=true` 模块；
* `limits.max_total_chars`：模块层合计字符预算，包含 `always=true` 模块；
* `modules[].id`：模块唯一 ID；
* `modules[].file`：模块文件路径，建议相对 `CONTEXT_MODULES_FILE` 所在目录解析；
* `modules[].always`：是否始终加载；
* `modules[].keywords`：关键词列表，任意命中即可激活；
* `modules[].priority`：排序优先级，值越大越靠前。

V1 先不要加：

* 正则；
* 排除关键词；
* 标签组合；
* 规则表达式；
* include / extends。

## 8. 匹配与选择流程

V1 建议流程：

```text
收到普通聊天用户输入
    ↓
加载固定 prompt / WORLD_FILE
    ↓
读取模块索引
    ↓
加入 always 模块
    ↓
对当前 user_text 做关键词匹配
    ↓
按 priority desc、id asc 排序
    ↓
应用 max_dynamic_modules
    ↓
应用 max_total_chars
    ↓
拼入最终 system_prompts
```

具体规则：

1. 首版仅使用当前轮 `user_text` 做模块匹配；
2. 不读取 session summary、历史消息或 memory 内容做隐式匹配；
3. 同一模块单轮最多加载一次；
4. `always=true` 模块不参与“命中排序”，但计入总字符预算；
5. 如果 `always=true` 模块本身就超过 `max_total_chars`，应视为配置错误；
6. 动态模块按 `priority` 从高到低排序，优先级相同按 `id` 升序打破平局；
7. 超出数量上限的动态模块直接跳过；
8. 超出字符预算时，从低优先级动态模块开始放弃，不截断模块正文；
9. 不自动扫描目录中的所有 Markdown，只有索引中显式声明的文件才允许加载。

## 9. 文本匹配规则

V1 保持简单、可预测：

* 关键词匹配采用“是否包含”语义；
* 英文匹配建议大小写不敏感；
* 中文按原样匹配；
* 空关键词、重复关键词在加载索引时去重或报错；
* 模块正文为空视为配置错误；
* 仅命中无意义短词造成误触发的情况，由部署方通过调整关键词负责控制。

不要在 V1 里引入分词器或模型判定，否则排障成本会明显上升。

## 10. 与现有配置的兼容策略

兼容要求：

1. `CONTEXT_MODULES_FILE` 未配置时，`PromptConfig` 行为与当前版本完全一致；
2. `PROMPT_DIR`、`WORLD_FILE`、`MEMBER_ID_MAPPING_FILE` 不改名、不降级、不迁移；
3. `WORLD_FILE` 与模块索引可以同时存在；
4. 现有持久化数据、session scope、memory / todo 流程不受影响；
5. 不新增对外 API 字段，不要求 gateway 传额外上下文。

也就是说，这个能力是对“普通聊天 prompt 组装层”的增量扩展，不是架构迁移。

## 11. 校验与错误处理

建议校验点：

* `CONTEXT_MODULES_FILE` 指定后，索引文件必须存在且可解析；
* `version` 必须是当前支持值；
* 模块 `id` 必须唯一；
* `file` 必须存在、可读、非空；
* 相对路径不能逃逸索引文件所在目录；
* `max_dynamic_modules`、`max_total_chars` 必须为正整数；
* `always=true` 模块集合不能天然超出总字符预算；
* 关键词列表存在时不能全部为空白字符串。

错误提示要求：

* 指出具体字段或模块 ID；
* 包含具体文件路径；
* 明确是“解析失败”“路径非法”“文件缺失”还是“超出预算”。

实现方式上，优先考虑在应用初始化或 `PromptConfig` 构造阶段解析索引元数据；如果继续沿用当前 prompt 文件的懒加载风格，至少也要保证首个聊天请求返回明确配置错误，而不是静默忽略。

## 12. 可观测性

建议增加 debug 级别日志：

* 当前是否启用 context modules；
* 本轮命中的模块 ID；
* `always` 模块数量；
* 动态模块命中数量；
* 因数量上限被跳过的模块 ID；
* 因字符预算被跳过的模块 ID；
* 最终模块层总字符数。

日志要求：

* 不输出模块正文；
* 不输出真实世界观或私有业务材料；
* 继续遵循现有脱敏原则。

## 13. 安全约束

必须保持：

* 用户输入不能指定任意本地文件；
* 模块文件只能来自预声明索引；
* 相对路径不能越过索引文件所在目录；
* 模块正文默认不进入日志；
* 仓库只提供 `.example` 模板，不提交真实私有模块内容。

## 14. 建议实现落点

建议最小改动路径：

1. 在 `qq-maid-core/src/config.rs` 增加 `CONTEXT_MODULES_FILE` 解析；
2. 在 `qq-maid-core/src/runtime/prompt/` 下新增模块索引与选择逻辑；
3. 让 `PromptConfig` 在普通聊天场景下接收当前 `user_text`，返回带模块的 `system_prompts`；
4. `runtime/respond/chat_flow.rs` 仅传入当前 `user_text`，不自行做匹配；
5. 在 `qq-maid-core/src/runtime/respond/tests/chat.rs` 和 `runtime/prompt` 相关测试中补覆盖；
6. 同步更新 `runtime/.env.example`、`runtime/README.md`、`qq-maid-core/README.md`，并新增公开 `.example` 模板。

可以接受的接口演进方向例如：

```rust
pub fn load_chat_system_prompts(&self, user_text: &str) -> Result<Vec<String>, LlmError>
```

或者：

```rust
pub fn load_system_prompts(&self, input: PromptSelectionInput) -> Result<Vec<String>, LlmError>
```

具体签名以实现时最小侵入为准，但不要把“模块选择”硬编码进 `handle_chat`。

## 15. 验收标准

V1 完成时至少满足：

1. 未配置 `CONTEXT_MODULES_FILE` 时，普通聊天行为与当前版本一致；
2. 支持 `always=true` 常驻模块；
3. 支持基于关键词的动态模块选择；
4. 支持稳定的优先级排序与平局规则；
5. 支持动态模块数量上限；
6. 支持总字符预算；
7. 非法路径和空文件能返回清晰配置错误；
8. debug 日志可观察命中模块和跳过原因；
9. 模块机制只进入普通聊天链路，不误伤 todo / memory / compact；
10. 不引入数据库、向量检索或外部检索服务；
11. 不自动加载索引外文件；
12. 不改动现有公开 HTTP 接口。

## 16. 测试建议

实现该任务后，最低验证应包括：

```bash
make test-core
```

如果配置解析或公共逻辑扩散到 workspace 其他成员，再执行：

```bash
make test
```

建议至少覆盖：

* 未配置模块索引时仍只加载原有固定 prompt / world / 成员映射；
* `always` 模块按顺序注入；
* 关键词命中时加载动态模块；
* 未命中时不加载；
* 同优先级模块按 `id` 排序；
* 超过 `max_dynamic_modules` 时截断；
* 超过 `max_total_chars` 时按优先级丢弃低优先级模块；
* `always` 模块超预算时报错；
* 模块文件为空时报错；
* 路径逃逸时报错；
* `memory_context`、`session_context`、时间上下文仍保持现有拼接顺序。

## 17. 后续演进

后续可以在保持当前接口的前提下，再评估：

* 基于 session state 的附加匹配；
* 标签匹配和排除规则；
* 本地全文检索；
* 与 `tasks/llm-rag-v1.md` 的知识库检索做分层衔接。

推荐演进顺序：

```text
固定 prompt + WORLD_FILE
    ↓
可配置上下文模块（本任务）
    ↓
更丰富的规则匹配
    ↓
本地知识库 / RAG
```
