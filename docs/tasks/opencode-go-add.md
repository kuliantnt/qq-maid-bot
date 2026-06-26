# TASKS：接入 OpenCode Go / GLM-5.2 聊天链路

## 任务目标

为 `qq-maid-bot` 增加 OpenCode Go Provider，使用 GLM-5.2 测试中文陪聊、人设保持、多人群聊和长对话表现。

本次接入要求：

* 支持私聊和群聊；
* 支持配置为普通聊天主模型或候选模型；
* 图片及其他附件继续沿用现有“附件转文本备注”行为，不因接入 OpenCode Go 而改变；
* 可恢复的调用失败进入现有候选链；
* 不影响命令、工具和后台任务；
* 不对现有 LLM 架构做无关重构；
* 可以通过配置完整关闭。

建议开发分支：

```text
feat/opencode-go-provider
```

---

## 1. 检查现有实现

先检查当前仓库中的：

* Provider 抽象和注册方式；
* OpenAI-compatible Chat Completions 实现；
* `ModelRoute` 和候选模型链；
* 普通聊天请求入口；
* 私聊与群聊上下文构造；
* 图片等附件的文本备注处理（当前 gateway 将附件追加为 `[附件 ...]` 文本，无独立多模态 Provider）；
* SSE 流式解析和非流式兜底；
* 配置加载及 `.env.example`。

优先复用现有 OpenAI-compatible 客户端。

如果现有实现已经支持自定义：

* `base_url`
* `api_key`
* `model`
* Chat Completions

则只增加 Provider 配置、注册和路由识别，不复制 HTTP 或 SSE 实现。

---

## 2. 增加 OpenCode Go Provider

Provider 标识：

```text
opencode-go
```

默认模型：

```text
glm-5.2
```

Base URL：

```text
https://opencode.ai/zen/go/v1
```

请求端点：

```text
POST /chat/completions
```

认证方式：

```http
Authorization: Bearer <API_KEY>
```

需要兼容：

* 流式 SSE；
* 非流式响应；
* usage 字段缺失；
* finish_reason 缺失或未知；
* 空 delta；
* 空输出；
* HTTP 401、403、429、5xx；
* 请求超时；
* 连接失败。

不要为 OpenCode Go 单独增加新的 HTTP 客户端依赖。

---

## 3. 增加配置项

环境变量命名可按仓库现有风格调整，建议：

```env
OPENCODE_GO_ENABLED=false
OPENCODE_GO_API_KEY=
OPENCODE_GO_BASE_URL=https://opencode.ai/zen/go/v1
OPENCODE_GO_MODEL=glm-5.2
```

聊天范围配置：

```env
OPENCODE_GO_PRIVATE_ENABLED=true
OPENCODE_GO_GROUP_ENABLED=true
OPENCODE_GO_ALLOWED_USER_IDS=
OPENCODE_GO_ALLOWED_GROUP_IDS=
```

白名单语义：

* `ALLOWED_USER_IDS` 为空：不限制私聊用户；
* `ALLOWED_GROUP_IDS` 为空：不限制群聊；
* 配置了 ID：只允许匹配的用户或群；
* Provider 总开关关闭时，所有范围均不生效。

要求：

* API Key 不得提交到仓库；
* API Key 不得出现在日志中；
* Provider 开启但 API Key 缺失时，应在启动阶段明确报错；
* Provider 未启用时，不创建相关客户端；
* 更新 `.env.example`。

测试环境可以直接配置：

```env
OPENCODE_GO_ENABLED=true
OPENCODE_GO_PRIVATE_ENABLED=true
OPENCODE_GO_GROUP_ENABLED=true
OPENCODE_GO_ALLOWED_USER_IDS=
OPENCODE_GO_ALLOWED_GROUP_IDS=
```

即开放全部普通私聊和群聊。

---

## 4. 接入模型候选链

现有模型路由需要识别：

```text
opencode-go:glm-5.2
```

本任务需要新增并接线普通聊天专用候选链配置。下面是目标实现完成后的配置示例；
当前仓库尚未读取该变量，实现前不要把它当作可直接生效的运行配置：

```env
CHAT_MODEL_ROUTE=opencode-go:glm-5.2,openai:<现有默认模型>
```

当前仓库主候选链由 `LLM_MODEL` 解析，且 Todo、记忆、压缩、翻译等内部模型在未配置专项变量时会沿用 `LLM_MODEL`。为了让 OpenCode Go 只进入普通聊天链路，不要只在文档中新增未接线变量，也不要简单要求部署者把 `LLM_MODEL` 改成 OpenCode Go。

实现要求：

* 扩展 `ModelProvider` / `ModelId` 前缀解析，使 `opencode-go:<model>` 成为合法候选；
* 在 Core 配置中新增 `CHAT_MODEL_ROUTE`，语法与 `LLM_MODEL` 候选链一致；
* 未设置 `CHAT_MODEL_ROUTE` 时，普通聊天继续使用现有 `LLM_MODEL`，保持向后兼容；
* 设置 `CHAT_MODEL_ROUTE` 时，只有普通聊天请求使用该候选链；
* `CHAT_MODEL_ROUTE` 需要加入启动期 provider route 校验，避免首次聊天时才发现配置错误；
* `.env.example` 和 Provider 配置文档需要说明 `CHAT_MODEL_ROUTE` 与 `LLM_MODEL` 的差异。

要求：

* OpenCode Go 可以作为普通聊天第一候选；
* 超时、限流、5xx、连接失败等可恢复错误继续尝试后续 Provider；
* 401、403 等鉴权错误继续作为配置错误暴露，不跨模型降级；
* 不改变标题、Todo、记忆、压缩和翻译等辅助模型链；
* 未配置 OpenCode Go 时，原有行为完全不变；
* 不删除现有兼容配置。

---

## 5. 支持群聊

OpenCode Go 应当支持普通群聊消息，而不是只支持私聊。

群聊链路需要保留现有：

* 群聊 system prompt；
* 当前群信息；
* 发送者昵称和用户 ID；
* 最近消息上下文；
* 当前前台成员或身份信息；
* 回复触发规则；
* @机器人检测；
* 群聊防刷屏机制。

不要因为接入新 Provider 改变机器人原有触发规则。

也就是说：

* 原来需要 @ 才回复的群，仍然需要 @；
* 原来允许自然触发的群，继续自然触发；
* OpenCode Go 只负责生成回复，不负责重新决定机器人是否应当回复。

---

## 6. 附件处理

GLM-5.2 本次只处理纯文本输入。

当前仓库没有独立的多模态 Provider：入站图片、文件、语音、视频等附件按 `qq-maid-gateway-rs/README.md` 会追加成 `[附件 ...]` 文本备注，并入 `content` 字段（`qq-maid-llm` 的 `ChatMessage.content` 为 `String`）。OpenCode Go 接入后继续沿用这一行为，不引入新的多模态分支，也不丢弃附件备注文本。

也就是说：

* 纯文本普通聊天 → OpenCode Go / GLM-5.2 → 失败后进入现有候选链；
* 带附件的消息 → 附件仍由 gateway 转为文本备注，整条消息按现有文本链路处理，OpenCode Go 与其他 Provider 一样只看到备注后的文本。

不要为了调用 GLM-5.2 而丢弃附件备注，也不要把消息导向不存在的“多模态 Provider”路径。

真正的多模态路由（按图片/语音等类型分流到支持对应模态的 Provider）属于另一个待新增能力，本任务不实现，也不在文档中假设其已存在。本任务同样不实现“先识图，再把图片摘要交给 GLM”的两阶段链路。

---

## 7. 命令和后台任务隔离

OpenCode Go 默认只进入普通聊天链路。

以下功能继续使用各自现有模型配置：

* `/todo`
* `/rss`
* `/weather`
* `/ping`
* 标题生成（继续使用现有 `TITLE_MODEL` 语义，未配置时按现有行为跳过自动标题生成）；
* 记忆提取；
* 会话压缩；
* 翻译；
* 定时任务；
* 状态检查；
* 其他命令类调用。

不要因为设置普通聊天主模型而连带改变辅助任务模型。`CHAT_MODEL_ROUTE` 只作为普通聊天覆盖；`TODO_MODEL`、`MEMORY_MODEL`、`COMPACT_MODEL`、`TRANSLATION_MODEL` 未显式配置时继续沿用现有 `LLM_MODEL` 语义，不应改为沿用 `CHAT_MODEL_ROUTE`。

---

## 8. 可选模型切换命令

如果现有系统已经有会话级模型选择能力，则增加：

```text
/model opencode-go
/model default
/model status
```

行为：

### `/model opencode-go`

* 当前私聊会话或群聊会话使用 OpenCode Go；
* 只影响普通文本聊天；
* 不修改全局配置；
* 带附件的消息仍按现有“附件转文本备注”行为处理，不因会话覆盖而改变。

### `/model default`

* 清除当前会话覆盖；
* 恢复全局模型候选链。

### `/model status`

展示：

* 当前聊天 Provider；
* 当前模型；
* 是否为会话覆盖；
* 附件备注是否按现有行为保留在输入文本中；
* 是否启用 fallback。

如果当前仓库没有会话级模型选择基础设施，本任务可以暂不增加命令，直接使用全局模型路由。

不要为了三个命令新增一套复杂持久化系统。

---

## 9. 并发和错误处理

不人为限制只能单并发测试。

复用现有聊天并发控制，同时补充：

* 429 时不进行持续重试；
* 401、403 鉴权失败按配置错误处理，启动期或调用期明确暴露，不标记为可 fallback 的临时不可用；
* 5xx 可以按现有策略重试一次；
* 空输出视为调用失败；
* SSE 中断后按现有策略处理；
* Provider 失败不得导致整条消息无回复；
* 不允许无限递归 fallback；
* 同一请求不得重复计费式重试多次。

不需要增加每小时人工请求上限，除非现有系统已经具备通用限流配置。

---

## 10. 日志与观测

记录：

* Provider；
* 模型；
* 私聊或群聊；
* 群 ID 的脱敏值或内部标识；
* 是否流式；
* 请求耗时；
* HTTP 状态码；
* 是否发生 fallback；
* 输入和输出 token 数量，字段存在时记录；
* 带附件的消息是否仍按现有文本备注链路处理。

不得记录：

* API Key；
* Authorization Header；
* 完整 system prompt；
* 完整用户消息；
* 完整模型回复。

错误类型需要区分：

* 配置缺失；
* 鉴权失败；
* 限流；
* 上游服务错误；
* SSE 解析错误；
* 空输出；
* 超时；
* 路由配置错误。

---

## 11. 测试

### 配置测试

* Provider 默认关闭；
* 开启但缺少 API Key；
* 自定义 Base URL；
* 自定义模型；
* 私聊开关；
* 群聊开关；
* 用户白名单；
* 群白名单；
* 空白名单表示不限制。

### 路由测试

* 私聊纯文本进入 OpenCode Go；
* 群聊纯文本进入 OpenCode Go；
* 群聊关闭时不进入；
* 指定群白名单生效；
* 非白名单群不进入；
* 图片消息经 gateway 转为文本备注后，按现有文本链路进入 OpenCode Go（不丢弃备注）；
* 图片加文字同样按文本备注链路进入 OpenCode Go；
* 普通命令不进入 OpenCode Go；
* Provider 关闭时恢复原有链路；
* 上游失败后进入下一候选模型。

### Provider 测试

使用 mock server 验证：

* 正常非流式响应；
* 正常 SSE 响应；
* 多个 delta 合并；
* 空 delta；
* usage 缺失；
* 401；
* 403；
* 429；
* 500；
* 超时；
* 无有效文本；
* fallback。

自动化测试不得真实调用 OpenCode Go API。

---

## 12. 文档

更新 Provider 配置文档，说明：

* 如何配置 OpenCode Go API Key；
* 如何作为普通聊天主模型；
* 如何加入候选模型链；
* 如何开启群聊；
* 如何配置群白名单；
* 图片等附件仍按现有“附件转文本备注”行为处理；
* 如何关闭并恢复原配置。

至少同步更新：

* `runtime/.env.example`：实现并接线 `CHAT_MODEL_ROUTE` 后，再增加 OpenCode Go 和 `CHAT_MODEL_ROUTE` 示例；
* `qq-maid-core/README.md`：说明 `CHAT_MODEL_ROUTE`、`LLM_MODEL` 和内部任务模型的关系；
* `qq-maid-llm/README.md`：说明新增 provider 前缀和启动期 route 校验。

无需在 README 首页大篇幅宣传，可以加入 Provider 配置表和示例。

---

## 13. 验收标准

* [ ] 可以使用 OpenCode Go 调用 `glm-5.2`；
* [ ] 私聊纯文本可以正常回复；
* [ ] 群聊纯文本可以正常回复；
* [ ] 群聊原有触发规则不变；
* [ ] 可以允许全部群或指定群；
* [ ] 图片等附件继续按现有文本备注链路处理；
* [ ] 命令和后台任务不受影响；
* [ ] OpenCode Go 的可恢复失败会自动 fallback；401/403 鉴权错误作为配置错误暴露，不 fallback；
* [ ] API Key 不出现在日志中；
* [ ] 不复制已有 HTTP/SSE 实现；
* [ ] 默认关闭时行为与修改前一致；
* [ ] `cargo fmt --check` 通过；
* [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` 通过；
* [ ] `cargo test --workspace` 通过。

---

## 14. 明确不做

本任务不包含：

* GLM 多模态适配；
* 图片摘要转交 GLM；
* Provider 架构整体重写；
* Prompt 系统重构；
* 记忆系统重构；
* Agent 或工具调用适配；
* 修改群聊触发规则；
* 修改现有命令模型配置。

目标是让 GLM-5.2 正式进入普通聊天候选链，并能够在真实私聊和群聊环境中进行效果测试。
