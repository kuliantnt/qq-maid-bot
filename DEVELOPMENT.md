# QQ Maid Bot 开发维护文档

本文面向项目开发者和维护者，保留仓库级架构边界、开发命令、维护约定和检查规则。运行目录、部署、私有配置和运行数据细节已经分流到 [runtime/README.md](./runtime/README.md)；QQ 官方 gateway 细节见 [qq-maid-gateway-rs/README.md](./qq-maid-gateway-rs/README.md)；Rust Core 模块细节见 [qq-maid-core/README.md](./qq-maid-core/README.md)。

如果只是第一次了解项目，请先阅读 [README.md](./README.md)。

## 架构边界

- `qq-maid-gateway-rs/`：QQ 官方 C2C / 群 at gateway 接入层，负责 QQ 事件接收、消息转换、`/ping` 诊断、回复发送和本机内部主动推送出口。
- `qq-maid-core/`：Rust Core / 查询 / 记忆 / session / prompt 模块，公开 `GET /healthz` 和 `POST /v1/respond`。
- `src/main.rs`：统一 `qq-maid-bot` 程序入口，负责一次性初始化 dotenv / tracing，并按顺序拉起 Core HTTP 与 Gateway。
- `qq-maid-common/`：gateway 和 Core 共享的无业务状态基础工具，目前承载时间、日期和时区处理。
- `runtime/`：服务器部署运行目录，保留 release 二进制、运行配置和运行产物。
- `scripts/`：部署、进程控制和网络诊断脚本源码目录。
- `scripts/diagnose-network.sh`：shell 版网络诊断脚本，替代旧 Python 诊断入口。

QQ 接入相关能力优先在 gateway 演进；普通聊天、查询、记忆、session、待办、会话命令和 prompt 等业务逻辑优先在 `qq-maid-core/` 内部维护。

## 项目结构

```text
.
├── Cargo.toml
├── Cargo.lock
├── Makefile
├── AGENTS.md
├── README.md
├── DEVELOPMENT.md
├── LICENSE
├── scripts/
│   ├── deploy.sh
│   ├── deploy-local.sh
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

Rust 构建由仓库根目录统一管理：根包产出唯一主可执行文件 `qq-maid-bot`，workspace 成员为 `qq-maid-common/`、`qq-maid-gateway-rs/` 和 `qq-maid-core/`；统一锁文件位于根目录 `Cargo.lock`，release 产物位于根目录 `target/release/`。

不要恢复子目录 `Cargo.lock`，也不要在文档或脚本中继续引用 `qq-maid-*/target/` 旧路径。

## 本地启动

环境要求：

- Rust toolchain
- Bash、curl 或 wget
- QQ 官方机器人 AppID 和 AppSecret
- 模型 provider 所需 API key
- 天气能力需要和风天气 API 配置

首次配置，从仓库根目录执行：

```bash
cp runtime/.env.example runtime/config/.env
```

复制后按需填写模型、QQ、天气和 RSS 配置。配置文件加载顺序、路径变量、私有 prompt、世界观、成员映射和运行数据目录说明见 [runtime/README.md](./runtime/README.md)。

编辑 `runtime/config/.env` 后，启动统一程序：

```bash
make run
```

## 文档分工

- [README.md](./README.md)：项目定位、核心能力、快速开始和用户可见指令示例。
- [qq-maid-core/README.md](./qq-maid-core/README.md)：Rust Core 模块边界、HTTP facade、指令 flow、配置项和检查方式。
- [qq-maid-gateway-rs/README.md](./qq-maid-gateway-rs/README.md)：QQ 官方 gateway、事件范围、消息发送、日志、`/ping` 和 `/internal/push`。
- [runtime/README.md](./runtime/README.md)：运行目录、部署产物、真实配置、路径解析、运行数据、控制脚本和诊断。
- [runtime/.env.example](./runtime/.env.example)：环境变量模板和字段说明。

## 常用命令

```bash
make run
make test
make test-common
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
- `make test-core`：执行 Rust common 与 Rust Core fmt check、测试和 `cargo check`。
- `make test-gateway`：执行 Rust common 与 Rust gateway fmt check、测试和 `cargo check`。
- `make build`：构建统一 `qq-maid-bot` release 二进制。
- `make deploy-local`：执行 `scripts/deploy-local.sh`，构建并安装到本地 `runtime/`。
- `make deploy-remote`：执行 `scripts/deploy.sh`，构建并发布 release 二进制到脚本配置的远端运行目录。
- `make diagnose`：运行 shell 网络诊断，检查配置文件存在性、代理、公网出口 IP 和 Core `/healthz`。
- `scripts/validate-runtime.sh check`：检查运行中统一服务状态、GLM 上游、Web 控制台和最近日志。
- `scripts/validate-runtime.sh glm`：只验证 GLM / OpenAI 兼容 key 和模型调用。
- `scripts/validate-runtime.sh console`：只验证 Web 控制台 `/console/`。
- `scripts/validate-runtime.sh restart-source`：用 `target/debug/qq-maid-bot` 临时验证当前源码统一程序。
- `make clean`：清理根目录 Cargo Workspace 的构建产物。

## HTTP 与命令入口

Rust Core HTTP 层只公开：

- `GET /healthz`
- `POST /v1/respond`

旧 HTTP 路由 `/query`、HTTP `/memory`、`/v1/chat` 不再公开。查询、记忆、待办、会话和 RSS 都通过 `/v1/respond` 内部命令流程承载。

当前常用 slash 指令：

- 会话：`/new`、`/rename`、`/resume`、`/clear`、`/state`、`/compact`、`/help`。`/list` 仍作为 deprecated 兼容别名保留，推荐使用 `/resume` 或 `/恢复`。
- 记忆：`/memory`、`/memory 记忆内容`、`/memory show 1`、`/memory edit 1 新内容`、`/memory delete 1`；中文别名 `/记忆`、`/记`。
- 待办：`/todo`、`/todo add 待办内容`、`/todo done 1`、`/todo undo 1`、`/todo edit 1 新内容`、`/todo delete 1`；中文别名 `/待办`、`/任务`。按编号完成或恢复通常依赖最近一次列表快照。
- RSS：`/rss`、`/rss add RSS地址 [名称]`、`/rss delete 1`、`/rss test RSS地址`；中文别名 `/订阅`。
- 查询：`/查 关键词`、`/查询 关键词`、`/search 关键词`。中文紧凑写法如 `/查今天新闻` 也会进入联网查询。
- 天气：`/天气杭州`、`/天气 杭州`、`/杭州天气`、`/weather 杭州`。
- 翻译：`/翻译 文本`、`/翻译日语 文本`、`/翻译成英语 文本`。

## 维护约定

- 默认做小改动，保持用户可见行为稳定。
- 新增或调整 QQ 接入、事件处理和发送逻辑时，优先修改 `qq-maid-gateway-rs/`。
- 修改普通聊天、查询、记忆、session、待办、会话命令或 prompt 时，优先修改 `qq-maid-core/`。
- Rust HTTP 层只公开 `GET /healthz` 和 `POST /v1/respond`；不要重新公开 `/query`、HTTP `/memory` 或 `/v1/chat`。
- 通用日期、时间和时区语义优先复用 `qq-maid-common/src/time_context.rs`；Core 内部的 `qq-maid-core/src/util/time_context.rs` 保留为兼容 re-export。
- 未来目标是通用 QQ 机器人；不要把具体人设、群聊内容、真实用户信息或业务材料写死进代码。

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
- 跨 Core / gateway 或提交前：执行 `make test`。
- 涉及启动、依赖、环境变量、QQ 事件或模型调用：除测试外还应运行 `scripts/validate-runtime.sh check`。
- 涉及 GLM / OpenAI 兼容 key、模型候选链或 `OPENAI_API_MODE=chat_only`：运行 `scripts/validate-runtime.sh glm`。
- 涉及 Web 控制台或 Markdown 预览接口：运行 `scripts/validate-runtime.sh console`，必要时人工访问 `/console/`。
- 涉及统一入口、启动顺序或未提交源码验证：先执行 `cargo build -p qq-maid-bot`，再运行 `scripts/validate-runtime.sh restart-source`。
- 涉及网络、代理或 QQ 后台白名单问题：运行 `make diagnose`。
- 只修改 Markdown 文档时，至少执行 `git diff --check` 并人工核对相对链接、命令和敏感信息。
