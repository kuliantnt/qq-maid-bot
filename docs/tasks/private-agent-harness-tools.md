# TASKS：私聊轻量 Agent / Harness 能力

> 来源：GitHub Issue #57「feat：为私聊场景增加轻量 Agent / Harness 能力」
>
> 规划版本：v0.10.0。
>
> 本文只做任务拆解和实施边界定义，不表示所有能力已经完成。实现时应按阶段拆分 PR，避免一次性引入完整 Agent 框架、宿主机执行环境或不受控文件访问。

## 任务目标

在私聊场景中，为 `qq-maid-bot` 增加基于模型原生 Tools 的轻量任务执行能力，让用户可以直接用自然语言提出搜索、网页阅读、文件处理、代码分析和多步骤整理任务。

首期目标是：

* 私聊中无需新增 `/agent`、`/run` 等显式命令；
* 普通聊天继续保持原有体验，不强制调用工具；
* 优先复用模型供应商托管的 Web Search、Code Interpreter、文件输入和文件输出能力；
* 仅在私聊开放通用任务执行，群聊继续沿用现有命令、@ 和普通聊天策略；
* 不在机器人宿主机直接运行模型生成的代码；
* 不向模型暴露宿主机文件、配置、日志、数据库、环境变量和源码；
* 业务写入能力必须复用现有 Todo、Memory、RSS 等确认和权限机制。

建议开发分支按阶段拆分，例如：

```text
feat/private-agent-harness-probe
feat/private-agent-web-tools
feat/private-agent-files-code
feat/private-agent-business-tools
```

---

## 0. 前置现状确认

实现前先确认当前仓库中的：

* `qq-maid-llm` 的 Provider 抽象、OpenAI Responses、Chat Completions fallback、Web Search 协议和候选链；
* `qq-maid-core` 的 `CoreService::respond`、普通聊天 flow、`/查` flow、pending 确认、Todo、Memory、RSS、天气和 session 作用域；
* `qq-maid-gateway-rs` 的私聊 / 群聊事件区分、附件备注拼接、消息发送和日志脱敏；
* `runtime/config/.env.example` 中现有模型、搜索、超时、并发和群聊策略配置；
* 当前供应商实际支持的原生 Tools，包括 Web Search、Code Interpreter、File Search、文件输入和文件输出；
* 现有 `/查` Web Search 是供应商原生工具协议还是项目内固定搜索流程。

输出一份简短调查结果，至少说明：

* 当前已可复用的能力；
* 当前缺失的协议字段或抽象；
* 哪些能力必须固定路由到支持 Tools 的模型；
* 哪些能力暂不可用，不能伪造成功。

---

## 1. 建立 Harness 能力边界

新增轻量 Harness 的内部边界，不直接把工具循环堆入现有聊天 flow。

建议职责划分：

* `qq-maid-llm`：承载模型原生 Tools 协议、tool choice、工具事件解析、连续调用请求 / 响应结构和 provider 能力声明；
* `qq-maid-core`：承载私聊任务入口判断、用户可见状态文案、业务工具注册、权限检查、pending 确认衔接和结果排版；
* `qq-maid-gateway-rs`：只负责传入私聊 / 群聊目标、附件元信息和发送结果，不理解模型工具协议。

工具类型边界：

* Hosted Tool：由 Provider 执行，`qq-maid-llm` 负责请求字段、流式事件、结果、来源标注和文件标识解析；
* Function Tool：由模型提出调用，`qq-maid-core` 负责服务端白名单、参数校验、权限、scope 和业务执行，再将结构化结果返回模型；
* `qq-maid-llm` 不得执行 Todo、Memory、RSS、知识库等业务逻辑；
* `qq-maid-core` 不得重复实现 Provider SSE、Responses 原生工具协议和模型候选链。

实现要求：

* 不让 `qq-maid-llm` 反向依赖 `qq-maid-core`；
* 不让 `qq-maid-core` 重新实现 Provider 协议、SSE frame 解析或模型候选链；
* Harness 请求必须带明确的 scope、用户身份和私聊 / 群聊上下文；
* 工具执行结果必须有结构化错误，不能用空字符串或普通文本伪造成功；
* 工具最大轮数、总超时、单工具超时、最大工具调用次数和最大输出长度必须有上限；
* Harness 任务必须接入现有 LLM limiter，并额外限制单用户并发和全局并发；
* 达到并发、调用次数、文件大小或超时限制时必须返回明确错误，不能无限排队、无限重试或静默降级；
* 日志默认脱敏，不记录完整工具输入中的密钥、文件内容和用户隐私。

---

## 2. 私聊入口与触发策略

首期只允许私聊进入通用 Harness。Harness 消费的是聚合后的逻辑用户回合，不直接处理平台推送的物理入站消息。

统一入站链路应与消息聚合保持一致：

```text
物理入站消息
→ 命令 / Pending / 控制消息分类
→ 普通文本聚合
→ 逻辑用户回合
→ Dispatcher
→ Core 普通聊天 / Harness
```

实现要求：

* Aggregator 负责物理消息聚合，Harness 只处理逻辑用户回合；
* 聚合关闭时，单条普通消息直接形成逻辑用户回合，再按同一入口进入 Dispatcher 和 Core；
* 命令、Pending 和控制消息优先在入站分类阶段分流，不进入通用 Harness；
* 建议先完成共同入站接口或消息聚合，再接入 Harness 私聊入口，避免 Harness 重复实现命令和 Pending 分类；
* `C2C_MESSAGE_CREATE` 私聊普通文本可形成逻辑用户回合，并进入支持 Tools 的聊天链路；
* 群聊 `GROUP_AT_MESSAGE_CREATE` 和 `GROUP_MESSAGE_CREATE` 不开放通用 Harness、代码执行和文件处理；
* 群聊原有命令、@、回复机器人和 active 关键词策略保持不变；
* `/todo`、`/memory`、`/查`、`/天气`、`/rss` 等现有显式命令优先级保持不变；
* pending 确认流程优先于 Harness，避免用户确认被普通任务吞掉；
* 用户发送“取消”时，只有命中现有 Pending 取消、显式命令或已定义控制消息状态，才作为控制输入处理；
* 无 Pending 或控制状态时，普通文本“取消”不得被 Harness 入口硬编码为任务取消，避免误杀用户正常表达；
* 普通私聊不额外做“是否需要工具”的硬分类，由支持 Tools 的模型自行决定是否调用工具。

验收点：

* 私聊“今天有点累”可以直接普通回复；
* 私聊“搜一下这个项目最近几个版本更新了什么”可以进入工具链；
* 群聊提出同类任务不会启用通用 Harness；
* 现有命令和 pending 确认不受影响。

---

## 3. Provider 原生 Tools 能力声明

在 LLM 层扩展 provider 能力描述，用于启动期校验和运行时路由。

建议至少表达：

* 是否支持 Responses API 工具调用；
* 是否支持 Web Search；
* 是否支持 Code Interpreter；
* 是否支持文件输入；
* 是否支持文件输出；
* 是否支持连续工具调用；
* 是否支持流式工具事件；
* 不支持时的明确错误类型。

实现要求：

* 当前模型不支持某项 Tool 时，返回明确错误或不注册对应能力；
* 不允许把不支持 Tools 的普通聊天模型伪装成可执行工具模型；
* 支持 Tools 的模型路由应可独立于普通聊天 `LLM_MODEL` 配置；
* Harness 任务开始后必须固定 Provider 和模型；
* 已进入工具循环后禁止静默 fallback 到其他 Provider 或模型；
* fallback 只允许发生在任务首轮开始前；已产生工具状态后，应明确失败或从头重启，不能带着旧 Provider 状态继续请求；
* fallback 到不支持 Tools 的模型时必须有显式策略，不能静默降级后伪造工具结果。

Provider 状态隔离要求：

* response ID、previous response ID、conversation ID、Code Interpreter container ID、provider file ID 和 continuation token 都必须视为受保护的 Provider 状态标识；
* 所有 Provider 状态标识必须绑定 `task_id`、`user_id`、scope、bot instance 和 provider；
* Provider 状态标识不得跨用户、跨会话、跨机器人实例或跨 Provider 复用；
* 不得接受用户直接提供的 Provider 状态 ID，即使格式看起来合法也只能作为普通文本处理或拒绝；
* 继续请求前必须校验状态归属，校验失败时返回明确越权或状态失效错误；
* 日志中不得打印完整 Provider 状态 ID，可记录脱敏前缀或哈希用于排障。

建议配置项：

```env
AGENT_HARNESS_ENABLED=false
AGENT_HARNESS_MODEL=
AGENT_HARNESS_MAX_TOOL_ROUNDS=8
AGENT_HARNESS_TOTAL_TIMEOUT_SECONDS=180
AGENT_HARNESS_TOOL_TIMEOUT_SECONDS=60
AGENT_HARNESS_MAX_OUTPUT_CHARS=12000
AGENT_HARNESS_MAX_CONCURRENT_PER_USER=1
AGENT_HARNESS_MAX_CONCURRENT_GLOBAL=4
AGENT_HARNESS_MAX_TOOL_CALLS_PER_TASK=16
AGENT_HARNESS_MAX_INPUT_FILE_TOTAL_BYTES=52428800
```

资源限制必须覆盖单用户 Harness 并发、全局 Harness 并发、单任务各类工具最大调用次数、单任务最大输入文件总量，并复用现有 LLM 并发 limiter 的 permit 释放语义。

配置命名可按实现调整，但必须写入 `.env.example` 并说明默认关闭。

---

## 4. 接通模型原生 Web Search

优先复用当前 `qq-maid-llm` 已有 OpenAI Responses + `web_search` 协议，不重复新增功能相同的搜索模块。

实现要求：

* 将现有 `/查` 的 Web Search transport 能力抽象为 Harness 可复用的 LLM 工具能力；
* 保留 `/查` 现有命令行为和排版，不把它强行迁成 Harness；
* Harness 中的 Web Search 结果应能回到模型继续推理，而不是只直接返回给用户；
* Provider 返回 sources / citations 等来源标注时必须保留；
* QQ 无法表达来源原格式时，最终回复必须转为纯文本来源列表；
* 来源列表至少保留标题与 URL，不得静默丢弃全部来源；
* 搜索失败时说明失败步骤，不编造搜索结果。

验收点：

* 私聊“搜一下 Rust 最近版本变化并整理摘要”可触发搜索并总结；
* 私聊普通聊天不会强制搜索；
* `/查 关键词` 仍保持原有命令行为。

---

## 5. Web Fetch 评估与受控实现

先确认模型原生 Web Search 是否能可靠读取用户指定 URL。

如果供应商原生能力足够：

* 不新增自定义 Web Fetch；
* 在文档中说明依赖供应商托管能力。

如果必须新增自定义 Web Fetch：

* 仅允许 `http` / `https`；
* 禁止 localhost、内网地址、link-local、multicast、云元数据地址和非公开地址；
* DNS 解析和重定向后都必须重新检查目标地址；
* 限制响应大小、超时、跳转次数和 content-type；
* 不携带用户登录态、Cookie、宿主机 header 或内网凭据；
* 复用项目现有 SSRF 防护思路，优先参考 RSS 拉取的私网地址拦截约束。

验收点：

* 公开网页可以读取并总结；
* `localhost`、内网 IP、metadata 地址和重定向到私网的 URL 被拒绝；
* 失败时明确说明无法读取网页。

---

## 6. Code Interpreter 与文件处理

首期只接入供应商托管的 Code Interpreter 或等价隔离代码工具，不在机器人宿主机执行模型生成的代码。

实现要求：

* Code Interpreter 能力必须来自模型供应商托管环境；
* 如果当前供应商不支持该能力，应明确标记不可用；
* 用户上传文件只允许在当前任务上下文中使用；
* 模型生成文件只保存到受控临时区域或直接通过供应商文件输出返回；
* 文件名、大小、数量、保存时间和 MIME 类型需要限制；
* 不允许通过用户传入路径读取宿主机文件；
* 不允许访问 `runtime/data/`、`runtime/logs/`、`.env`、源码、SQLite、SSH 配置或系统路径；
* 生成结果文件返回给用户前应有大小和类型检查。

文件完整链路：

1. Gateway 获取 QQ 附件；
2. Gateway 校验文件大小、名称和 MIME；
3. Gateway 写入受控临时区域；
4. Core 为文件绑定 `task_id`、`user_id` 和 scope；
5. LLM 层上传至支持文件工具的 Provider；
6. LLM 层解析 Provider 返回的生成文件标识；
7. LLM 层下载结果，Core / Gateway 再次校验大小和类型；
8. Gateway 将结果文件发送给当前用户；
9. 成功、失败、超时和取消场景都必须执行本地临时文件清理。

职责与归属校验：

* Gateway 负责 QQ 附件下载与发送；
* Core 负责任务级文件权限、生命周期和清理编排；
* `qq-maid-llm` 负责 Provider 文件协议、上传、结果标识和下载；
* 任何文件读取都必须校验 `task_id` 与 `user_id` 归属；
* 不得仅凭用户提供的路径或 provider file ID 读取文件；
* 实现前必须盘点 Provider 远端文件删除、过期和保留策略；无法主动删除时，须在调查结果和文档中说明。

建议补充配置：

```env
AGENT_HARNESS_FILE_INPUT_ENABLED=false
AGENT_HARNESS_FILE_OUTPUT_ENABLED=false
AGENT_HARNESS_CODE_INTERPRETER_ENABLED=false
AGENT_HARNESS_MAX_INPUT_FILE_BYTES=10485760
AGENT_HARNESS_MAX_INPUT_FILE_TOTAL_BYTES=52428800
AGENT_HARNESS_MAX_OUTPUT_FILE_BYTES=10485760
AGENT_HARNESS_FILE_RETENTION_SECONDS=86400
```

验收点：

* 用户提供 JSON / CSV / 文本文件后，可以统计并生成 Markdown 表格；
* 可以生成结果文件并返回；
* 宿主机任意路径无法被模型读取；
* 不支持 Code Interpreter 时返回明确不可用说明。

---

## 7. 自定义业务工具注册

自定义工具只补充机器人已有业务能力，不重复实现平行业务流程。

首批候选工具：

* 查询天气；
* 查询 Todo；
* 创建 Todo 草稿或发起待确认写入；
* 查询 RSS 订阅摘要；
* 查询长期记忆；
* 查询知识库片段。

实现要求：

* 所有业务工具必须通过 `qq-maid-core` 既有模块执行；
* 写入类工具必须复用 pending 确认流程；
* 写入工具需要用户确认时，必须返回结构化 `confirmation_required` 状态；
* 收到 `confirmation_required` 后，当前 Harness 必须立即停止，不继续调用其他工具；
* 最终回复只能说明“已创建待确认操作”，不得宣称 Todo、Memory、RSS 等写入已经完成；
* 用户后续确认继续走现有 pending 分发流程；
* 首期确认后不恢复原 Harness 上下文，只返回写入操作结果；
* 取消、过期、确认人与发起人不一致时，沿用现有 pending 行为；
* 工具参数必须做 schema 校验和业务校验；
* 工具执行必须带当前用户身份、scope 和权限上下文；
* 不允许模型通过任意工具名或任意函数名绕过服务端白名单；
* 工具错误要结构化返回给模型，并在最终回复中可解释。

Function Tool 幂等性要求：

* 使用 `harness_task_id + provider_tool_call_id` 作为工具调用幂等键；
* 同一 Tool Call 最多执行业务逻辑一次；
* 重复事件必须返回首次执行的结构化结果，不能再次创建 Pending 或再次写入；
* Pending 创建和所有写入类工具必须强制幂等；
* 超时但执行结果未知时不得直接重复写入，应返回状态未知、等待已有执行完成，或走人工确认 / 查询路径；
* 幂等记录必须绑定 `task_id`、`user_id`、scope、bot instance、provider 和工具名，避免跨任务或跨用户复用。

首期不要求一次接入全部业务工具。建议先接只读能力，再接写入能力。

验收点：

* “看一下明天的天气，如果下雨就帮我创建一个带伞待办”可以先查天气，再按现有确认机制发起待办写入；
* Todo、Memory、RSS 的权限和作用域不被 Tool Calling 绕过；
* 群聊不能调用通用 Harness 的业务写入工具。

---

## 8. 多轮工具循环与用户体验

Harness 需要支持模型自主决定工具调用轮数，但必须有服务端限制。

实现要求：

* 单次任务最大工具轮数可配置；
* 总耗时和单工具耗时可配置；
* 工具失败、超时、达到并发 / 调用次数限制或模型返回不合法工具调用时能中止并给出说明；
* 如首期支持用户取消正在运行的 Harness 任务，必须通过显式命令或统一控制消息触发，并绑定 `task_id`、`user_id`、scope 和 bot instance；
* Harness 取消不得仅靠自然语言关键词猜测，也不得绕过 Dispatcher / Core 既有顺序和权限边界；
* 取消正在运行的 Hosted Tool 时，如果 Provider 不支持真实中断，应标记本地任务取消、停止继续轮询或发送后续工具结果，并说明远端工具可能已完成但结果被丢弃；
* 写入工具返回 `confirmation_required` 时，必须视为当前 Harness 的终止状态，不能继续调用其他工具或把该状态解释为写入成功；
* 可按需要展示简短状态，例如“正在搜索”“正在读取网页”“正在处理文件”；
* 不展示模型内部思考过程；
* 最终回复优先展示结论、文件和操作结果；
* 失败回复说明哪一步失败、已完成哪些步骤、是否有部分结果和是否需要用户补充信息。

验收点：

* “搜索 API 文档，读取参数说明，再生成 Python 示例”可以自然完成多步；
* 工具不可用时不会无限重试；
* 超过轮数、调用次数、并发限制或超时会给出明确失败说明；
* 写入需要确认时，最终回复只说明已创建待确认操作；
* 显式取消只取消当前用户、当前 scope、当前 bot instance 下匹配的 Harness 任务；
* 普通文本“取消”在无 Pending 或控制状态时不会误杀 Harness 任务；
* 流式重复 Tool Call 事件不会重复执行业务逻辑。

---

## 9. 安全与隔离检查清单

实现和 review 时必须逐项检查：

不可信内容与跨工具数据流约束：

* Web Search 结果、Web Fetch 内容、用户上传文件及外部工具输出均视为不可信数据，不能改变系统指令、权限规则或工具策略；
* 外部内容不能授权调用业务写入工具；
* PrivateRead 工具返回的 Todo、Memory、RSS、知识库等私人数据，默认不得拼入 Web Search 查询、Web Fetch URL、请求参数或发送给第三方托管工具；
* 只有用户当前请求明确要求时，才允许将必要的最小数据发送给外部工具；
* 业务写入不能仅依据网页、搜索结果或上传文件中的指令触发；
* 工具注册信息建议包含信任或风险分类，例如 `PublicNetwork`、`PrivateRead`、`BusinessWrite`、`HostedCode`；
* 服务端必须约束危险的跨工具数据流，不能只依赖 Prompt 提醒模型。

检查清单：

* [ ] 不向模型暴露宿主机任意文件；
* [ ] 不向模型暴露源码、配置、日志、数据库、环境变量和密钥；
* [ ] 不在宿主机执行模型生成代码；
* [ ] 不访问 Docker Socket、SSH 配置、内网服务或云元数据服务；
* [ ] 自定义 Web Fetch 有 SSRF 防护；
* [ ] 文件输入只限当前任务明确上传的文件；
* [ ] 文件输出有大小、类型和保存时间限制；
* [ ] Provider 状态标识绑定 `task_id`、`user_id`、scope、bot instance 和 provider；
* [ ] 不接受用户直接提供的 response ID、conversation ID、provider file ID 或 continuation token；
* [ ] 任务进入工具循环后不会静默 fallback 到其他 Provider；
* [ ] 工具调用必须经过服务端白名单；
* [ ] 工具参数必须校验；
* [ ] 工具执行带用户身份和 scope；
* [ ] Function Tool 按 `harness_task_id + provider_tool_call_id` 幂等执行；
* [ ] 写入能力复用 pending 确认；
* [ ] `confirmation_required` 会立即终止当前 Harness，不被解释为写入成功；
* [ ] PrivateRead 数据不会默认流向 Web Search、Web Fetch 或第三方托管工具；
* [ ] 外部不可信内容不能触发业务写入；
* [ ] 危险跨工具数据流有服务端拦截；
* [ ] 群聊不开放通用 Harness、代码执行和文件处理；
* [ ] 日志不记录 raw event envelope、Authorization header、token、secret、openid 全量值和完整敏感文件内容。

---

## 10. 文档与配置更新

实现阶段需要同步更新：

* `runtime/config/.env.example`：新增 Harness、Tools、文件、超时和模型路由配置；
* `qq-maid-llm/README.md`：说明模型原生 Tools 协议、能力声明和不支持项；
* `qq-maid-core/README.md`：说明私聊 Harness 入口、业务工具边界、pending 写入约束；
* `qq-maid-gateway-rs/README.md`：说明群聊不开放通用 Harness，附件边界不变；
* `README.md`：仅在用户可用能力落地后补充简短使用说明。

文档中不得写入真实 token、openid、群号、私聊内容、文件样本中的敏感信息或生产配置。

---

## 11. 测试计划

按阶段补充测试，优先从 LLM 协议和 Core 入口做单元测试。

建议覆盖：

* Provider 能力声明解析和不支持 Tools 的错误；
* Harness 最大轮数、超时、并发上限、工具调用次数和工具错误处理；
* 单用户并发、全局并发、限额释放、超时后 permit 归还；
* Web Search 工具结果回传模型继续处理；
* 私聊允许、群聊拒绝的入口分支；
* pending 优先级高于 Harness；
* Pending 取消优先于 Harness，且无 Pending 时普通文本“取消”不会误触发任务取消；
* 显式 Harness 取消只影响同一 `task_id`、`user_id`、scope 和 bot instance 的任务；
* 现有 `/查`、`/todo`、`/memory`、普通聊天命令不回归；
* Web Fetch SSRF 拦截，包括 localhost、内网 IP、metadata 地址和重定向绕过；
* 恶意网页诱导读取记忆、把私人数据放入 URL、外部内容诱导写入等请求必须被拒绝；
* 文件大小、总量、类型、路径穿越、归属校验、Provider file ID 越权和保留时间限制；
* 跨用户 Provider 状态 ID 不能继续请求或读取文件；
* 跨 Provider continuation token 不能复用；
* 已产生工具状态后的错误 fallback 不会静默切换 Provider；
* 成功、失败、超时和取消场景都会清理本地临时文件；
* 业务写入工具不绕过确认流程；
* 重复 Tool Call 事件返回首次结构化结果，不重复执行业务逻辑；
* 流式重复事件不会重复创建 Pending 或重复写入；
* 超时重试在执行结果未知时不会直接重复写入；
* 模型不得把 `confirmation_required` 解释为成功；
* 工具失败最终回复不伪造成功。

涉及 Provider 协议、SSE 或候选链时，需要至少执行：

```bash
cargo fmt --all -- --check
cargo test -p qq-maid-llm --all-features
cargo test -p qq-maid-core --all-features
cargo test --workspace --all-features
```

提交前按影响范围补充：

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --release --all-features
```

---

## 12. 首期建议拆分

### Phase 1：能力调查与配置骨架

* 输出 provider 原生 Tools 支持矩阵；
* 增加 Harness 默认关闭配置；
* 增加 provider capability 类型和启动期校验；
* 不改变用户可见行为。

### Phase 2：私聊 Web Search Harness

* 私聊逻辑用户回合可进入支持 Tools 的模型链路；
* 复用现有 OpenAI Responses Web Search 能力；
* 保持 `/查` 和群聊行为不变；
* 增加失败说明和轮数 / 超时保护。

### Phase 3：网页读取与连续调用

* 评估原生 Web Fetch 能力；
* 必要时实现受控 Web Fetch；
* 支持搜索后读取网页并继续生成结果；
* 完成 SSRF 防护测试。

### Phase 4：文件与托管代码工具

* 接入供应商托管 Code Interpreter；
* 支持用户文件输入和结果文件输出；
* 完成文件边界、大小、类型和保存时间限制；
* 不在宿主机执行模型代码。

### Phase 5：业务工具

* 先接天气、Todo 查询等只读工具；
* 再接 Todo 创建等写入工具；
* 写入统一走 pending 确认；
* 补齐权限、作用域和 Tool Call 幂等测试。

---

## 13. 验收场景

* 普通私聊：直接返回文本，不强制调用工具；
* 搜索任务：可以使用 Web Search 获取信息并整理结果；
* 连续调用：可以搜索、读取网页、处理内容并自然结束；
* 文件处理：可以读取用户明确提供的文件并处理；
* 代码执行：只使用供应商托管 Code Interpreter 返回真实结果；
* 结果文件：可以生成受控文件并返回给用户；
* 写入确认：Todo、Memory、RSS 等写入不绕过现有确认；
* 工具幂等：重复 Tool Call、流式重复事件和超时重试不会重复创建 Pending 或重复写入；
* 状态隔离：Provider 状态 ID 不跨用户、跨会话、跨机器人实例或跨 Provider 复用；
* 固定路由：任务进入工具循环后不静默 fallback 到其他 Provider 或模型；
* 用户取消：Pending 取消和显式 Harness 取消有清晰边界，普通文本“取消”不会误触发；
* 群聊限制：群聊不能使用通用 Harness、代码执行和文件处理；
* 宿主机保护：工具无法读取源码、配置、密钥、数据库、日志和环境变量；
* 工具不支持：当前模型或供应商不支持某项 Tool 时明确反馈，不伪造执行结果。

## 暂不包含

首期不做：

* 群聊 Agent；
* `/agent` 显式命令；
* 多 Agent 协作；
* 后台长期任务；
* 定时自主执行；
* 浏览器登录和账号操作；
* 图形化浏览器自动化；
* 宿主机代码执行；
* 任意 Shell；
* Docker 管理权限；
* 未经确认的业务写入；
* 无限制网络访问；
* 自建复杂沙箱平台；
* 复杂工作流编辑器；
* 一次接入全部业务工具。
