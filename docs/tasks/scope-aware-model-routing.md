# TASKS：群聊 / 私聊按场景配置聊天模型与查询模型

> 来源：本地需求
> 规划版本：建议跟随下一个功能版本
> 状态：待实现
>
> 本文仅定义需求、实施边界和验收标准，不表示相关能力已经完成。
>
> 实现应拆分为小 PR：
>
> 1. 普通聊天模型场景化。
> 2. `/查` 查询模型场景化。
> 3. 健康观测与配置文档补充。

## 一、背景

当前群聊和私聊共用相同的模型配置：

* 普通聊天使用 `LLM_MODEL`。

  * 支持 `ModelRoute` 候选链。
  * 支持 `provider:model` 格式。
  * 支持使用逗号配置多个候选模型并按顺序 fallback。
* `/查` 联网查询使用 `OPENAI_SEARCH_MODEL`。

  * 当前为单个 OpenAI 查询模型。
  * 未配置时按现有规则回退。
  * 仍依赖兼容 OpenAI Responses `web_search` 的端点。

实际使用中，两类场景的模型需求不同：

* 群聊消息量较大，更关注响应速度、吞吐量和调用成本。
* 私聊通常包含更深入或连续的对话，更关注模型质量和上下文理解能力。

项目现有的 `TITLE_MODEL`、`MEMORY_MODEL`、`COMPACT_MODEL`、`TRANSLATION_MODEL` 能够显式覆盖内部辅助任务模型，但普通聊天和 `/查` 尚不能按群聊、私聊场景分别配置。

本任务只处理以下两条链路：

1. 普通聊天。
2. `/查` 联网查询。

其他专项模型配置保持现状。

---

## 二、目标

### 2.1 普通聊天模型

新增两个可选配置项：

* `GROUP_LLM_MODEL`：群聊普通聊天候选链。
* `PRIVATE_LLM_MODEL`：私聊普通聊天候选链。

配置语法与 `LLM_MODEL` 完全一致，继续使用现有 `ModelRoute` 解析、provider 校验和 fallback 机制。

### 2.2 `/查` 查询模型

新增两个可选配置项：

* `GROUP_OPENAI_SEARCH_MODEL`：群聊 `/查` 查询模型。
* `PRIVATE_OPENAI_SEARCH_MODEL`：私聊 `/查` 查询模型。

查询模型继续保持当前约束：

* 一期只支持单个模型，不支持候选链。
* 只允许现有 `OPENAI_SEARCH_MODEL` 所接受的 OpenAI 模型配置。
* 继续使用 OpenAI Responses `web_search` 兼容端点。
* 不扩展到 DeepSeek 或其他 provider。
* 配置解析和校验必须复用现有 `OPENAI_SEARCH_MODEL` 规则，不自行定义一套近似规则。

### 2.3 向后兼容

四个新配置项均为可选项。

旧环境未配置这些变量时：

* 群聊和私聊普通聊天仍使用 `LLM_MODEL`。
* 群聊和私聊 `/查` 仍使用已经解析完成的 `OPENAI_SEARCH_MODEL`。
* 现有 `.env` 无需修改。
* 外部行为应与本任务实施前一致。

---

## 三、任务类型

本任务属于以下类型的组合：

* 功能新增：支持按聊天场景选择模型。
* 配置扩展：新增四个可选环境变量。
* 内部路由调整：根据请求元数据选择模型。
* 健康观测补充：纳入新增的普通聊天候选链。
* 文档补充：更新 `.env.example` 和 README。

不涉及数据库迁移、协议重构或持久化格式变化。

---

## 四、配置语义

### 4.1 普通聊天选择优先级

群聊普通聊天：

```text
GROUP_LLM_MODEL
    ↓ 未配置或为空
LLM_MODEL
```

私聊普通聊天：

```text
PRIVATE_LLM_MODEL
    ↓ 未配置或为空
LLM_MODEL
```

场景配置只覆盖对应场景，不影响另一场景。

例如只配置 `GROUP_LLM_MODEL` 时：

* 群聊使用 `GROUP_LLM_MODEL`。
* 私聊继续使用 `LLM_MODEL`。

### 4.2 `/查` 查询模型选择优先级

群聊 `/查`：

```text
GROUP_OPENAI_SEARCH_MODEL
    ↓ 未配置或为空
已解析的 OPENAI_SEARCH_MODEL
```

私聊 `/查`：

```text
PRIVATE_OPENAI_SEARCH_MODEL
    ↓ 未配置或为空
已解析的 OPENAI_SEARCH_MODEL
```

场景查询模型未配置时，不应重新复制一遍 `OPENAI_SEARCH_MODEL` 的完整默认解析逻辑，而应直接回退到配置加载阶段已经解析完成的默认查询模型。

### 4.3 空值语义

以下情况均视为未配置：

* 环境变量不存在。
* 环境变量为空字符串。
* 环境变量去除首尾空白后为空。

空值不得产生一个空的 `ModelRoute` 或空模型名称。

### 4.4 配置示例

```env
# 默认配置，也是所有场景配置的兜底
LLM_MODEL=openai:gpt-5.4-mini
OPENAI_SEARCH_MODEL=gpt-5.5

# 群聊：优先考虑速度和成本
GROUP_LLM_MODEL=openai:gpt-5.4-mini,deepseek:deepseek-chat
GROUP_OPENAI_SEARCH_MODEL=gpt-5.5

# 私聊：优先考虑模型质量
PRIVATE_LLM_MODEL=openai:gpt-5.5
PRIVATE_OPENAI_SEARCH_MODEL=gpt-5.5
```

最小配置保持不变：

```env
LLM_MODEL=openai:gpt-5.4-mini
```

---

## 五、非目标

本任务不包含以下内容：

* 不修改 `TITLE_MODEL`。
* 不修改 `MEMORY_MODEL`。
* 不修改 `COMPACT_MODEL`。
* 不修改 `TRANSLATION_MODEL`。
* 不改变 provider 协议。
* 不改变 SSE frame 解析。
* 不重写候选链 fallback 机制。
* 不改变限流器或 provider client 的基础实现。
* 不修改 session 作用域。
* 不修改命令语义。
* 不修改持久化数据格式。
* 不引入数据库迁移。
* 不引入新依赖。
* 不让 `/查` 场景模型支持候选链。
* 不把非 OpenAI provider 接入 `/查`。
* 不通过字符串解析或 ID 格式猜测当前请求是否为群聊。

---

## 六、现状参考

### 6.1 配置读取

主要位置：

```text
qq-maid-core/src/config.rs
```

当前相关行为：

* `LLM_MODEL` 使用 `env_model_string` 和 `ModelRoute::parse_config`。
* `TITLE_MODEL`、`MEMORY_MODEL`、`COMPACT_MODEL`、`TRANSLATION_MODEL` 使用可选模型配置，未配置时使用当前场景 Profile 的 `aux_route`，再回退该场景的 `main_route`。
* `OPENAI_SEARCH_MODEL` 使用现有 OpenAI 查询模型解析和校验路径。
* `validate` 负责模型和 provider 配置校验。
* `model_routes_for_health` 负责收集健康检查需要观察的模型路由。

实现前应以仓库当前代码为准，确认这些函数的实际签名和最新调用关系。

### 6.2 普通聊天入口

主要位置：

```text
qq-maid-core/src/runtime/respond/chat_flow.rs
```

`handle_chat` 当前已经可以从请求元数据中判断是否为群聊。

普通响应和流式响应都必须使用相同的场景模型选择规则。

### 6.3 `/查` 查询入口

主要位置：

```text
qq-maid-core/src/runtime/respond/search_flow.rs
qq-maid-core/src/runtime/tools/search.rs
qq-maid-llm/src/web_search.rs
```

当前 `/查` 执行器通过类似以下路径构建：

```text
build_web_search_executor(&config.llm_config())
```

`WebSearchExecutor` 当前持有单个 `search_model`。

本任务不要求修改 `qq-maid-llm` 的 web search 请求协议，但调用方需要能够根据当前请求场景选择正确的查询模型。

### 6.4 配置模板

主要位置：

```text
runtime/config/.env.example
```

README 中的模型配置说明也需要同步更新。

---

## 七、总体实现约束

### 7.1 场景来源

群聊或私聊的判断必须来自当前请求已经携带的明确元数据，例如现有的 `is_group_chat`。

禁止：

* 解析 scope 字符串猜测场景。
* 根据用户 ID、群 ID 的格式猜测场景。
* 在配置层反向推断请求类型。
* 为同一请求在不同调用层重复计算场景。

### 7.2 集中模型选择逻辑

应优先在配置层或专用辅助函数中集中实现类似以下语义：

```text
chat_model_route_for(is_group_chat)
openai_search_model_for(is_group_chat)
```

具体函数名以仓库风格为准。

目标是避免以下位置分别实现一套优先级判断：

* 非流式聊天。
* 流式聊天。
* `/查` 命令入口。
* 查询执行器构建逻辑。
* 测试代码。

模型选择规则应只有一个可信实现来源。

### 7.3 并发安全

群聊和私聊请求可能并发执行。

不得通过以下方式实现查询模型切换：

1. 取得一个共享的 `WebSearchExecutor`。
2. 在请求开始前修改其 `search_model`。
3. 查询结束后再改回原值。

这种方式可能导致并发请求互相覆盖模型配置。

应采用不可变、请求隔离的方案，例如：

* 为不同场景构建独立的不可变执行器。
* 在调用点根据场景构建使用对应模型的执行器。
* 在不改变 LLM 层协议的前提下，将选中的模型作为构建参数传入。

具体落点由 Codex 根据当前依赖注入和生命周期设计决定，但不得引入共享可变模型状态。

---

## 八、阶段一：普通聊天模型场景化

### 8.1 配置扩展

在配置结构中新增等价字段：

```rust
group_llm_model: Option<ModelRoute>
private_llm_model: Option<ModelRoute>
```

字段具体归属以当前配置结构为准，可以位于 `CoreConfig` 或现有 LLM 子配置中，但应保持项目现有组织方式。

从环境变量读取：

```text
GROUP_LLM_MODEL
PRIVATE_LLM_MODEL
```

要求：

* 使用现有可选 `ModelRoute` 解析路径。
* 语法与 `LLM_MODEL` 完全一致。
* 支持逗号分隔的候选链。
* 支持已有的 `provider:model` 语法。
* provider 前缀的接受和拒绝规则与 `LLM_MODEL` 一致。
* 空值解析为 `None`。

### 8.2 配置校验

`validate` 必须覆盖新增的场景聊天模型：

* 每一个候选项都应经过现有 provider 校验。
* 非法 provider 前缀应在启动配置校验阶段报错。
* 不应等到收到对应场景消息后才暴露配置错误。
* 错误信息应能够识别出是哪个环境变量配置错误。

### 8.3 聊天链路

在普通聊天入口，根据当前请求的 `is_group_chat` 选择模型：

* 群聊优先使用 `GROUP_LLM_MODEL`。
* 私聊优先使用 `PRIVATE_LLM_MODEL`。
* 未配置时回退 `LLM_MODEL`。

必须同时覆盖：

* 非流式聊天。
* 流式聊天。

除模型路由外，以下行为保持不变：

* system prompt 选择。
* session 读取与保存。
* conversation history。
* pending 状态。
* summary / compact 行为。
* 标题生成。
* 消息发送。
* 错误处理。
* 候选链 fallback。
* provider 限流。
* 流式输出行为。

### 8.4 阶段一测试

至少覆盖以下组合：

1. 群聊和私聊模型均未配置。

   * 两种场景都使用 `LLM_MODEL`。
2. 只配置 `GROUP_LLM_MODEL`。

   * 群聊使用群聊模型。
   * 私聊使用 `LLM_MODEL`。
3. 只配置 `PRIVATE_LLM_MODEL`。

   * 私聊使用私聊模型。
   * 群聊使用 `LLM_MODEL`。
4. 两者均配置。

   * 群聊和私聊分别使用对应模型。
5. 群聊模型配置多个候选项。

   * 能按 `ModelRoute` 解析为完整候选链。
6. 私聊模型 provider 前缀非法。

   * 配置校验明确失败。
7. 群聊模型 provider 前缀非法。

   * 配置校验明确失败。
8. 流式和非流式入口。

   * 对相同场景选择相同的模型路由。

测试应尽量验证实际传入 provider 或聊天执行链的路由，而不只测试一个与生产调用脱节的辅助函数。

---

## 九、阶段二：`/查` 查询模型场景化

### 9.1 配置扩展

新增等价字段：

```rust
group_openai_search_model: Option<String>
private_openai_search_model: Option<String>
```

读取环境变量：

```text
GROUP_OPENAI_SEARCH_MODEL
PRIVATE_OPENAI_SEARCH_MODEL
```

要求：

* 允许整个配置项为空。
* 非空时严格复用 `OPENAI_SEARCH_MODEL` 的解析、标准化和校验规则。
* 不自行发明新的 provider 语法。
* 不因为名称包含 `OPENAI` 就简单跳过校验。
* 不允许通过场景配置绕过现有 OpenAI web search 约束。

如果当前 `OPENAI_SEARCH_MODEL` 允许或拒绝某种前缀形式，两个新配置项必须保持完全相同的行为。

### 9.2 场景信息透传

把当前请求是否为群聊的明确状态透传到 `/查` 执行链。

要求：

* 使用当前请求已有的元数据。
* 不从 session ID、scope key 或命令文本反推。
* 场景状态在进入查询链后不能丢失。
* 不改变 `/查` 的用户命令格式。

### 9.3 查询模型选择

执行 `/查` 时：

* 群聊优先使用 `GROUP_OPENAI_SEARCH_MODEL`。
* 私聊优先使用 `PRIVATE_OPENAI_SEARCH_MODEL`。
* 未配置时回退已经解析完成的 `openai_search_model`。

不得影响：

* 查询 prompt。
* web search tool 配置。
* Responses 请求协议。
* 查询结果解析。
* Markdown 渲染。
* 错误回退和错误提示。
* 其他使用 OpenAI Responses 的功能。

### 9.4 执行器生命周期

由于 `WebSearchExecutor::search_model` 当前为单值，实现可以根据仓库现状选择以下方式之一：

* 为群聊、私聊分别构建不可变执行器。
* 根据当前请求选中的模型构建短生命周期执行器。
* 在 Core 层增加轻量包装，按场景选择已经构建好的执行器。

禁止通过修改共享执行器内部模型字段实现请求级切换。

不要求修改 `qq-maid-llm` 层的公开协议，除非仓库实际结构证明无法在 Core 调用侧完成；如确实需要调整，应保持修改最小，并在完成报告中解释原因。

### 9.5 阶段二测试

至少覆盖以下组合：

1. 两个场景查询模型均未配置。

   * 群聊和私聊都使用 `OPENAI_SEARCH_MODEL`。
2. 只配置群聊查询模型。

   * 群聊使用群聊查询模型。
   * 私聊使用默认查询模型。
3. 只配置私聊查询模型。

   * 私聊使用私聊查询模型。
   * 群聊使用默认查询模型。
4. 两者均配置。

   * 群聊和私聊分别使用对应查询模型。
5. 场景查询模型为空字符串或纯空白。

   * 视为未配置并正常回退。
6. 场景查询模型配置不符合现有 OpenAI 查询模型规则。

   * 启动配置校验失败。
7. 并发场景。

   * 群聊和私聊查询不会因共享可变状态而串用模型。

并发测试可以通过构造两个不同场景的请求并验证最终执行模型完成，不要求发起真实外部网络请求。

---

## 十、阶段三：健康观测与文档

### 10.1 健康观测

将以下普通聊天模型路由纳入现有上游健康观测：

* `LLM_MODEL`。
* `GROUP_LLM_MODEL`。
* `PRIVATE_LLM_MODEL`。

采用合并去重策略：

* 相同 provider 或相同实际探测目标不重复探测。
* 不因为同一模型同时出现在默认和场景配置中而显示多条重复状态。
* 保持现有 `upstream_status` 输出结构和语义。
* 不新增场景维度的重复健康请求。

本任务不要求额外扩展 `/查` 查询模型的健康探测范围。

如果现有 `OPENAI_SEARCH_MODEL` 已经被某套健康逻辑覆盖，则新增场景查询模型应保持同类语义；如果当前未覆盖，则本任务不额外新增查询模型探测。

### 10.2 `.env.example`

在以下配置附近增加新变量说明：

```text
LLM_MODEL
OPENAI_SEARCH_MODEL
```

需要补充：

* `GROUP_LLM_MODEL`。
* `PRIVATE_LLM_MODEL`。
* `GROUP_OPENAI_SEARCH_MODEL`。
* `PRIVATE_OPENAI_SEARCH_MODEL`。
* 场景配置均为可选项。
* 未配置时回退对应默认模型。
* 普通聊天支持候选链。
* `/查` 场景模型一期只支持单个 OpenAI 查询模型。
* 提供简短、可直接使用的示例。

不得在示例中放入密钥、真实私有地址或部署环境专属值。

### 10.3 README

在模型配置小节增加简短说明：

* 普通聊天可按群聊、私聊设置不同候选链。
* `/查` 可按群聊、私聊设置不同查询模型。
* 四个变量均可省略。
* 省略后保持默认行为。

README 只说明用户可见配置方式，不复制实现细节或完整测试矩阵。

---

## 十一、PR 拆分要求

### PR 1：普通聊天模型场景化

范围：

* 场景聊天配置读取。
* 配置校验。
* 模型选择 helper。
* 非流式和流式聊天链路接入。
* 对应单元测试。

不包含：

* `/查` 改造。
* README 大段更新。
* 无关配置重构。

### PR 2：`/查` 查询模型场景化

范围：

* 场景查询模型配置。
* 查询链路场景透传。
* 查询模型选择。
* 并发安全处理。
* 对应单元测试。

不包含：

* 修改 web search provider 协议。
* 为查询模型增加候选链。
* 无关的 query 模块重构。

### PR 3：健康观测与文档

范围：

* 场景聊天模型健康路由收集。
* 去重行为验证。
* `.env.example`。
* README。
* 必要的文档或配置测试。

如果健康观测改动与 PR 1 耦合度很高，可以在 PR 1 中完成代码部分，在 PR 3 中只补文档；但不得因此把普通聊天和 `/查` 两个功能合并成一个大 PR。

---

## 十二、兼容性要求

* 不新增必填配置。
* 旧 `.env` 无需修改。
* 未配置四个新变量时，运行行为保持不变。
* 配置一个场景时，另一个场景不受影响。
* 不改变历史 session 的读取方式。
* 不改变现有消息 scope。
* 不改变命令格式和响应格式。
* 不改变 provider fallback 顺序。
* 不改变数据库 schema。
* 不改变公开协议语义。
* 不影响 title、todo、memory、compact、translation 等专项模型。

---

## 十三、风险与注意事项

### 13.1 流式链路遗漏

普通聊天通常存在流式和非流式两条调用路径。

如果只修改其中一条，会造成相同场景在不同 transport 下使用不同模型。

必须通过测试明确覆盖两条路径。

### 13.2 查询执行器共享状态

如果查询执行器是全局共享对象，直接修改其 `search_model` 会产生并发串模型风险。

模型选择必须保持不可变或请求隔离。

### 13.3 配置回退重复实现

不要在多个调用点手写：

```rust
if is_group_chat {
    group_model.as_ref().unwrap_or(&default_model)
} else {
    private_model.as_ref().unwrap_or(&default_model)
}
```

应集中封装选择逻辑，避免后续配置语义发生漂移。

### 13.4 健康观测重复探测

同一个 provider 或模型可能同时出现在：

* 默认模型。
* 群聊模型。
* 私聊模型。
* 候选链中的多个位置。

健康观测应沿用现有身份定义进行去重，不应按配置变量名机械地产生重复探测。

### 13.5 错误信息

非法场景配置应在启动时失败，并指出相关配置项。

不得：

* 静默忽略非法配置。
* 自动改用默认模型掩盖错误。
* 吞掉 provider 前缀校验错误。
* 对错误配置伪造健康状态。

---

## 十四、验收标准

### 14.1 普通聊天

* 配置 `GROUP_LLM_MODEL` 后，群聊普通聊天使用该候选链。
* 配置 `PRIVATE_LLM_MODEL` 后，私聊普通聊天使用该候选链。
* 任一场景未配置时，使用 `LLM_MODEL`。
* 只配置一个场景不会影响另一个场景。
* 流式和非流式聊天使用相同的场景选择规则。
* 候选链解析和 provider 校验与 `LLM_MODEL` 一致。
* title、todo、memory、compact、translation 模型选择不受影响。

### 14.2 `/查`

* 配置 `GROUP_OPENAI_SEARCH_MODEL` 后，群聊 `/查` 使用该模型。
* 配置 `PRIVATE_OPENAI_SEARCH_MODEL` 后，私聊 `/查` 使用该模型。
* 场景项未配置时，回退 `OPENAI_SEARCH_MODEL`。
* `/查` 继续使用单个 OpenAI 查询模型。
* 不支持查询候选链。
* 群聊和私聊并发查询不会串用模型。
* 现有查询结果解析和错误处理保持不变。

### 14.3 配置与健康观测

* 旧配置可以直接启动，行为不变。
* 非法场景聊天 provider 配置会明确报错。
* 非法场景查询模型会按现有 OpenAI 查询模型规则报错。
* 场景聊天候选链纳入上游健康观测。
* 重复 provider 或探测目标不会重复展示和探测。
* `.env.example` 和 README 已同步更新。

---

## 十五、测试与检查要求

每个 PR 应运行与自身范围相关的最小测试。

最终合并前运行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

还应完成：

* `.env.example` 文本自检。
* 确认新变量没有被错误写成必填项。
* 确认空值回退行为。
* 确认流式与非流式普通聊天均已接入。
* 确认 `/查` 不存在共享可变模型状态。
* 确认健康观测去重。

如果完整 workspace 检查因与本任务无关的既有问题无法通过，应：

1. 运行受影响 crate 或模块的定向测试。
2. 记录无法运行完整检查的具体原因。
3. 不得声称未实际执行的检查已经通过。

---

## 十六、给 Codex 的执行要求

实现前请先：

1. 阅读仓库根目录的 `AGENTS.md`、`README.md` 和相关开发文档。
2. 检查当前分支状态和未提交改动。
3. 搜索所有 `LLM_MODEL`、`OPENAI_SEARCH_MODEL`、`ModelRoute`、`build_web_search_executor`、`WebSearchExecutor`、`is_group_chat` 的实际调用点。
4. 确认流式和非流式聊天的真实调用链。
5. 确认 `/查` 执行器的创建位置、所有权和生命周期。
6. 确认当前健康观测的去重身份是 provider、模型还是 endpoint。
7. 以仓库当前实现为准，不要仅根据本文提供的文件名猜测调用关系。

实现原则：

* 优先复用现有解析、校验和 fallback 逻辑。
* 采用最小、清晰、可维护的修改。
* 不进行与任务无关的重构。
* 不擅自修改公开接口语义。
* 不新增依赖。
* 不硬编码模型名称。
* 不通过吞掉错误实现兼容。
* 不通过共享可变状态切换查询模型。
* 不伪造测试、构建或运行结果。

若仓库实际情况与本文描述不一致，应以仓库事实为准，并在完成报告中说明：

* 哪项描述已经过时。
* 实际实现位于哪里。
* 最终采用了什么调整方案。
* 调整是否影响本文定义的外部行为。

---

## 十七、完成后输出

每个 PR 完成后，Codex 需要说明：

1. 当前实现的调用链和原有问题。
2. 本次采用的模型选择方案。
3. 修改了哪些文件。
4. 每个文件解决了什么问题。
5. 如何保证群聊和私聊不会串用模型。
6. 如何处理未配置和空值回退。
7. 健康观测如何去重。
8. 新增或修改了哪些测试。
9. 实际执行了哪些命令。
10. 各项测试和检查的真实结果。
11. 是否存在未解决问题、兼容性风险或后续建议。

---

## 十八、已确定决策

本任务按以下决策执行，无需再次确认：

1. 健康观测采用合并去重展示，不按群聊、私聊重复探测同一上游。
2. `/查` 场景模型一期保持单值，不引入候选链。
3. 普通聊天场景模型继续使用现有 `ModelRoute` 候选链。
4. 四个新配置均为可选项。
5. 场景判定使用现有请求元数据。
6. 查询模型切换不得依赖共享可变状态。
