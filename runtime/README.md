# runtime/ — 服务器运行配置目录

本目录是服务器运行目录示例，部署后会放置 release 二进制、控制脚本和运行期配置。真实 `.env`、成员映射、世界观和提示词都属于本地私有配置；
仓库只保留 `.example` 模板，用于说明字段含义。生产部署可以通过 `runtime/config/.env` 或 `runtime/.env` 把路径指向外部私有配置仓库或本机私有目录。

## 目录结构

```
runtime/
├── .env.example                     # 可提交的环境变量模板
├── .env                             # 兼容环境变量文件，不提交
├── qq-maid-bot                      # 部署后的统一 Rust release 二进制，不提交
├── botctl.sh                        # 部署后的聚合控制脚本，不提交
├── validate-runtime.sh              # 部署后的运行诊断脚本，不提交
├── README.md                        # 本文件
├── static/
│   └── index.html                   # 可提交的本地 Web 控制台静态页
├── config/
│   ├── .env                         # 推荐真实环境变量文件，不提交
│   ├── world.example.md             # 可提交的 WORLD_FILE 模板
│   ├── world.md                     # 可选世界观文件，路径由 WORLD_FILE 指定，不提交
│   ├── context_modules.example.toml # 可提交的上下文模块索引模板
│   ├── context_modules.toml         # 可选上下文模块索引，不提交
│   ├── member_id_mapping.example.json
│   ├── member_id_mapping.json       # 本地私有成员编号映射，不提交
│   ├── context/
│   │   ├── deploy.example.md        # 可提交的上下文模块模板
│   │   └── ops.example.md           # 可提交的上下文模块模板
│   └── prompts/
│       ├── *.example.md             # 可提交的通用模板
│       ├── maid_system.md           # 本地私有系统提示词，不提交
│       ├── mode_rules.md            # 本地私有模式规则，不提交
│       └── session_context.md       # 本地私有会话上下文规则，不提交
├── data/
│   └── storage/
│       └── app.db                   # 默认 SQLite 数据库，不提交
├── logs/                            # 控制脚本日志目录，不提交
└── run/                             # pid 等运行状态，不提交
```

## 快速配置

从仓库根目录复制模板：

```bash
cp runtime/.env.example runtime/config/.env
```

编辑 `runtime/config/.env`，填写 QQ 官方机器人、模型 provider、天气和 RSS 等必要配置。公开仓库只包含源码和 `.example` 模板；真实 `.env`、prompt、世界观、成员映射、SQLite、日志、pid 和聊天记录都不要提交。

未显式配置 `PROMPT_DIR` 时，Core 模块会使用默认 `config/prompts`。默认目录缺少真实 prompt 文件时，当前实现会回退到内置通用 prompt；显式配置 `PROMPT_DIR` 后，缺文件或空文件会作为配置错误处理。

## 配置加载顺序

`scripts/botctl.sh` 部署后会复制为运行目录下的 `botctl.sh`。控制脚本只 source 第一个存在的配置文件：

1. 显式 `BOT_ENV_FILE`
2. `runtime/config/.env`
3. `runtime/.env`

Rust 进程自身会按当前工作目录尝试加载 `config/.env` 再加载 `.env`。`make run` 和部署控制脚本都会以 `runtime/` 作为工作目录启动，因此默认相对路径都按 `runtime/` 解析。

`dotenvy` 默认不覆盖已存在的环境变量：进程环境变量优先，先加载的 dotenv 文件会保留同名变量，后续文件只补充缺失项。

常用外部路径变量：

- `PROMPT_DIR`：包含 `maid_system.md`、`mode_rules.md`、`session_context.md` 的目录。
- `WORLD_FILE`：可选世界观文件；留空表示不注入世界观，配置后文件必须存在、可读且非空。
- `CONTEXT_MODULES_FILE`：普通聊天链路的可选上下文模块索引；留空表示关闭该能力。索引中的模块文件建议相对索引文件所在目录维护，真实私有模块内容不要提交。
- `MEMBER_ID_MAPPING_FILE`：成员编号映射 JSON。文件不存在时按空映射处理；JSON 语法错误会启动失败。
- `APP_DB_FILE`：通用 SQLite 文件路径，承载 Session、待办、长期记忆、RSS / Atom 订阅及 RSS 去重状态。

推荐把公开源码、私有配置和运行数据分开，例如：

```text
/opt/qqbot/
├── app/       # 公开源码仓库
├── private/   # 私有配置仓库或本机私有目录，不公开
└── data/      # SQLite、日志、pid 等运行产物，不进任何 Git 仓库
```

对应配置示例：

```env
PROMPT_DIR=/opt/qqbot/private/config/prompts
MEMBER_ID_MAPPING_FILE=/opt/qqbot/private/config/member_id_mapping.json
WORLD_FILE=/opt/qqbot/private/config/world.md
CONTEXT_MODULES_FILE=/opt/qqbot/private/config/context_modules.toml
APP_DB_FILE=/opt/qqbot/data/app.db
```

## 文件说明

### `config/.env` / `.env`

全局环境变量。控制 QQ Bot SDK 参数、LLM 供应商（OpenAI / DeepSeek）、主模型、内部任务模型（含翻译模型）、LLM 服务监听地址、超时和外部配置路径等。首次配置推荐从仓库根目录执行：

```bash
cp runtime/.env.example runtime/config/.env
```

控制脚本默认先读取 `runtime/config/.env`，再读取 `runtime/.env`；显式 `BOT_ENV_FILE` 会覆盖默认查找。
**注意：包含密钥，不要提交到公开仓库。**

和私有配置仓库相关的常用路径变量：

- `PROMPT_DIR`：包含 `maid_system.md`、`mode_rules.md`、`session_context.md` 的目录。
- `WORLD_FILE`：可选世界观文件；留空表示按通用助手运行。
- `CONTEXT_MODULES_FILE`：普通聊天链路的上下文模块索引；未配置时完全关闭该能力。
- `MEMBER_ID_MAPPING_FILE`：成员编号映射 JSON 文件。
- `APP_DB_FILE`：运行数据库文件，应放在不进 Git 的数据目录。

完整字段以 [`.env.example`](./.env.example) 为准。

### `config/member_id_mapping.json`

成员编号映射。键为成员编号（字符串），值为名称和简介。JSON 格式不支持注释，字段含义：

- `name` — 成员名称
- `profile` — 一句话简介

真实成员映射可能包含个人信息或私人设定，应保留在外部私有路径或本地未跟踪文件中。公开仓库只提交
`member_id_mapping.example.json`。文件不存在时按空映射处理；JSON 语法错误会启动失败。

### `config/prompts/maid_system.md`

**核心系统提示词**。定义助手职责、默认语气、QQ 群聊规则、现实问题规则和安全规则。
修改此文件会直接影响机器人的回复风格。真实提示词不提交，公开仓库只提交 `.example.md`。

### `config/world.md`

可选世界观或角色设定提示词。正式入口是运行目录配置中的 `WORLD_FILE`，不再要求把世界观固定写入
`PROMPT_DIR/innerworld_lore.md`。未配置 `WORLD_FILE` 时按通用助手运行；一旦配置，文件必须存在、可读且非空。

开源前如果曾提交过真实世界观，需要额外清理 Git 历史；单纯从当前 HEAD 删除不能移除历史记录。

### `config/context_modules.toml`

普通聊天链路的可选上下文模块索引。只有命中的模块才会作为额外 system prompt 注入；`/todo`、`/memory`、`/compact` 等流程不会读取它。索引建议从 `context_modules.example.toml` 复制，并把模块正文放到同目录下的 `context/` 私有文件中。

### `config/prompts/mode_rules.md`

根据用户消息内容自动判断应进入的模式：

1. 日常聊天模式
2. 整理归档模式
3. 方案建议模式
4. 低打扰支持模式
5. 现实问题模式

### `config/prompts/session_context.md`

多轮对话的上下文处理规则：

- 前台成员可能切换或多人同时在场
- 如何判断当前说话者
- 短句（"对啊""继续""给 codex"）优先理解为补充而非新话题
- slash 指令已由程序处理，不要假装执行

## 运行数据

默认运行产物：

```text
runtime/
├── data/
│   └── storage/
│       └── app.db
├── logs/
├── run/
└── qq-maid-bot
```

Session、待办、长期记忆、RSS / Atom 订阅和 RSS 去重状态均保存在 `APP_DB_FILE` 指向的通用 SQLite 文件中。旧版 Session JSON 目录和旧版 Memory JSONL 文件不再读取，也不会自动迁移；本地如残留旧目录或旧文件，只作为历史运行产物处理。

长期记忆只能通过明确记忆指令生成草稿，并由用户确认后写入。普通聊天不要自动写长期记忆。RSS 通过 `/rss` 或 `/订阅` 管理，首次添加订阅只建立当前条目基线，不主动推送历史文章；后续轮询由 `qq-maid-core` 调用 gateway 的本机内部 push 入口发送到对应私聊或群聊目标。

配置、prompt、世界观、成员映射、日志、pid、release 二进制和 gateway WebSocket 临时状态不属于 `APP_DB_FILE` 承载范围。

## 构建和部署

从仓库根目录构建 release 二进制：

```bash
make build
```

本地构建产物位于：

```text
target/release/qq-maid-bot
```

发布到脚本配置的远端服务器：

```bash
make deploy-remote  # 内部调用 scripts/deploy-remote.sh
```

脚本会构建 release 二进制、上传到远端 `runtime/` 目录，并重启远端服务。远端运行目录结构：

```text
runtime/
├── qq-maid-bot
├── botctl.sh
├── diagnose-network.sh
├── validate-runtime.sh
├── static/
│   └── index.html
└── config/
```

服务器上可把真实 `.env` 放到 `runtime/.env` 或 `runtime/config/.env`，并在其中把 `PROMPT_DIR`、`MEMBER_ID_MAPPING_FILE`、`WORLD_FILE`、`APP_DB_FILE` 指向外部私有配置或运行数据目录，再执行：

```bash
cd runtime
./botctl.sh start
```

如果服务器上仍保留旧 `llm/` 运行目录，首次切换前需要先按旧路径停掉旧进程或迁移 pid / log / `.env` 等运行文件，避免新旧目录同时拉起服务。

## 运行验证脚本

`validate-runtime.sh` 用于复查机器人运行状态、GLM / OpenAI 兼容上游、Web 控制台和最近日志。脚本只读取运行状态和调用本机 HTTP 接口，不会打印 `.env` 中的密钥值。

从仓库源码目录运行：

```bash
scripts/validate-runtime.sh check
```

部署到 `runtime/` 后运行：

```bash
cd runtime
./validate-runtime.sh check
```

常用子命令：

```bash
./validate-runtime.sh check      # 服务状态、/healthz、上游诊断、/console/ 和最近日志
./validate-runtime.sh glm        # 只验证 GLM / OpenAI 兼容 key 和模型调用
./validate-runtime.sh console    # 只验证 Web 控制台 /console/
./validate-runtime.sh logs       # 只查看统一服务最近日志
./validate-runtime.sh restart    # 重启 release 版统一程序后执行 check
```

本地调试未提交源码时，可以用 debug/source 统一程序验证当前工作区构建产物：

```bash
cargo build -p qq-maid-bot
scripts/validate-runtime.sh restart-source
```

`restart-source` 会停止 release 版统一程序，然后用 `target/debug/qq-maid-bot` 启动临时源码构建；日志和 pid 默认写入 `runtime/logs/qq-maid-bot-source.log` 与 `runtime/run/qq-maid-bot-source.pid`。

常用环境覆盖：

```bash
LINES=30 ./validate-runtime.sh check
LLM_SERVER_URL=http://127.0.0.1:8787 ./validate-runtime.sh glm
QQ_MAID_RUNTIME_DIR=/opt/qqbot/runtime ./validate-runtime.sh check
```

## GitHub Release 包

推送形如 `v*` 的 Git tag 会触发 GitHub Actions 构建 Linux x86_64 Release 包，并创建同名 GitHub Release：

```bash
git tag v0.1.0
git push origin v0.1.0
```

发布包名称类似：

```text
qq-maid-bot-v0.1.0-linux-x86_64.tar.gz
qq-maid-bot-v0.1.0-linux-x86_64.tar.gz.sha256
```

Release 包采用白名单生成，只包含统一 `qq-maid-bot` release 二进制、`botctl.sh`、`diagnose-network.sh`、`validate-runtime.sh`、`static/index.html`、本文件、`.env.example`、公开 `.example` 配置模板、`VERSION` 和空的 `data/storage/` 目录。真实 `.env`、私有 prompt、世界观、成员映射、SQLite 数据库、日志、pid 和 `.bak` 备份不会被写入归档。

首次使用 Release 包：

```bash
tar -xzf qq-maid-bot-v0.1.0-linux-x86_64.tar.gz
cd qq-maid-bot-v0.1.0-linux-x86_64
cp .env.example config/.env
```

编辑 `config/.env`，填写 QQ 官方机器人、模型 provider、天气和 RSS 等必要配置后启动：

```bash
./botctl.sh start
```

打包阶段已经保留二进制和脚本的可执行权限；如果文件经过不保留权限的传输方式复制，再手工执行 `chmod +x qq-maid-bot botctl.sh diagnose-network.sh validate-runtime.sh`。

升级时不要直接覆盖已有运行目录中的私有文件和运行数据，尤其是：

- `config/.env`
- 私有 prompt、世界观和成员映射
- SQLite 数据库
- 日志和 pid 等运行状态

建议先解压到新的目录，确认版本和配置模板变化后，再按需替换二进制、控制脚本和公开 `.example` 模板。

## 控制脚本和诊断

常用控制命令：

```bash
./botctl.sh status
./botctl.sh restart
./botctl.sh console
./botctl.sh health
./botctl.sh logs
```

诊断脚本可从仓库根目录执行：

```bash
make diagnose
```

也可在部署后的运行目录执行：

```bash
./diagnose-network.sh
./validate-runtime.sh check
```

诊断输出只应展示 secret 是否存在、脱敏后的 ID / URL、代理和公网出口检查结果，不应打印完整 token、AppSecret、API Key、openid、群 ID 或聊天内容。

## 联动关系

```
runtime/config/.env 或 runtime/.env (供应商/密钥)
  └→ Rust Core HTTP (127.0.0.1:8787)
       └→ /v1/respond 接口
            └→ 组装 system prompt:
                 maid_system.md + mode_rules.md + session_context.md
                 + WORLD_FILE（可选）
                 + CONTEXT_MODULES_FILE 命中的 context/*.md（仅普通聊天）
                 + member_id_mapping.json (注入为成员信息)
```

运行前可按 `.example` 模板复制为无 `.example` 后缀的本地文件，也可以直接把运行目录配置中的路径变量指向外部私有配置仓库。若启用上下文模块，先把 `config/context/*.example.md` 复制为同名 `.md` 私有文件，再让 `context_modules.toml` 引用这些 `.md`。Secret、数据库、日志和聊天记录不应进入任何 Git 仓库；真实 prompt、世界观、上下文模块和成员映射只应放在私有配置仓库或本地私有目录，不进入公开仓库。
