# 小女仆机器人 开发维护文档

本文面向项目开发者和维护者，保留仓库级架构边界、开发命令、维护约定和检查规则。运行目录、部署、私有配置和运行数据细节已经分流到 [runtime/README.md](../runtime/README.md)；QQ 官方 gateway 细节见 [qq-maid-gateway-rs/README.md](../qq-maid-gateway-rs/README.md)；Rust Core 模块细节见 [qq-maid-core/README.md](../qq-maid-core/README.md)。

如果只是第一次了解项目，请先阅读 [README.md](../README.md)。

## 架构边界

- `qq-maid-gateway-rs/`：QQ 官方 C2C / 群 at 和可选微信服务号接入层，负责平台事件接收、统一入站转换、`/ping` 诊断、回复发送和主动推送出口。
- `qq-maid-core/`：Rust Core / 查询 / 记忆 / session / prompt / 业务 Tool 模块，通过 `CoreService` 提供进程内业务入口；HTTP 层固定公开 `GET /healthz`，启用只读控制台时再公开对应管理路由。
- `qq-maid-llm/`：模型协议、Provider 路由、fallback、SSE、usage、健康观测、OpenAI Web Search 和模型原生 Tool Loop 基础设施。
- `src/main.rs`：统一 `qq-maid-bot` 程序入口，负责一次性初始化 dotenv / tracing，并按顺序拉起 Core HTTP 与 Gateway。
- `qq-maid-common/`：两个及以上 crate 共享的无业务状态基础工具，目前承载身份上下文、输入输出结构、Markdown 安全转换、脱敏、时间和通用文本处理。
- `runtime/`：服务器部署运行目录，保留 release 二进制、运行配置和运行产物。
- `scripts/`：部署、进程控制和网络诊断脚本源码目录。
- `scripts/diagnose-network.sh`：shell 版网络诊断脚本，替代旧 Python 诊断入口。

QQ、OneBot、微信等入口接入相关能力优先在 gateway 的平台 adapter / sender 边界演进；模型协议、provider fallback 和 Tool Loop 协议优先在 `qq-maid-llm/` 演进；普通聊天、查询命令、记忆、session、待办、会话命令、prompt 和具体业务 Tool 等业务逻辑优先在 `qq-maid-core/` 内部维护。

多平台入口维护时必须区分三类 ID：平台原始 ID 是 `ReplyTarget` / `DeliveryTarget` 的真实投递目标；`scope_key` / `owner_key` 是 Session、Pending、Memory、Todo 的业务隔离键；Core、LLM 和 Tool Loop 不应理解 QQ、OneBot 或微信协议字段。RSS、Notification、Todo 提醒和 Push 需要保留平台原始发送目标，不允许发送逻辑从 `scope_key` / `owner_key` 反解析 raw target。conversation / actor / interaction / owner / delivery target 的术语边界见 [scope-identity-boundary.md](./design/scope-identity-boundary.md)。

## 项目结构

```text
.
├── Cargo.toml
├── Cargo.lock
├── Makefile
├── AGENTS.md
├── README.md
├── docs/
│   ├── DEVELOPMENT.md
│   ├── development/
│   │   └── custom-tools.md
│   ├── design/
│   └── tasks/
├── LICENSE
├── scripts/
│   ├── deploy-remote.sh
│   ├── deploy-local.sh
│   ├── qbot.sh
│   ├── qbot.ps1
│   ├── qbot.cmd
│   ├── sync_knowledge.sh
│   ├── deploy.conf.example
│   ├── diagnose-network.sh
│   └── botctl.sh
├── runtime/
│   ├── README.md
│   └── config/
│       ├── .env.example
│       └── agent.toml
├── web-console/
│   ├── src/
│   ├── dist/
│   └── README.md
├── src/
│   └── main.rs
├── qq-maid-common/
│   └── src/
├── qq-maid-llm/
│   └── src/
├── qq-maid-core/
│   ├── src/
│   └── README.md
└── qq-maid-gateway-rs/
    ├── src/
    │   ├── app/
    │   ├── config/
    │   └── gateway/
    └── README.md
```

Rust 构建由仓库根目录统一管理：根包产出唯一主可执行文件 `qq-maid-bot`，workspace 成员为 `qq-maid-common/`、`qq-maid-llm/`、`qq-maid-gateway-rs/` 和 `qq-maid-core/`；统一锁文件位于根目录 `Cargo.lock`，release 产物位于根目录 `target/release/`。

不要恢复子目录 `Cargo.lock`，也不要在文档或脚本中继续引用 `qq-maid-*/target/` 旧路径。

## 本地启动（开发调试）

开发调试时，以前台方式启动方便直接观察输出：

```bash
make run
```

首次使用需先完成配置，详见 [README.md 快速开始](../README.md#快速开始) 和 [runtime/README.md](../runtime/README.md)。

## 文档分工

- [README.md](../README.md)：项目定位、核心能力、快速开始和用户可见指令示例。
- [qq-maid-core/README.md](../qq-maid-core/README.md)：Rust Core 模块边界、HTTP facade、指令 flow、配置项和检查方式。
- [qq-maid-gateway-rs/README.md](../qq-maid-gateway-rs/README.md)：QQ 官方 gateway、事件范围、消息发送、日志、`/ping` 和进程内主动推送。
- [runtime/README.md](../runtime/README.md)：运行目录、部署产物、真实配置、路径解析、运行数据、控制脚本和诊断。
- [runtime/config/.env.example](../runtime/config/.env.example)：环境变量模板和字段说明。
- [custom-tools.md](./development/custom-tools.md)：自定义 Tool 的注册、场景白名单、领域后处理和安全要求。
- [web-console/README.md](../web-console/README.md)：只读管理面板的 TypeScript 源码、可复现构建与嵌入产物约定。
- [response-event-runtime.md](./design/response-event-runtime.md)：统一响应事件流的现状基线、事件模型和分阶段迁移边界。

## 常用命令

```bash
make run
make test
make test-common
make test-llm
make test-core
make test-gateway
make build
make deploy-local
make deploy-remote
make status
make diagnose
scripts/validate-runtime.sh check
scripts/validate-runtime.sh glm
scripts/validate-runtime.sh console
scripts/validate-runtime.sh restart-source
make clean
```

- `make run`：启动统一 `qq-maid-bot`，内部先启动 Core HTTP，再启动 QQ Gateway。
- `make test`：执行根目录 Cargo Workspace 的 fmt、test 和 check。
- `make test-common`：执行 Rust common fmt check、测试和 `cargo check`。
- `make test-llm`：执行 Rust common 与 Rust LLM fmt check、测试和 `cargo check`。
- `make test-core`：执行 Rust common、Rust LLM 与 Rust Core fmt check、测试和 `cargo check`。
- `make test-gateway`：执行 Rust common 与 Rust gateway fmt check、测试和 `cargo check`。
- `make build`：构建统一 `qq-maid-bot` release 二进制。
- `make deploy-local`：执行 `scripts/deploy-local.sh`，构建并安装到本地 `runtime/`。
- `powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1`：在 Windows 原生环境执行 workspace release 构建，并把 exe 与 Windows 控制脚本安装到 `runtime\`。
- `make deploy-remote`：执行 `scripts/deploy-remote.sh`，构建并发布 release 二进制到 `scripts/deploy.conf` 配置的远端运行目录。
- `make diagnose`：运行 shell 网络诊断，检查配置文件存在性、代理、公网出口 IP 和 Core `/healthz`。
- `scripts/validate-runtime.sh check`：检查运行中统一服务状态、GLM 上游、Web 控制台和最近日志。
- `scripts/validate-runtime.sh glm`：只验证 GLM / OpenAI 兼容 key 和模型调用。
- `scripts/validate-runtime.sh console`：只验证 Web 控制台 `/console/`。
- `scripts/validate-runtime.sh restart-source`：用 `target/debug/qq-maid-bot` 临时验证当前源码统一程序。
- `bash scripts/sync_knowledge.sh`：以镜像语义把本地知识库 Markdown 同步到 `scripts/deploy.conf` 配置的远端服务器。本地删除或重命名的 `.md` 会从对应远端子目录中删除，删除范围仅限该子目录内部；非 `.md` 文件不会被传输或删除。
- `bash scripts/sync_knowledge.sh --dry-run`：仅预览同步差异（包含将被删除的远端 `.md`），不实际传输或删除。远端知识库根目录由 `deploy.conf` 的 `REMOTE_KNOWLEDGE_DIR` 控制；未设置时优先使用 `${REMOTE_APP_DIR}/config/knowledge`，未配置独立应用根目录时兼容 `${REMOTE_PROJECT_DIR}/runtime/config/knowledge`。
- `make clean`：清理根目录 Cargo Workspace 的构建产物。

## HTTP 与命令入口

Rust HTTP 层只公开外部运维 / 管理能力：

- 始终公开：`GET /healthz`。
- 仅在 `WEB_CONSOLE_ENABLED=true` 时公开：`GET /console/`、`GET /console/{asset}`、`GET /api/v1/console/status` 和 `POST /api/v1/markdown/render`；Markdown 预览路由同时处理 CORS preflight。

旧 HTTP 路由 `/query`、HTTP `/memory`、`/v1/chat` 和内部 respond 主入口不再公开。查询、记忆、待办、会话和 RSS 都通过 `CoreService::respond` 进程内命令流程承载。

当前常用 slash 指令：

- 会话：`/new`、`/rename`、`/resume`、`/clear`、`/state`、`/compact`、`/help`。`/list` 仍作为 deprecated 兼容别名保留，推荐使用 `/resume` 或 `/恢复`。
- 记忆：`/memory`、`/memory 记忆内容`、`/memory personal|profile 内容`、`/memory group`、`/memory group 关键词`、`/memory group list 关键词`、`/memory group add 内容`、`/memory show 1`、`/memory edit 1 新内容`、`/memory delete 1`；中文别名 `/记忆`、`/记`。群公共记忆保留旧关键词搜索语义，只有显式 `group add` 写入；新增记忆校验通过后直接写入，清空和画像停用等破坏性操作继续确认。
- 待办：slash 入口只保留查询（`/todo`、`/todo all`、`/todo search 关键词`、`/todo done`、`/todo undo`；中文别名 `/待办`、`/任务`）。新增、完成、恢复、修改、取消和永久删除请直接用自然语言触发 Todo Tool；按编号继续操作依赖最近一次用户可见列表快照。
- RSS：`/rss`、`/rss recent [数量]`、`/rss add RSS地址 [名称]`、`/rss delete 1`、`/rss test RSS地址`；中文别名 `/订阅`。
- 查询：`/查 关键词`、`/查询 关键词`、`/search 关键词`。中文紧凑写法如 `/查今天新闻` 也会进入联网查询。
- 天气：`/天气杭州`、`/天气 杭州`、`/杭州天气`、`/weather 杭州`。
- 翻译：`/翻译 文本`、`/翻译日语 文本`、`/翻译成英语 文本`。

## 维护约定

- 默认做小改动，保持用户可见行为稳定。
- 新增或调整 QQ、OneBot、微信等平台接入、事件处理和发送逻辑时，优先修改 `qq-maid-gateway-rs/` 的 adapter / sender 边界。
- 修改模型协议、Provider 路由、fallback、SSE、usage、健康观测、OpenAI Web Search 传输或 Tool Loop 协议时，优先修改 `qq-maid-llm/`。
- Gateway 内部继续保持分层边界：`gateway/mod.rs` 负责顶层编排，`gateway/platform/` 负责平台协议到 `InboundMessage` / `CoreRequest` 的映射，`gateway/protocol.rs` 负责 WebSocket 协议与事件分发，`gateway/outbound.rs` 负责出站投递能力和发送状态记录，`respond.rs` 负责 CoreService 进程内桥接；不要把这些职责重新混回单个超长文件。
- 修改普通聊天、查询命令、记忆、session、待办、会话命令、prompt 或具体业务 Tool 时，优先修改 `qq-maid-core/`。
- Rust HTTP 层只公开 `GET /healthz`，以及启用控制台时的 `/console/`、静态资源、`/api/v1/console/status` 和 `/api/v1/markdown/render`；不要重新公开 `/query`、HTTP `/memory`、`/v1/chat` 或内部 respond 主入口。
- 通用日期、时间和时区语义优先复用 `qq-maid-common/src/time_context/`；跨 crate 的身份上下文、输入输出结构、Markdown 转换和脱敏也应优先复用 common 现有模块，不要在 Core 或 Gateway 重复实现。
- Tool Calling 的最终目标参考 Codex 的受控工具调用体验，但本项目必须保持 QQ 场景边界：私聊优先、群聊谨慎、工具白名单、权限校验、超时和输出大小限制不可省略。
- 自定义业务 Tool 的二开步骤见 [custom-tools.md](./development/custom-tools.md)，包括新增文件、注册、`agent.toml` 白名单和测试要求。
- 未来目标是支持多入口的通用机器人；不要把具体人设、群聊内容、真实用户信息或业务材料写死进代码。

## Agent Chat 与 Tool 边界

普通纯文本消息在场景、Provider 能力、群聊开关和工具白名单允许时统一进入 Agent Chat。模型可在同一次原生响应中直接回答、请求澄清或发出 Tool Call；关键词分类只能提供状态提示和 diagnostics，不能决定模型是否看到工具。工具域业务判断必须收敛到 `qq-maid-core/src/runtime/tools/`，`runtime/respond/` 不直接理解 Todo、RSS、天气、火车、搜索等业务域的工具结果。

Agent Runtime 的成功与失败共享同一份 `AgentRunDiagnostics`。成功时由 `ChatOutcome.agent` 返回；超时、取消、最大轮次、Provider 中断或 progress receiver 关闭时由 `LlmError.agent` 返回，调用方仍必须按 `Result::Err` 处理，不能把 diagnostics 当成成功回复。`model_rounds` 的固定语义是“整次请求已发起的模型请求次数”：跨候选累计，首轮为 1，最终超时或取消的在途请求也计入；它不是零基循环序号，也不是工具调用轮数。模型发出的工具、已启动工具和可信工具结果同样跨候选累计；单个候选 attempt 只拥有临时终止态，新候选开始时会清理上一候选的 `Failed` 等状态，请求级 Timeout/Cancelled 不会被后续清理或候选失败覆盖。

统一轨迹至少包含模型轮次、模型发出的工具名、是否进入工具校验/执行、实际执行工具、可信工具结果、Agent 流式回退状态和 `AgentStopReason`。Core 成功 diagnostics 直接消费该结构；失败事件通过 `CoreRespondFailure.agent` 保留结构化轨迹，日志只记录 stop reason、轮次、工具名和脱敏错误摘要，不输出原始工具结果。`AgentRunHandle` 只负责在 Runtime 与 Core 外层超时/receiver 生命周期之间共享这份轨迹和取消信号，不是第二套结果模型。

取消后不得开始新的工具副作用：Runtime 在发起每次模型请求和执行每个已校验工具前检查取消信号。已经开始的单个工具调用先写入 `executed_tools` 和 `tools_with_unknown_result`，并允许在工具自身 timeout 范围内返回真实结果；结果可信后移出 unknown 集合并写入 `tool_results`。Core 整体超时会立即设置请求级 Timeout，再使用单项工具 timeout 作为受控清理预算；预算耗尽会终止本地任务并保留 unknown 状态，不会继续下一项工具或下一轮模型请求。候选模型失败时，只要已经开始工具副作用（包括结果未知），就不再跨候选重跑 Agent，以免重复执行。

- `runtime/respond/agent_route.rs`：Agent Runtime 能力入口，只用场景策略、Provider 能力和工具白名单等确定性条件决定是否暴露工具，并返回 `AgentRouteDecision`；不读取业务交互状态，也不生成状态提示。
- `runtime/tools/status_classifier.rs`：聚合各业务域的轻量状态语义，只生成用户状态提示和 diagnostics；不得参与 Tool Schema、白名单或执行路径决策。
- `runtime/tools/todo/route.rs`：Todo / Reminder 自然语言语义门面，只为状态提示和 diagnostics 提供候选 domain/action，不解析最终目标、编号、日期或写入语义。
- `runtime/tools/<domain>/`：具体业务域的关键词、状态字段、成功判断、失败文案、可见实体、pending、owner 和持久化不变量都应放在这里。

并发限制保持两套独立资源边界：LLM 与 `/查` 共用 `MAX_CONCURRENT_RESPONSES` semaphore；SQLite 使用独立的 `QQ_MAID_DB_POOL_MAX_SIZE` 连接池。Agent Runtime 的 diagnostics/cancel handle 不占用新的全局 permit，也不会把 SQL 与 LLM 串行化。

Tool Loop 执行完成后的整轮后处理也在 tools 层：

- `runtime/tools/agent_turn.rs`：整轮后处理抽象调度入口，负责选择 conversation / interaction session、合并各 domain 的 `ToolExecutionOutcome`、套用可信展示、汇总诊断并返回通用 `AgentTurnOutcome`。
- `runtime/tools/todo/agent_turn.rs`：Todo 接入通用后处理的 domain adapter，负责调用领域聚合、产出可见实体快照，并提供成功验真和诊断适配。
- `runtime/tools/todo/receipt.rs`：Todo Tool 结果聚合、状态判断和确定性回执；`runtime/tools/todo/flow/receipt.rs` 只保留 slash / pending flow 所需的薄适配。
- `runtime/tools/todo/visible_entity.rs`：Todo 可见编号与最近操作对象快照，负责把用户看到的编号绑定到真实对象，同时保持 scope / owner / actor 隔离。
- `runtime/tools/agent_presenters.rs`：暂时承载非 Todo 工具的确定性展示 adapter。后续某个工具域出现持久化、确认、可见实体、主动通知或复杂诊断时，应从这里拆到独立 `runtime/tools/<domain>/agent_turn.rs`。
- `runtime/freshness.rs`：跨业务复用的新鲜度 helper。Todo 查询快照的新鲜度判断在 `runtime/tools/todo/freshness.rs`，不要把 Todo 专属判断放回 storage/session。

新增或调整工具域时，先判断它属于哪一类接入：

- 只读、无持久化、无后续引用的工具：可以先注册 Tool，并在 `tools/agent_presenters.rs` 增加确定性展示。
- 有自然语言状态提示：在 `tools/<domain>/route.rs` 提供 domain 门面，再由 `tools/status_classifier.rs` 聚合；不得把分类结果用于工具能力决策。
- 有用户可继续引用的对象：实现 visible entity 快照和同 scope / owner / actor 隔离。
- 有多步写入或跨存储副作用：把领域操作收敛到 `tools/<domain>/ops.rs`，Tool 入口只做参数解析、上下文校验和结构化结果返回。
- 有确认或澄清：领域 payload、状态机和恢复执行放在 `tools/<domain>/pending.rs` 或 `tools/<domain>/flow/pending.rs`；`respond/pending.rs` 只保留跨域 envelope / 会话写入 helper。
- 有写入、删除、主动通知、可信回执或复杂诊断：实现 `tools/<domain>/agent_turn.rs` 和必要的领域聚合模块，让 `tools/agent_turn.rs` 只消费抽象 outcome / diagnostics。

## 修改后检查

修改代码后，根据影响范围执行：

```bash
make test-core
make test-gateway
make test
```

- 只影响 Rust Core：至少执行 `make test-core`。
- 只影响 Rust gateway：至少执行 `make test-gateway`。
- 只影响 Rust common：至少执行 `make test-common`；涉及调用方时再执行 `make test-core` 或 `make test-gateway`。
- 只影响 Rust LLM：至少执行 `make test-llm`；涉及 Core 调用方或 Tool Loop 入口时再执行 `make test-core`。
- 跨 Core / gateway 或提交前：执行 `make test`。
- 涉及启动、依赖、环境变量、QQ 事件或模型调用：除测试外还应运行 `scripts/validate-runtime.sh check`。
- 涉及 GLM / OpenAI 兼容 key、模型候选链或 `OPENAI_API_MODE=chat_only`：运行 `scripts/validate-runtime.sh glm`。
- 涉及 Web 控制台或 Markdown 预览接口：运行 `scripts/validate-runtime.sh console`，必要时人工访问 `/console/`。
- 修改 `web-console/src/` 后，在该目录执行 `npm ci`、`npm run check`、`npm run build`，并确认 `git diff --exit-code -- web-console/dist` 无差异。`src/` 是人工源码，`dist/` 是提交到仓库并由 Rust 嵌入的可复现产物；普通 Cargo 构建不运行 npm，也不需要 Node.js。
- 涉及统一入口、启动顺序或未提交源码验证：先执行 `cargo build -p qq-maid-bot`，再运行 `scripts/validate-runtime.sh restart-source`。
- 涉及网络、代理或 QQ 后台白名单问题：运行 `make diagnose`。
- 只修改 Markdown 文档时，至少执行 `git diff --check` 并人工核对相对链接、命令和敏感信息。
