# qq-maid-core — Rust Core 模块

`qq-maid-core/` 是小女仆机器人的核心业务模块，负责 `CoreService`、普通聊天、联网查询命令、列车时刻查询、天气、翻译、会话、长期记忆、Todo、RSS / Atom 订阅、业务 Tool 和业务 prompt 组装。模型协议、Provider 路由、fallback、SSE、usage、健康观测、Web Search 传输和 Tool Loop 协议由 `qq-maid-llm/` 承载。

QQ 平台事件解析、白名单、`/ping` 本地诊断和消息回发不在本模块处理，相关实现见 [qq-maid-gateway-rs/README.md](../qq-maid-gateway-rs/README.md)。运行目录、私有配置、部署产物和数据文件说明见 [runtime/README.md](../runtime/README.md)。

## 当前范围

- HTTP 层默认只公开进程级 `GET /healthz`；本地 Web 控制台默认关闭，启用后才注册 `/console/`、只读状态 API 和 `/api/v1/markdown/render`。
- 普通聊天、查询、列车时刻、天气、翻译、会话命令、长期记忆、Todo、RSS、`/ops` 指令和业务 Tool 都通过 `CoreService::respond` 进程内分发。
- Session、Todo、长期记忆、RSS / Atom 订阅、RSS 去重状态和知识检索索引统一写入 `APP_DB_FILE` 指向的 SQLite。
- 长期记忆可通过确定性 `/memory` 命令或 `save_memory` Tool 写入；只有用户明确要求长期保存时才应调用 Tool，普通陈述不会自动写入。新增校验通过后直接保存，破坏性管理仍需确认。
- RSS 后台轮询、Todo 单次提醒和 Todo 每日提醒由本模块调度，推送内容先写入 Notification Outbox，再由统一 Worker 通过进程内 `PushSink` 交给 gateway 发送。
- OpenAI / DeepSeek、模型候选链 fallback、Web Search 传输、Tool Loop 协议和上游健康观测由 `qq-maid-llm` 提供，Core 只保留业务调用边界和 Tool 注册。

私聊普通聊天默认使用场景白名单 Tool Loop。群聊完整 Tool Loop 只由 `agent.toml` 的场景策略显式开启，默认关闭；关闭时只保留 Memory-only 受控路径，Registry 仅暴露 `save_memory`，由 Luna 根据 Tool 描述判断是否调用，不会同时开放 Todo 或其他写工具。群聊如需开放其他工具，必须在场景 `enabled_tools` 白名单中显式加入并开启完整 Tool Loop。slash 命令、pending 确认、文件处理和宿主机代码执行不进入 Tool Loop；`/查` 仍是显式联网查询入口。自然语言 `web_search` 与 `/查` 使用同一份场景 `search_route`；多实体对比由主 Agent 给出实体和维度后限并发独立搜索，单项失败或超时不会丢弃其他结果。

旧 HTTP `/query`、HTTP `/memory`、`/v1/chat` 等入口不再公开，也不要重新引入 Python LLM、Python 查询、Python 记忆或 Python fallback 入口。

## 模块结构

```text
qq-maid-core/src/
├── app/                 # 启动、dotenv 加载、日志、组件装配
├── config.rs            # 环境变量解析和默认值
├── http/                # /healthz、控制台和 Markdown render
├── service.rs           # CoreService / CoreHandle 进程内契约
├── runtime/
│   ├── respond/         # CoreService 后的 chat/search/weather/todo/memory/session flow
│   ├── pending/         # 跨工具 pending envelope 与通用确认分类
│   ├── rss/             # RSS / Atom 拉取、存储封装和通知任务生产
│   ├── prompt/          # 固定 prompt 加载
│   ├── knowledge/       # Markdown 知识目录扫描、分段和检索上下文
│   ├── session.rs       # 会话领域逻辑
│   ├── memory.rs        # 长期记忆领域逻辑
│   ├── todo.rs          # Todo 领域逻辑
│   ├── tools/           # Core 业务 Tool 适配层，注册天气、列车、RSS、搜索和 Todo Tool
│   ├── train/           # 列车时刻查询执行器
│   └── weather/         # 天气执行器
├── storage/             # SQLite、migration、session/memory/todo/rss/knowledge 持久化
└── util/                # 指标采集
```

`runtime/respond.rs` 是 `CoreService::respond` 后的统一业务入口；具体 flow 在 `runtime/respond/` 下维护。`runtime/tools/` 只负责把现有业务执行器包装成模型可调用 Tool，不加载 Skill 文件，也不把业务逻辑迁入 `qq-maid-llm`。通用日期、时间和时区语义统一复用 `qq-maid-common/src/time_context/`，不要在 Core 内部重复实现。

## HTTP 接口

### `GET /healthz`

返回进程级健康状态、当前 provider、模型、流式配置和当前进程内最近一次真实上游调用的脱敏快照，供控制脚本和诊断脚本探测。Gateway `/ping` 直接读取 `CoreService::health_snapshot()`，不通过 HTTP。进程启动后尚无调用时，上游状态为 `unverified`；进程重启后不会沿用旧配置下的状态。

### CoreService

Gateway 调用 Core 的唯一业务入口是 `CoreService::respond(CoreRequest)`。Gateway 只传入最终拼接后的文本、平台、成员身份和私聊 / 群聊目标；`scope_key` 由 Core 根据目标派生。私聊和群聊普通聊天是否进入完整 Tool Loop，以及模型可见的 `enabled_tools` 白名单，都由 `agent.toml` 对应场景决定。明确定义的 slash 前缀命令和 pending 确认流程继续走既有分支。`/ping check` 调用 `CoreService::upstream_check()`，该分支不进入 respond 业务 flow，不创建 session，也不触发标题、记忆、Todo、查询或 Tool Calling。

群聊入口可在不唤醒机器人的情况下把斜杠候选交给 Core。Core 依次使用现有确定性命令解析器执行已注册命令并保留原参数、角色和 scope 权限校验；所有解析器均未命中的未知群命令返回明确的静默结果，不进入聊天模型。`/ops` 在 session、pending 和模型路由之前确定性收口，拒绝、帮助和受理路径都不会进入 LLM 或 Tool Loop。私聊未知斜杠文本维持原有普通聊天兼容行为。

### `/ops` 白名单运维命令

`/ops <command> [args...]` 默认关闭。Core 从 `OPS_CONFIG_FILE`（默认 `config/ops.toml`）加载独立配置；默认路径缺失时保持关闭，显式配置的文件缺失或内容非法时启动失败。内置 `/ops codex <任务描述>`、`/ops list` 和 `/ops cancel <任务ID>` 由框架管理，但 Codex 仍有独立的 `codex.enabled=false` 默认开关及固定程序、工作目录、profile、sandbox 和并发配置。公开模板见 [`runtime/config/ops.example.toml`](../runtime/config/ops.example.toml)，完整安全边界见 [`/ops` 使用指南](../docs/development/ops-command.md)。

私聊需要同时开启总开关和私聊开关，且来自平台事件、成员 API 或可信缓存的 `actor.user_id` 必须命中 `allowed_user_ids`；历史兜底或文本弱身份不会放行。群聊还需要开启群聊开关、原始群目标命中 `allowed_group_ids`，并且 Gateway 提供的可信成员角色为 `owner` 或 `admin`；角色缺失、未知或普通成员均拒绝。`allowed_group_ids=[]` 表示不允许任何群。

每个命令只保存固定绝对 `program`，用户输入只能提供已注册命令名和通过数量、允许值或完整匹配正则校验的参数。执行使用独立 argv，不调用 `sh -c` / `bash -c`，也不解释管道、重定向或命令替换。复杂运维逻辑应由部署者封装在固定脚本中，并自行管理文件权限、sudo、systemd 或 Docker 权限。

校验通过后先按可信 `platform + account_id + message_id` 摘要原子领取，重复入站不会再次 spawn。后台进程区分成功、非零退出、超时、取消和启动失败；所有路径都只给 stdout/stderr 有限 drain 时间。结果以同源 Markdown/fallback 多段写入一个 `source_type=ops` Outbox 任务，Worker 持久化已成功段序号并从首个未确认段重试，不会重新执行脚本或 Codex。Unix 的 Codex 取消/超时终止独立进程组；Windows 首期只保证直接子进程，详见使用指南。

`scope_key` 表示 conversation scope，只描述消息发生的对话空间；`actor.user_id` 表示发言人；Todo / Memory 等业务 owner 由 `qq-maid-core/src/identity.rs` helper 在 conversation scope 上叠加 actor 推导。详细术语见 [Scope 与 Identity 边界](../docs/design/scope-identity-boundary.md)。

Memory v3 的查询相关召回、确定性后台整理、Grok Build 能力对照，以及后续会话候选边界见
[Memory v3 与 Grok Build 记忆机制对照](../docs/design/memory-grok-build-evaluation.md)。

### 统一通知接入

Notification Outbox 是业务生产者与平台投递之间的唯一主动推送边界。业务模块负责判断是否应该通知、生成内容快照并调用 `NotificationOutboxStore::upsert` / `cancel_by_source`；通知层只负责保存任务、按 `scheduled_at` 领取、通过 `PushSink` 投递、记录重试和终态，不反查 RSS、Todo 或未来业务表，也不重新解释业务状态。

通知任务的核心字段语义如下：

- `source_type` / `source_id`：业务来源和业务对象标识，例如 `rss`、`todo`；通知层只用于取消、查询和日志聚合。
- `dedupe_key`：业务生产者生成的稳定幂等键；同一业务事件重复提交必须命中同一键，业务确实要产生新提醒时再生成新键。
- `target`：`PushTarget { platform, account_id, target_type, target_id }`，必须由业务在创建任务时显式传入真实投递目标；`scope_key` 只能辅助继承 platform/account，不能替代 raw target。
- `channel` / `kind`：渠道族和通知类型标签，例如当前使用 `channel=push`、`kind=rss_update` / `todo_reminder` / `todo_daily_reminder`。
- `payload`：已渲染的内容快照，当前 Worker 识别 `{ message_type, text, fallback_text }`；业务内容、标题、摘要、Todo 展示格式都应在入队前确定。
- `scheduled_at`：计划投递时间；立即通知也写成当前时间附近的同一任务模型，不拆另一套立即发送系统。
- `status`：`pending -> sending -> sent` 或 `pending/sending -> retry -> failed`，业务取消走 `cancelled`；发送失败的 retry / failed 由 Worker 根据 `attempts` 和 `max_attempts` 决定。

当前落地来源包括：RSS 新条目在 `runtime/rss/scheduler.rs` 中按订阅和条目生成 `rss_update`；Todo 单次提醒在 `runtime/tools/todo/reminder.rs` 中按待办和提醒时间生成 `todo_reminder`，编辑提醒会取消旧未终结任务；Todo 每日提醒在 `runtime/tools/todo/reminder_worker.rs` 中按 owner 和日期生成 `todo_daily_reminder`，只负责每日快照入队，真实发送失败由统一 Worker 重试。`TODO_DAILY_REMINDER_ENABLED` 只控制后台调度是否启动，用户/范围级开关由私聊 `/todo daily on`、`/todo daily off` 和 `/todo daily status` 维护。

Todo 提醒当前支持分钟/小时级调度边界：自然语言可以创建“X 分钟后提醒”，重复规则可以创建“每 N 分钟提醒”或“每 N 小时提醒”。Notification Worker 只负责投递和回写 sent/retry/failed；发送成功后的 Todo 重排由 Todo reminder 侧的 sent hook 处理，它会根据 Todo 当前状态把同一个待办推进到下一次提醒，并重新写入一个新的 outbox。发送失败、Todo 已完成/非 pending、重复处理旧 outbox 或提醒时间锚点不匹配时不会重排。错过触发后采用“推进到下一次未来时间”的补偿策略，不补齐离线期间每一分钟的历史提醒。

这不是通用定时执行工具/命令能力。当前不会在每次触发时动态调用系统状态、Web Search、RSS、slash 命令或其他 Tool，也不会把模型文案当作工具执行结果。后续如需支持定时执行 Tool/命令，应单独立项设计执行模型、权限边界、限流/去重、结果投递和失败恢复。

新增业务源的最小接入方式：在业务自己的调度或写操作中确定 `source_type`、`source_id`、`dedupe_key`、`PushTarget`、`scheduled_at` 和内容快照，调用 `NotificationOutboxStore::upsert`；业务状态取消或失效时调用 `cancel_by_source`；不要修改 `NotificationWorker` 增加业务类型分支。天气预警、系统通知、定时摘要等后续来源可先复用该模型；用户级推送偏好、多渠道路由、重复提醒和历史归档只保留在 `target/channel/kind/payload` 周围扩展的边界，等真实需求出现后再实现策略中心。

### 部署管理控制台

仅当 `WEB_CONSOLE_ENABLED=true` 时注册。新实例没有 Provider 或平台凭据时进入 `setup_required`，仍保留 `/healthz` 和受保护的 `/console/`，但不构造业务 Provider、Worker 或 Gateway。首次启动会在 `config/secrets/bootstrap.token` 生成约 22 字符、短时单次的 token，并向启动控制台输出一次；普通状态请求、有效 token 复用和重启不会重复输出，初始化成功后文件立即删除。登录页也可按相同边界生成密码重置 token，提交成功后撤销全部旧 Admin 会话。

配置详情和写接口要求独立于聊天 session 的部署管理员服务端会话。密码最少 6 个字符并使用 Argon2id 哈希；登录/初始化/重置受限流、SameSite HttpOnly cookie、CSRF、会话轮换/过期和脱敏审计保护。普通字段写入受管 `runtime.toml`，secret 认证加密写入 SQLite，Agent 路线与 Tool Calling 结构化修改现有 `agent.toml`。所有修改携带 revision；冲突不会覆盖人工新文件。分步首次向导允许保存整体尚未完成的配置，但页面和 `/healthz` 会继续报告 `setup_required`，直至重启后完整预检通过。

状态接口只读取已有配置、文件元数据和进程内观测，不调用 Provider、不探测外网。配置页的“安全校验”复用正式启动预检；只有管理员显式点击“受控测试”时，服务端才会向所选 HTTPS Provider 的模型列表端点发起 8 秒内的无聊天内容请求。自定义公网地址会先拒绝环回、私网、链路本地等非公网解析结果并固定本次 DNS 地址，仍禁止重定向，避免形成任意内网 SSRF 能力。Markdown 渲染接口限制请求体 64 KiB，并使用 HTML sanitizer 清理脚本、事件属性和危险链接。

服务不会启用任意来源 CORS。`WEB_CONSOLE_ALLOWED_ORIGINS` 为空时仅同源；认证 cookie 不作为反向代理身份断言。控制台仍只适合本机或受控内网，不建议将 8787 裸露到公网；确需外部访问时必须由受信反向代理提供 TLS，并保留应用自身认证、CSRF 和权限校验。

前端源码和构建说明见 [`../web-console/README.md`](../web-console/README.md)。Rust 直接嵌入已提交的 `web-console/dist/`，普通 Cargo 构建和运行不需要 Node.js。

## 指令能力

- 会话：`/new`、`/rename`、`/resume`、`/clear`、`/state`、`/compact`、`/help`。`/list` 仅作为 deprecated 兼容别名保留，推荐 `/resume` 或 `/恢复`。
- 记忆：`/memory`、`/memory 内容`、`/memory show 1`、`/memory edit 1 新内容`、`/memory delete 1`；中文别名 `/记忆`、`/记`。
- 待办：slash 入口只保留查询（`/todo`、`/todo all`、`/todo search 关键词`、`/todo done`、`/todo undo`；中文别名 `/待办`、`/任务`），新增、完成、恢复、修改、取消和永久删除请直接用自然语言触发 Todo Tool。火车时刻请使用 `/火车 车次 [日期]` 查询。
- RSS：`/rss`、`/rss recent [数量]`、`/rss add RSS地址 [名称]`、`/rss delete 1`、`/rss test RSS地址`；中文别名 `/订阅`。
- 运维：`/ops <command> [args...]`；默认关闭，仅执行部署者在独立配置中声明的固定程序。
- 查询：`/查 关键词`、`/查询 关键词`、`/search 关键词`。
- 列车：`/火车 G1`、`/火车 G1 明天`、`/火车 G1 2026-06-28`；未提供日期时默认今天，当前只做时刻查询。
- 天气：`/天气杭州`、`/天气 杭州`、`/杭州天气`、`/weather 杭州`。
- 翻译：`/翻译 文本`、`/翻译日语 文本`、`/翻译成英语 文本`。

私聊普通聊天还可以让模型按需调用天气、列车时刻、RSS 最近条目、联网搜索和 Todo Tool，例如“杭州明天要带伞吗”“查一下 G1 明天的时刻”“查看上次 Codex 发布的 RSS”“搜索 Rust 最新进展”“看看我还有哪些事情没做”。群聊默认只保留确定性命令和普通聊天；确需在受控群里试用时，可开启群聊 Tool Calling，但默认模型只会看到天气、列车时刻、RSS 最近条目和联网搜索工具。若要开放 `list_todos`、`get_todo` 或 Todo 写工具，必须在 `agent.toml` 的群聊 `enabled_tools` 中显式加入，并先确认群聊 Todo owner 语义符合预期。这条路径复用现有业务执行器、RSS 本地状态、TodoStore、查询执行器和 session 快照，但不替代 `/天气`、`/火车`、`/rss`、`/todo`、`/查` 等显式命令；显式命令始终优先并保持原排版和 session 行为。

待确认操作会优先于普通命令处理；跨工具通用 envelope 和确认分类复用 `runtime/pending/`，Todo 专属 pending payload、确认/澄清状态机和用户文案维护在 `runtime/tools/todo/`，`runtime/respond/pending.rs` 只保留会话写入 helper。

## 配置和数据

本模块从进程环境变量读取配置。`make run` 和部署控制脚本都会以 `runtime/` 为工作目录启动统一程序，因此默认会依次尝试加载：

```text
runtime/config/.env
runtime/.env
```

`dotenvy` 默认不覆盖已存在的环境变量：进程环境变量优先，先加载的 dotenv 文件会保留同名变量，后续文件只补充缺失项。

常用配置项：

- `config/agent.toml` / `AGENT_CONFIG_FILE`：非敏感 Agent 场景策略文件，也是模型路线、搜索路线、Profile、Scene、Tool Calling 和工具白名单的唯一权威来源。文件统一描述 `fast / balanced / deep` 档位、群聊 / 私聊策略、Tool Loop 轮数、输出预算、工具白名单、`/查` 搜索路线和 OpenAI-compatible provider 元数据；标题、Memory、压缩、翻译等内部任务使用当前场景 Profile 的 `aux_route`，未配置时继承 `main_route`。文件缺失、引用非法或内容不完整都会拒绝启动，不再从环境变量合成兼容策略。
- `OPENAI_API_KEY`、`OPENAI_BASE_URLS`、`OPENAI_API_MODE`、`DEEPSEEK_API_KEY`、`DEEPSEEK_BASE_URL`、`BIGMODEL_API_KEY`、`BIGMODEL_BASE_URL`、`GEMINI_API_KEY`、`GEMINI_BASE_URL`、`MIMO_API_KEY`：Provider 凭证和连接配置；Core 解析后传给 `qq-maid-llm`。`OPENAI_BASE_URLS` 为逗号分隔时取第一个非空地址。`OPENAI_API_MODE=auto` 优先 Responses API 并在可恢复错误时降级 Chat Completions；`chat_only` 仅用于只实现 Chat Completions 的网关。MiMo 等自定义 Provider 的公开连接元数据可在 `agent.toml [providers.*]` 声明，真实 key 只能由 `api_key_env` 指向环境变量，不能写入 `agent.toml`。
- `LLM_SERVER_HOST`、`LLM_SERVER_PORT`、`LLM_REQUEST_TIMEOUT_SECONDS`：外部健康 / 控制台 HTTP 服务和请求超时行为；`AGENT_FINALIZATION_RESERVE_SECONDS` 为最终无工具回答预留时间，短请求会按总预算裁剪。
- `WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT_SECONDS`、`WEB_SEARCH_IDLE_TIMEOUT_SECONDS`、`WEB_SEARCH_ABSOLUTE_TIMEOUT_SECONDS`：Agent Tool 与 `/查` 共用的搜索流首活动、静默和独立绝对超时，默认分别为 60、30、120 秒，以兼容联网搜索首字较慢或总耗时较长的上游。
- `WEB_CONSOLE_ENABLED`、`WEB_CONSOLE_ALLOWED_ORIGINS`：部署管理控制台和跨域 allowlist；新实例默认开启但监听默认仍为 `127.0.0.1`，显式设为 false 时页面、认证和配置 API 全部关闭。
- `APP_DB_FILE`：统一 SQLite 文件，承载业务数据和知识检索索引。
- `QQ_MAID_DB_POOL_MAX_SIZE`：本地 SQLite 连接池大小，默认 8，合法范围 1～32；独立于 `MAX_CONCURRENT_RESPONSES`。
- `MEMORY_CONSOLIDATION_*`：确定性长期记忆整理开关与时间、数量、来源、单次记录数和字符门槛；默认关闭，只归档同一完整作用域内正文与语义键完全相同的重复项，不读取聊天正文、不调用模型。
- `MEMORY_DREAM_*`：独立于确定性整理的 Session Dream 开关与时间、新 Session 数、单批 Session 数、输入字符软上限和输出候选数门槛；默认关闭，只读取带稳定消息 ID 的活跃及归档用户消息，不读取 Session Summary。
- `PROMPT_DIR`：固定 prompt 目录。
- `KNOWLEDGE_DIR`：Markdown 知识目录；留空时使用 `config/knowledge`，启动时自动同步到 SQLite FTS5，普通聊天按需检索片段。
- `RSS_*`：RSS / Atom 轮询、去重、推送和 SSRF 防护相关配置。
- `QWEATHER_API_KEY`、`QWEATHER_API_HOST`、`QWEATHER_GEO_HOST`：天气配置；`QWEATHER_API_KEY` 留空时关闭天气能力且不影响启动，Host 留空使用官方默认地址。

`agent.toml` 的模型 route 支持单候选和候选链：

```toml
[model_routes.private_main]
candidates = ["openai:gpt-5.4-mini", "deepseek:deepseek-chat"]
```

候选项按从左到右的优先级执行。`qq-maid-llm` 会在超时、HTTP/网络错误、Provider 协议错误、上游空响应、429 和 5xx 等可恢复失败后尝试下一个候选；配置错误、本地请求构造错误和业务参数错误不会继续请求其他模型。当前普通聊天使用请求开始时解析出的 `ResolvedAgentPolicy`；会话标题、Memory 草稿、会话压缩、翻译命令和 RSS 翻译使用同一场景策略中的 `aux_route`，缺省辅助路线时继承当前场景 `main_route`。Tool Loop 使用同一请求级策略中的模型、输出预算、reasoning effort 和最大轮数；`/查` 按场景 `search_route` 选择 OpenAI Responses web_search 或 Gemini Google Search 工具。

完整字段以 [runtime/config/.env.example](../runtime/config/.env.example) 为准。真实 `.env`、API Key、Prompt、Markdown 知识资料、SQLite、日志和聊天记录不要提交到仓库。

## 运行和检查

从仓库根目录执行：

```bash
cp runtime/config/.env.example runtime/config/.env
make run
```

构建统一 release 二进制：

```bash
make build
```

修改 Core 代码后至少执行：

```bash
make test-core
```

`make test-core` 会同时检查 `qq-maid-common/` 和 `qq-maid-llm/`，因为 Core 的时间上下文和模型调用边界依赖这两个 crate。

跨 Core / gateway、提交前或涉及 workspace 依赖时执行：

```bash
make test
```

只修改本文档时，至少执行 `git diff --check` 并人工核对链接、命令和敏感信息。
