# qq-maid-core — Rust Core 模块

`qq-maid-core/` 是 QQ Maid Bot 的核心业务模块，负责 `CoreService`、普通聊天、联网查询命令、列车时刻查询、天气、翻译、会话、长期记忆、Todo、RSS / Atom 订阅、业务 Tool 和业务 prompt 组装。模型协议、Provider 路由、fallback、SSE、usage、健康观测、OpenAI Web Search 传输和 Tool Loop 协议由 `qq-maid-llm/` 承载。

QQ 平台事件解析、白名单、`/ping` 本地诊断和消息回发不在本模块处理，相关实现见 [qq-maid-gateway-rs/README.md](../qq-maid-gateway-rs/README.md)。运行目录、私有配置、部署产物和数据文件说明见 [runtime/README.md](../runtime/README.md)。

## 当前范围

- HTTP 层默认只公开进程级 `GET /healthz`；本地 Web 控制台默认关闭，启用后才注册 `/console/` 和 `/api/v1/markdown/render`。
- 普通聊天、查询、列车时刻、天气、翻译、会话命令、长期记忆、Todo、RSS 指令和业务 Tool 都通过 `CoreService::respond` 进程内分发。
- Session、Todo、长期记忆、RSS / Atom 订阅、RSS 去重状态和知识检索索引统一写入 `APP_DB_FILE` 指向的 SQLite。
- 长期记忆只能通过明确 `/memory`、`/记忆`、`/记` 指令生成草稿，用户确认后写入；普通聊天不会自动写长期记忆。
- RSS 后台轮询由本模块调度，主动推送通过进程内 `PushSink` 交给 gateway 发送。
- OpenAI / DeepSeek、模型候选链 fallback、Web Search 传输、Tool Loop 协议和上游健康观测由 `qq-maid-llm` 提供，Core 只保留业务调用边界、Tool 注册与兼容 re-export。

当前 Tool Calling 仍只在私聊普通聊天中默认启用，已注册天气和 Todo 业务 Tool；群聊 Tool Calling 由 `TOOL_CALLING_GROUP_ENABLED` 显式开启，默认关闭以避免工具调用阻塞群聊回复。slash 命令、pending 确认、文件处理和宿主机代码执行不进入 Tool Loop。最终目标是参考 Codex 的受控工具调用体验，但新增工具必须先经过白名单、权限、超时和输出大小限制。

旧 HTTP `/query`、HTTP `/memory`、`/v1/chat` 等入口不再公开，也不要重新引入 Python LLM、Python 查询、Python 记忆或 Python fallback 入口。

## 模块结构

```text
qq-maid-core/src/
├── app/                 # 启动、dotenv 加载、日志、组件装配
├── config.rs            # 环境变量解析和默认值
├── http/                # /healthz、控制台和 Markdown render
├── service.rs           # CoreService / CoreHandle 进程内契约
├── provider/            # qq-maid-llm provider-facing 类型的兼容 re-export
├── runtime/
│   ├── respond/         # CoreService 后的 chat/search/weather/todo/memory/session flow
│   ├── pending/         # 待确认操作类型和确认分类
│   ├── query/           # qq-maid-llm Web Search 执行器的兼容 facade
│   ├── rss/             # RSS / Atom 拉取、存储封装、调度和 PushSink
│   ├── prompt/          # 固定 prompt 加载
│   ├── knowledge/       # Markdown 知识目录扫描、分段和检索上下文
│   ├── session.rs       # 会话领域逻辑
│   ├── memory.rs        # 长期记忆领域逻辑
│   ├── todo.rs          # Todo 领域逻辑
│   ├── tools/           # Core 业务 Tool 适配层，注册天气和 Todo Tool
│   ├── train/           # 列车时刻查询执行器
│   └── weather/         # 天气执行器
├── storage/             # SQLite、migration、session/memory/todo/rss/knowledge 持久化
└── util/                # 指标，以及 time_context 兼容 re-export
```

`runtime/respond.rs` 是 `CoreService::respond` 后的统一业务入口；具体 flow 在 `runtime/respond/` 下维护。`runtime/tools/` 只负责把现有业务执行器包装成模型可调用 Tool，不加载 Skill 文件，也不把业务逻辑迁入 `qq-maid-llm`。通用日期、时间和时区语义优先复用 `qq-maid-common/src/time_context/`；Core 内部可继续通过 `util/time_context.rs` 兼容入口引用，不要在具体命令里重复实现。

## HTTP 接口

### `GET /healthz`

返回进程级健康状态、当前 provider、模型、流式配置和当前进程内最近一次真实上游调用的脱敏快照，供控制脚本和诊断脚本探测。Gateway `/ping` 直接读取 `CoreService::health_snapshot()`，不通过 HTTP。进程启动后尚无调用时，上游状态为 `unverified`；进程重启后不会沿用旧配置下的状态。

### CoreService

Gateway 调用 Core 的唯一业务入口是 `CoreService::respond(CoreRequest)`。Gateway 只传入最终拼接后的文本、平台、成员身份和私聊 / 群聊目标；`scope_key` 由 Core 根据目标派生。私聊普通聊天在 `TOOL_CALLING_ENABLED=true` 时可进入 Tool Loop；群聊普通聊天还需要 `TOOL_CALLING_GROUP_ENABLED=true`。有 slash 前缀的命令和 pending 确认流程继续走既有分支。`/ping check` 调用 `CoreService::upstream_check()`，该分支不进入 respond 业务 flow，不创建 session，也不触发标题、记忆、Todo、查询或 Tool Calling。

### `GET /console/` 与 `POST /api/v1/markdown/render`

仅当 `WEB_CONSOLE_ENABLED=true` 时注册。控制台用于本机 Markdown 预览，默认不暴露；Markdown 渲染接口限制请求体 64 KiB，并使用 HTML sanitizer 清理脚本、事件属性和危险链接。

服务不会启用任意来源 CORS。`WEB_CONSOLE_ALLOWED_ORIGINS` 为空时仅同源；如确需跨域访问，必须显式配置 allowlist。生产外网暴露控制台时应由反向代理或外部网关增加认证和访问控制。

## 指令能力

- 会话：`/new`、`/rename`、`/resume`、`/clear`、`/state`、`/compact`、`/help`。`/list` 仅作为 deprecated 兼容别名保留，推荐 `/resume` 或 `/恢复`。
- 记忆：`/memory`、`/memory 内容`、`/memory show 1`、`/memory edit 1 新内容`、`/memory delete 1`；中文别名 `/记忆`、`/记`。
- 待办：slash 入口只保留查询（`/todo`、`/todo all`、`/todo search 关键词`、`/todo done`、`/todo undo`；中文别名 `/待办`、`/任务`），新增、完成、恢复、修改、取消和永久删除请直接用自然语言触发 Todo Tool。火车时刻请使用 `/火车 车次 [日期]` 查询。
- RSS：`/rss`、`/rss add RSS地址 [名称]`、`/rss delete 1`、`/rss test RSS地址`；中文别名 `/订阅`。
- 查询：`/查 关键词`、`/查询 关键词`、`/search 关键词`。
- 列车：`/火车 G1`、`/火车 G1 明天`、`/火车 G1 2026-06-28`；未提供日期时默认今天，当前只做时刻查询。
- 天气：`/天气杭州`、`/天气 杭州`、`/杭州天气`、`/weather 杭州`。
- 翻译：`/翻译 文本`、`/翻译日语 文本`、`/翻译成英语 文本`。

私聊普通聊天还可以让模型按需调用天气和 Todo Tool，例如“杭州明天要带伞吗”“看看我还有哪些事情没做”。群聊默认只保留确定性命令和普通聊天，不执行 Todo 写入等工具操作；确需在受控群里试用时，可配置 `TOOL_CALLING_GROUP_ENABLED=true`。这条路径复用现有业务执行器、TodoStore、session 快照和 pending 机制，但不替代 `/天气`、`/todo` 等显式命令；显式命令始终优先并保持原排版和 session 行为。

待确认操作会优先于普通命令处理；若修改 pending、确认分类或 todo / memory 的状态转换，优先复用 `runtime/pending/` 和 `runtime/respond/pending.rs` 中的既有逻辑。

## 配置和数据

本模块从进程环境变量读取配置。`make run` 和部署控制脚本都会以 `runtime/` 为工作目录启动统一程序，因此默认会依次尝试加载：

```text
runtime/config/.env
runtime/.env
```

`dotenvy` 默认不覆盖已存在的环境变量：进程环境变量优先，先加载的 dotenv 文件会保留同名变量，后续文件只补充缺失项。

常用配置项：

- `LLM_PROVIDER`：`openai` / `deepseek` / `bigmodel` / `auto`；`auto` 会按模型候选链中的 provider 前缀路由。
- `config/agent.toml` / `AGENT_CONFIG_FILE`：非敏感 Agent 场景策略文件，统一描述 `fast / balanced / deep` 档位、群聊 / 私聊 profile、Tool Loop 轮数、输出预算和 `/查` 搜索路线名称。默认模板已显式声明 `private_main`、`group_main` 和 `aux`；未显式声明的路线才会从环境变量生成的内置路线继承具体模型。显式设置 `AGENT_CONFIG_FILE` 但文件缺失会启动失败；默认文件缺失时回退旧环境变量兼容路径。
- `LLM_MODEL`、`PRIVATE_LLM_MODEL`、`GROUP_LLM_MODEL`、`TITLE_MODEL`、`MEMORY_MODEL`、`COMPACT_MODEL`、`TRANSLATION_MODEL`：主模型、场景模型和内部任务模型；`TRANSLATION_MODEL` 供 `/翻译` 和 RSS 推送前翻译共用，留空时沿用主模型。`LLM_MODEL` 仍作为主路线兼容默认，`PRIVATE_LLM_MODEL` / `GROUP_LLM_MODEL` 优先覆盖对应场景；`agent.toml` 显式声明同名 `[model_routes.*]` 时再覆盖环境继承路线。`TODO_MODEL` 已不再用于 slash 待办解析，Todo 写操作统一走 Tool Calling。
- `OPENAI_API_KEY`、`OPENAI_BASE_URL`、`OPENAI_BASE_URLS`、`OPENAI_API_MODE`、`DEEPSEEK_API_KEY`、`DEEPSEEK_BASE_URL`、`DEEPSEEK_MODEL`、`BIGMODEL_API_KEY`、`BIGMODEL_BASE_URL`、`BIGMODEL_MODEL`：provider 配置；Core 解析后传给 `qq-maid-llm`。`OPENAI_BASE_URLS` 为逗号分隔时取第一个非空地址，优先于 `OPENAI_BASE_URL`。`OPENAI_API_MODE=auto` 优先 Responses API 并在可恢复错误时降级 Chat Completions；`chat_only` 仅用于普通聊天兼容只实现 Chat Completions 的网关，不会请求 `/v1/responses`。
- `LLM_SERVER_HOST`、`LLM_SERVER_PORT`、`LLM_REQUEST_TIMEOUT_SECONDS`：外部健康 / 控制台 HTTP 服务和请求超时行为。
- `TOOL_CALLING_ENABLED`、`TOOL_CALLING_GROUP_ENABLED`、`TOOL_CALLING_MAX_ROUNDS`：旧兼容开关。存在 `agent.toml` 时，请优先使用 `[scenes.*].tool_calling_enabled` 和 profile 的 `max_tool_rounds`；群聊默认仍不会进入 Tool Loop。该能力依赖 provider Tool Calling 能力，`OPENAI_API_MODE=chat_only` 时 OpenAI Responses 原生 Tool Loop 不会执行。
- `WEB_CONSOLE_ENABLED`、`WEB_CONSOLE_ALLOWED_ORIGINS`：本地控制台和跨域 allowlist；默认关闭且不允许任意来源。
- `APP_DB_FILE`：统一 SQLite 文件，承载业务数据和知识检索索引。
- `PROMPT_DIR`：固定 prompt 目录。
- `KNOWLEDGE_DIR`：Markdown 知识目录；留空时使用 `config/knowledge`，启动时自动同步到 SQLite FTS5，普通聊天按需检索片段。
- `RSS_*`：RSS / Atom 轮询、去重、推送和 SSRF 防护相关配置。
- `OPENAI_SEARCH_MODEL`、`PRIVATE_OPENAI_SEARCH_MODEL`、`GROUP_OPENAI_SEARCH_MODEL`：旧兼容联网查询模型配置；存在 `agent.toml` 时 `/查` 按群聊 / 私聊场景解析 `search_route`，默认 `private_search` / `group_search` 仍从这些环境变量继承。`SEARCH_CONTEXT_SIZE`、`SEARCH_MAX_RESULTS` 当前没有环境变量入口，`/查` flow 使用查询模块默认值。
- `QWEATHER_API_KEY`、`QWEATHER_API_HOST`、`QWEATHER_GEO_HOST`：天气配置；当前 `QWEATHER_API_KEY` 为必需项。

模型配置支持单模型和候选链两种写法：

```env
LLM_MODEL=openai:gpt-5.4-mini
LLM_MODEL=bigmodel:glm-5.2,deepseek:deepseek-chat
```

候选项按从左到右的优先级执行，候选项前后的空格会被忽略。`qq-maid-llm` 会在超时、HTTP/网络错误、provider 协议错误、上游空响应等可恢复失败后尝试下一个候选；配置错误、本地请求构造错误和业务参数错误不会继续请求其他模型。OpenAI provider 内部在 `OPENAI_API_MODE=auto` 时仍先完成 Responses API、空流补非流和 Chat Completions 兼容 fallback，只有该候选整体失败后才进入下一个候选；`chat_only` 时直接使用 Chat Completions。DeepSeek 和 BigModel 均复用 OpenAI 兼容 Chat Completions adapter，但使用各自独立的 key、base URL 和模型前缀。当前普通聊天使用请求开始时解析出的 `ResolvedAgentPolicy`；标题、Memory 草稿、会话压缩、翻译和 RSS 翻译继续使用各自可选模型，未配置时沿用主 provider。Tool Loop 使用同一请求级策略中的模型、输出预算、reasoning effort 和最大轮数；Provider 不支持 reasoning 时会忽略该参数而不是在业务层伪装生效。`/查` 仍走 OpenAI Responses web_search 协议，但模型由场景 `search_route` 或旧 `OPENAI_SEARCH_MODEL` 兼容路径决定。

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
