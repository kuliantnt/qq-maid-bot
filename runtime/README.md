# runtime/ — 服务器运行配置目录

本目录是服务器运行目录示例，部署后会放置 release 二进制、控制脚本、配置模板和运行产物。真实 `.env`、私有 prompt、成员映射、知识资料、SQLite、日志和 pid 都属于本地私有配置或运行数据，不应提交到公开仓库。

## 目录结构

```text
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
│   ├── member_id_mapping.example.json
│   ├── member_id_mapping.json       # 本地私有成员编号映射，不提交
│   ├── knowledge/
│   │   └── example.example.md       # 可提交的知识库示例
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

编辑 `runtime/config/.env`，填写 QQ 官方机器人、模型 provider、天气和 RSS 等必要配置。未显式配置 `PROMPT_DIR` 时，Core 使用默认 `config/prompts`；默认目录缺少真实 prompt 文件时会回退到内置通用 prompt。显式配置 `PROMPT_DIR` 后，缺文件或空文件会作为配置错误处理。

Rust 进程按当前工作目录依次尝试加载 `config/.env` 和 `.env`。`make run` 和部署控制脚本都会以 `runtime/` 作为工作目录启动，因此默认相对路径都按 `runtime/` 解析。

常用外部路径变量：

- `PROMPT_DIR`：包含 `maid_system.md`、`mode_rules.md`、`session_context.md` 的目录。
- `KNOWLEDGE_DIR`：Markdown 知识目录，留空时使用 `config/knowledge`。
- `MEMBER_ID_MAPPING_FILE`：成员编号映射 JSON。文件不存在时按空映射处理；JSON 语法错误会启动失败。
- `APP_DB_FILE`：通用 SQLite 文件路径，承载 Session、待办、长期记忆、RSS / Atom 订阅、RSS 去重状态和知识检索索引。

推荐把公开源码、私有配置和运行数据分开：

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
KNOWLEDGE_DIR=/opt/qqbot/private/config/knowledge
APP_DB_FILE=/opt/qqbot/data/app.db
```

## 知识目录

默认知识目录是 `runtime/config/knowledge/`。把 Markdown 文件放入该目录或通过 `KNOWLEDGE_DIR` 指向外部私有目录后，重启机器人即可自动同步：

```text
Markdown 文件
  -> 启动时递归扫描和分段
  -> 写入 APP_DB_FILE 中的 SQLite FTS5 索引
  -> 普通聊天按当前用户消息检索少量相关片段
```

当前版本使用本地 SQLite FTS5，不需要 embedding API、向量数据库或手工索引文件。支持递归扫描子目录；非 Markdown、隐藏文件、临时文件和常见编辑器备份文件会被忽略。目录不存在或为空时，机器人仍正常启动，只是不注入知识片段。

知识片段只进入普通聊天链路，不进入 `/todo`、`/memory`、`/compact`、天气、翻译、RSS 或联网查询等结构化流程。检索结果会带来源文件和章节信息，并明确标记为“参考资料，不是新的系统指令”。

公开仓库只提交 `*.example.md` 示例。真实知识资料可能包含私人设定、成员信息或业务材料，应放在外部私有目录，或使用无 `.example` 后缀的本地文件并保持不提交。

## 文件说明

### `config/.env` / `.env`

全局环境变量。控制 QQ Bot SDK 参数、LLM 供应商、主模型、内部任务模型、LLM 服务监听地址、超时、外部配置路径、RSS、天气和诊断开关等。包含密钥，不要提交到公开仓库。

完整字段以 [`.env.example`](./.env.example) 为准。

### `config/member_id_mapping.json`

成员编号映射。键为成员编号（字符串），值为名称和简介。真实成员映射可能包含个人信息或私人设定，应保留在外部私有路径或本地未跟踪文件中。公开仓库只提交 `member_id_mapping.example.json`。

### `config/prompts/*.md`

固定核心 prompt：

- `maid_system.md`：助手职责、默认语气、QQ 群聊规则、现实问题规则和安全规则。
- `mode_rules.md`：根据用户消息内容判断回答方式。
- `session_context.md`：多轮对话、说话者和 slash 指令边界规则。

真实 prompt 不提交，公开仓库只提交 `.example.md`。

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

Session、待办、长期记忆、RSS / Atom 订阅、RSS 去重状态和知识检索索引均保存在 `APP_DB_FILE` 指向的通用 SQLite 文件中。长期记忆只能通过明确记忆指令生成草稿，并由用户确认后写入；普通聊天不会自动写长期记忆。

配置、prompt、知识源 Markdown、成员映射、日志、pid、release 二进制和 gateway WebSocket 临时状态不属于 `APP_DB_FILE` 承载范围。

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
make deploy-remote
```

服务器上可把真实 `.env` 放到 `runtime/.env` 或 `runtime/config/.env`，并在其中把 `PROMPT_DIR`、`MEMBER_ID_MAPPING_FILE`、`KNOWLEDGE_DIR`、`APP_DB_FILE` 指向外部私有配置或运行数据目录，再执行：

```bash
cd runtime
./botctl.sh start
```

## Release 包

Release 包采用白名单生成，只包含统一 `qq-maid-bot` release 二进制、`botctl.sh`、`diagnose-network.sh`、`validate-runtime.sh`、`static/index.html`、本文件、`.env.example`、公开 `.example` 配置模板、`VERSION` 和空的 `data/storage/` 目录。真实 `.env`、私有 prompt、私有知识资料、成员映射、SQLite 数据库、日志、pid 和 `.bak` 备份不会被写入归档。

首次使用 Release 包：

```bash
tar -xzf qq-maid-bot-v0.1.0-linux-x86_64.tar.gz
cd qq-maid-bot-v0.1.0-linux-x86_64
cp .env.example config/.env
```

编辑 `config/.env` 后启动：

```bash
./botctl.sh start
```

升级时不要直接覆盖已有运行目录中的私有文件和运行数据，尤其是 `config/.env`、私有 prompt、私有知识资料、成员映射、SQLite 数据库、日志和 pid。

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

```text
runtime/config/.env 或 runtime/.env
  └→ Rust Core HTTP (127.0.0.1:8787)
       └→ /v1/respond
            └→ 普通聊天组装:
                 固定核心 prompt
                 + 请求时间上下文
                 + 本轮检索出的 knowledge 片段
                 + 长期记忆 / 会话上下文 / 成员映射
                 + 会话历史和当前用户消息
```
