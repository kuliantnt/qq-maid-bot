# QQ Bot 本地项目

这是一个 QQ 官方机器人本地开发仓库。当前主线由 Rust QQ gateway 和 Rust LLM 服务组成；旧 Python bot 接入层已经移除，后续新开发默认放到 `qq-maid-gateway-rs/` 或 `qq-maid-llm/`。

## 架构边界

- `qq-maid-gateway-rs/`：QQ 官方 C2C / 群 at gateway 接入层，负责 QQ 事件接收、消息转换、`/ping` 诊断、回复发送和本机内部主动推送出口。
- `qq-maid-llm/`：Rust LLM / 查询 / 记忆 / session / prompt 服务，公开 `GET /healthz` 和 `POST /v1/respond`。
- `runtime/`：服务器部署运行目录，保留 release 二进制、运行配置和运行产物。
- `scripts/`：部署、进程控制和网络诊断脚本源码目录。
- `scripts/diagnose-network.sh`：shell 版网络诊断脚本，替代旧 Python 诊断入口。

QQ 接入相关能力优先在 gateway 演进；普通聊天、查询、记忆、session、待办、会话命令和 prompt 等业务逻辑优先在 `qq-maid-llm/` 内部维护。

## 项目结构

```text
.
├── Cargo.toml
├── Cargo.lock
├── Makefile
├── AGENTS.md
├── .env.example
├── README.md
├── scripts/
│   ├── deploy.sh
│   ├── diagnose-network.sh
│   ├── gatewayctl.sh
│   └── llmctl.sh
├── runtime/
│   └── config/
├── qq-maid-llm/
│   ├── src/
│   └── config/
└── qq-maid-gateway-rs/
    ├── src/
    │   ├── app/
    │   ├── config/
    │   └── gateway/
    └── README.md
```

Rust 构建由仓库根目录的 Cargo Workspace 统一管理，workspace 成员为 `qq-maid-gateway-rs/` 和 `qq-maid-llm/`；统一锁文件位于根目录 `Cargo.lock`，release 产物位于根目录 `target/release/`。

## 快速开始

环境要求：

- Rust toolchain
- Bash、curl 或 wget
- QQ 官方机器人 AppID 和 AppSecret
- 模型 provider 所需 API key

首次配置：

```bash
cp .env.example .env
```

公开仓库只包含源码和 `.example` 模板。复制 `.env.example` 后，按需填写模型、QQ 和天气配置；未显式配置 `PROMPT_DIR` 时，LLM 会在缺少私有 prompt 文件的情况下使用内置通用 prompt 启动。私人 prompt、世界观、成员映射和运行数据建议放在外部私有目录，再通过 `.env` 中的路径变量注入。

编辑根目录 `.env` 后，先启动 Rust LLM 服务：

```bash
make run-llm
```

再启动 Rust gateway：

```bash
make run
```

`make run` 当前等价于 `make run-gateway`。

## 常用命令

```bash
make run
make run-llm
make run-gateway
make test
make test-llm
make test-gateway
make build
make build-llm
make build-gateway
make deploy
make status
make diagnose
make clean
```

- `make run-llm`：启动 Rust LLM / 查询 / 记忆服务。
- `make run` / `make run-gateway`：启动 Rust QQ C2C gateway。
- `make test`：执行根目录 Cargo Workspace 的 fmt、test 和 check。
- `make test-llm`：执行 Rust LLM fmt check 和测试。
- `make test-gateway`：执行 Rust gateway fmt check、测试和 `cargo check`。
- `make build`：构建 Rust LLM 和 Rust gateway release 二进制。
- `make deploy`：执行 `scripts/deploy.sh`，构建并发布 release 二进制到远端。
- `make diagnose`：运行 shell 网络诊断，检查配置文件存在性、代理、公网出口 IP 和 LLM `/healthz`。
- `make clean`：清理根目录 Cargo Workspace 的构建产物。

## 部署

`runtime/` 是服务器运行目录。执行 `make build` 只会构建本地 release 二进制；发布到服务器可从仓库根目录执行：

```bash
./scripts/deploy.sh
```

脚本会构建 release 二进制、上传到远端 `runtime/` 目录，并重启远端服务。远端运行目录结构：

```text
runtime/
├── qq-maid-llm
├── qq-maid-gateway-rs
├── llmctl.sh
├── gatewayctl.sh
├── diagnose-network.sh
└── config/
```

本地构建完成后的二进制路径为：

```text
target/release/qq-maid-llm
target/release/qq-maid-gateway-rs
```

推荐把公开源码、私有配置和运行数据分开，例如：

```text
/opt/qqbot/
├── app/       # 公开源码仓库
├── private/   # 私有配置仓库或本机私有目录，不公开
└── data/      # SQLite、日志、pid 等运行产物，不进任何 Git 仓库
```

服务器上可把真实 `.env` 放到 `runtime/.env` 或 `runtime/config/.env`，并在其中把 `PROMPT_DIR`、`MEMBER_ID_MAPPING_FILE`、`WORLD_FILE`、`APP_DB_FILE` 指向外部私有配置或运行数据目录，再执行：

```bash
cd runtime
./llmctl.sh start
./gatewayctl.sh start
```

如果服务器上仍保留旧 `llm/` 运行目录，首次切换前需要先按旧路径停掉旧进程或迁移 pid / log / `.env` 等运行文件，避免新旧目录同时拉起服务。

## 配置入口

- 本地统一配置入口：仓库根 `.env`，模板见根 `.env.example`。
- 模块兼容配置入口：`qq-maid-gateway-rs/config/.env`、`qq-maid-llm/config/.env`。
- 部署目录配置：`runtime/.env` 或 `runtime/config/.env`。

`scripts/llmctl.sh` 和 `scripts/gatewayctl.sh` 部署后会复制为远端 `runtime/llmctl.sh` 与 `runtime/gatewayctl.sh`。控制脚本当前只 source 第一个存在的配置文件：显式 `LLM_ENV_FILE` / `GATEWAY_ENV_FILE` 优先，其次是仓库根 `.env`、`runtime/.env`、`runtime/config/.env` 与对应模块 `config/.env`。Rust 进程自身还会按当前工作目录尝试加载 `config/.env` 再加载 `.env`；`dotenvy` 默认不覆盖已存在的环境变量，所以进程环境变量优先，先加载的 dotenv 文件会保留同名变量，后续文件只补充缺失项。

常用外部路径变量：

- `PROMPT_DIR`：包含 `maid_system.md`、`mode_rules.md`、`session_context.md` 的目录。未配置或为空时使用默认 `config/prompts`，默认目录缺少真实 prompt 时回退到内置通用 prompt；显式配置后缺文件或空文件会报配置错误。
- `WORLD_FILE`：可选世界观文件。未配置或为空表示不注入世界观；配置后文件必须存在、可读且非空。
- `MEMBER_ID_MAPPING_FILE`：成员编号映射 JSON。文件不存在时按空映射处理；JSON 语法错误会启动失败并给出路径和原因。
- `APP_DB_FILE`：通用 SQLite 文件路径，承载 Session、待办、长期记忆和 RSS / Atom 订阅。

相对路径按进程启动工作目录解析。本地 `make run-llm` 通常以仓库根目录为工作目录；部署脚本启动时会 `cd runtime` 后运行二进制。

不要读取、打印或提交真实 `.env`，也不要把 token、secret、API Key、bot appid、私钥、真实 QQ 群聊或私聊内容、openid、群 ID、用户数据写进文档和代码。Secret、SQLite 数据库、日志、pid 文件和聊天记录不应进入任何 Git 仓库；真实 prompt、世界观和成员映射只应放在私有配置仓库或本地私有目录，不进入公开仓库。

## 运行数据

运行时数据默认生成在：

```text
qq-maid-llm/data/
└── storage/
    └── app.db

runtime/
├── logs/
├── run/
├── qq-maid-llm
└── qq-maid-gateway-rs
```

长期记忆只能通过明确记忆指令生成草稿，并由用户确认后写入。普通聊天不要自动写长期记忆。

Session、待办、长期记忆和 RSS / Atom 订阅均保存在 `APP_DB_FILE` 指向的通用 SQLite 文件中。公开源码场景建议把 `APP_DB_FILE` 指向外部运行数据目录，例如 `/opt/qqbot/data/app.db`。旧版 Session JSON 目录和旧版 Memory JSONL 文件不再读取，也不会自动迁移；本地如残留旧目录或旧文件，只作为历史运行产物处理。RSS 通过 `/rss` 或 `/订阅` 管理，首次添加订阅只建立当前条目基线，不主动推送历史文章；后续轮询由 `qq-maid-llm` 调用 gateway 的本机内部 push 入口发送到对应私聊或群聊目标。

## 维护约定

- 默认做小改动，保持用户可见行为稳定。
- 新增或调整 QQ 接入、事件处理和发送逻辑时，优先修改 `qq-maid-gateway-rs/`。
- 修改普通聊天、查询、记忆、session、待办、会话命令或 prompt 时，优先修改 `qq-maid-llm/`。
- Rust HTTP 层只公开 `GET /healthz` 和 `POST /v1/respond`；不要重新公开 `/query`、HTTP `/memory` 或 `/v1/chat`。
- 通用日期边界解析优先复用 `qq-maid-llm/src/util/time_context.rs`。
- 未来目标是通用 QQ 机器人；不要把具体人设、群聊内容、真实用户信息或业务材料写死进代码。

## 修改后检查

修改代码后，根据影响范围执行：

```bash
make test-llm
make test-gateway
make test
```

- 只影响 Rust LLM：至少执行 `make test-llm`。
- 只影响 Rust gateway：至少执行 `make test-gateway`。
- 跨 LLM / gateway 或提交前：执行 `make test`。
- 涉及启动、依赖、环境变量、QQ 事件或模型调用：除测试外还应本地启动验证。
- 涉及网络、代理或 QQ 后台白名单问题：运行 `make diagnose`。
