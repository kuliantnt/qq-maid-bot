# 小女仆机器人 开发维护文档

本文面向项目开发者和维护者，保留仓库级架构边界、开发命令、维护约定和检查规则。运行目录、部署、私有配置和运行数据细节已经分流到 [runtime/README.md](../runtime/README.md)；QQ 官方 gateway 细节见 [qq-maid-gateway-rs/README.md](../qq-maid-gateway-rs/README.md)；Rust Core 模块细节见 [qq-maid-core/README.md](../qq-maid-core/README.md)。

如果只是第一次了解项目，请先阅读 [README.md](../README.md)。

## 架构边界

- `qq-maid-gateway-rs/`：QQ 官方 C2C / 群 at gateway 接入层，负责 QQ 事件接收、消息转换、`/ping` 诊断、回复发送和本机内部主动推送出口。
- `qq-maid-core/`：Rust Core / 查询 / 记忆 / session / prompt / 业务 Tool 模块，通过 `CoreService` 提供进程内业务入口，并公开 `GET /healthz`。
- `qq-maid-llm/`：模型协议、Provider 路由、fallback、SSE、usage、健康观测、OpenAI Web Search 和模型原生 Tool Loop 基础设施。
- `src/main.rs`：统一 `qq-maid-bot` 程序入口，负责一次性初始化 dotenv / tracing，并按顺序拉起 Core HTTP 与 Gateway。
- `qq-maid-common/`：gateway 和 Core 共享的无业务状态基础工具，目前承载时间、日期和时区处理。
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
│   └── tasks/
├── LICENSE
├── scripts/
│   ├── deploy-remote.sh
│   ├── deploy-local.sh
│   ├── sync_knowledge.sh
│   ├── deploy.conf.example
│   ├── diagnose-network.sh
│   └── botctl.sh
├── runtime/
│   ├── .env.example
│   ├── README.md
│   └── config/
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
- `make deploy-remote`：执行 `scripts/deploy-remote.sh`，构建并发布 release 二进制到 `scripts/deploy.conf` 配置的远端运行目录。
- `make diagnose`：运行 shell 网络诊断，检查配置文件存在性、代理、公网出口 IP 和 Core `/healthz`。
- `scripts/validate-runtime.sh check`：检查运行中统一服务状态、GLM 上游、Web 控制台和最近日志。
- `scripts/validate-runtime.sh glm`：只验证 GLM / OpenAI 兼容 key 和模型调用。
- `scripts/validate-runtime.sh console`：只验证 Web 控制台 `/console/`。
- `scripts/validate-runtime.sh restart-source`：用 `target/debug/qq-maid-bot` 临时验证当前源码统一程序。
- `bash scripts/sync_knowledge.sh`：以镜像语义把本地知识库 Markdown 同步到 `scripts/deploy.conf` 配置的远端服务器。本地删除或重命名的 `.md` 会从对应远端子目录中删除，删除范围仅限该子目录内部；非 `.md` 文件不会被传输或删除。
- `bash scripts/sync_knowledge.sh --dry-run`：仅预览同步差异（包含将被删除的远端 `.md`），不实际传输或删除。远端知识库根目录由 `deploy.conf` 的 `REMOTE_KNOWLEDGE_DIR` 控制，未设置时默认为 `${REMOTE_PROJECT_DIR}/runtime/config/knowledge`，便于兼容应用通过 `KNOWLEDGE_DIR` 读取外部知识目录的部署方式。
- `make clean`：清理根目录 Cargo Workspace 的构建产物。

## HTTP 与命令入口

Rust HTTP 层只公开外部运维 / 管理能力：

- `GET /healthz`

旧 HTTP 路由 `/query`、HTTP `/memory`、`/v1/chat` 和内部 respond 主入口不再公开。查询、记忆、待办、会话和 RSS 都通过 `CoreService::respond` 进程内命令流程承载。

当前常用 slash 指令：

- 会话：`/new`、`/rename`、`/resume`、`/clear`、`/state`、`/compact`、`/help`。`/list` 仍作为 deprecated 兼容别名保留，推荐使用 `/resume` 或 `/恢复`。
- 记忆：`/memory`、`/memory 记忆内容`、`/memory show 1`、`/memory edit 1 新内容`、`/memory delete 1`；中文别名 `/记忆`、`/记`。
- 待办：slash 入口只保留查询（`/todo`、`/todo all`、`/todo search 关键词`、`/todo done`、`/todo undo`；中文别名 `/待办`、`/任务`）。新增、完成、恢复、修改、取消和永久删除请直接用自然语言触发 Todo Tool；按编号继续操作依赖最近一次用户可见列表快照。
- RSS：`/rss`、`/rss add RSS地址 [名称]`、`/rss delete 1`、`/rss test RSS地址`；中文别名 `/订阅`。
- 查询：`/查 关键词`、`/查询 关键词`、`/search 关键词`。中文紧凑写法如 `/查今天新闻` 也会进入联网查询。
- 天气：`/天气杭州`、`/天气 杭州`、`/杭州天气`、`/weather 杭州`。
- 翻译：`/翻译 文本`、`/翻译日语 文本`、`/翻译成英语 文本`。

## 维护约定

- 默认做小改动，保持用户可见行为稳定。
- 新增或调整 QQ、OneBot、微信等平台接入、事件处理和发送逻辑时，优先修改 `qq-maid-gateway-rs/` 的 adapter / sender 边界。
- 修改模型协议、Provider 路由、fallback、SSE、usage、健康观测、OpenAI Web Search 传输或 Tool Loop 协议时，优先修改 `qq-maid-llm/`。
- Gateway 内部继续保持分层边界：`gateway/mod.rs` 负责顶层编排，`gateway/platform/` 负责平台协议到 `InboundMessage` / `CoreRequest` 的映射，`gateway/protocol.rs` 负责 WebSocket 协议与事件分发，`gateway/outbound.rs` 负责出站投递能力和发送状态记录，`respond.rs` 负责 CoreService 进程内桥接；不要把这些职责重新混回单个超长文件。
- 修改普通聊天、查询命令、记忆、session、待办、会话命令、prompt 或具体业务 Tool 时，优先修改 `qq-maid-core/`。
- 普通聊天进入 Tool Loop 前后的工具域业务判断要优先收敛到 `qq-maid-core/src/runtime/tools/<domain>/`：例如 Todo 的自然语言路由、编号续指、输出成功验真和失败文案应由 Todo 域暴露门面；`runtime/respond/` 只保留通用分流、会话连接、状态提示和必要 adapter。
- Rust HTTP 层只公开 `GET /healthz`，以及启用控制台时的 `/console/` 和 `/api/v1/markdown/render`；不要重新公开 `/query`、HTTP `/memory`、`/v1/chat` 或内部 respond 主入口。
- 通用日期、时间和时区语义优先复用 `qq-maid-common/src/time_context/`，不要在 Core 内部保留重复 helper。
- Tool Calling 的最终目标参考 Codex 的受控工具调用体验，但本项目必须保持 QQ 场景边界：私聊优先、群聊谨慎、工具白名单、权限校验、超时和输出大小限制不可省略。
- 自定义业务 Tool 的二开步骤见 [custom-tools.md](./development/custom-tools.md)，包括新增文件、注册、`agent.toml` 白名单和测试要求。
- 未来目标是支持多入口的通用机器人；不要把具体人设、群聊内容、真实用户信息或业务材料写死进代码。

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
- 涉及统一入口、启动顺序或未提交源码验证：先执行 `cargo build -p qq-maid-bot`，再运行 `scripts/validate-runtime.sh restart-source`。
- 涉及网络、代理或 QQ 后台白名单问题：运行 `make diagnose`。
- 只修改 Markdown 文档时，至少执行 `git diff --check` 并人工核对相对链接、命令和敏感信息。
