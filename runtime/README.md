# runtime/ — 服务器运行配置目录

本目录是服务器运行目录示例，部署后会放置 release 二进制、控制脚本、配置模板和运行产物。真实 `.env`、私有 prompt、知识资料、SQLite、日志和 pid 都属于本地私有配置或运行数据，不应提交到公开仓库。

## 目录结构

```text
runtime/
├── config/.env.example              # 可提交的环境变量模板
├── config/agent.toml                # 可提交的非敏感 Agent 场景策略
├── .env                             # 兼容环境变量文件，不提交
├── qq-maid-bot                      # 部署后的统一 Rust release 二进制，不提交
├── botctl.sh                        # 部署后的聚合控制脚本，不提交
├── qq-maid-systemd.sh               # systemd service 生成 / 安装脚本，不提交
├── windows-startup-example.bat      # Windows 登录后启动示例，不提交
├── validate-runtime.sh              # 部署后的运行诊断脚本，不提交
├── README.md                        # 本文件
├── static/
│   └── index.html                   # 可提交的本地 Web 控制台静态页
├── config/
│   ├── .env                         # 推荐真实环境变量文件，不提交
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
cp config/.env.example config/.env
```

编辑 `runtime/config/.env`，填写 QQ 官方机器人、模型 Provider、天气和 RSS 等必要配置。默认 `runtime/config/agent.toml` 维护非敏感 Agent 策略，不启用注释示例中的模型路线；`.env` 继续保存 API Key、Base URL、旧兼容兜底模型和运行参数。如不希望模型在普通私聊中主动调用工具，优先修改 `[scenes.private].tool_calling_enabled=false`。未显式配置 `PROMPT_DIR` 时，Core 使用默认 `config/prompts`；默认目录缺少真实 prompt 文件时会回退到内置通用 prompt。显式配置 `PROMPT_DIR` 后，缺文件或空文件会作为配置错误处理。

Rust 进程按当前工作目录依次尝试加载 `config/.env` 和 `.env`。`make run` 和部署控制脚本都会以 `runtime/` 作为工作目录启动，因此默认相对路径都按 `runtime/` 解析。

常用外部路径变量：

- `PROMPT_DIR`：包含 `maid_system.md`、`mode_rules.md`、`session_context.md` 的目录。
- `KNOWLEDGE_DIR`：Markdown 知识目录，留空时使用 `config/knowledge`。
- `APP_DB_FILE`：通用 SQLite 文件路径，承载 Session、待办、长期记忆、RSS / Atom 订阅、RSS 去重状态和知识检索索引。
- `AGENT_CONFIG_FILE`：Agent 场景策略文件路径，默认 `config/agent.toml`。显式设置后文件缺失会启动失败；默认文件缺失时会回退旧环境变量兼容路径。

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
KNOWLEDGE_DIR=/opt/qqbot/private/config/knowledge
APP_DB_FILE=/opt/qqbot/data/app.db
```

## 微信服务号文本回调配置

微信服务号入口是 Gateway 的可选能力，默认关闭。它实现“微信服务号收到文本消息 -> 同步 XML 文本回复”的快路径；当 Core 处理超过同步安全预算时，会先结束微信 HTTP 响应，避免微信侧超时重试导致重复执行。若已配置 AppID / AppSecret，则后台完成后通过客服文本消息补发最终结果。

最小配置示例：

```env
WECHAT_SERVICE_ENABLED=true
WECHAT_SERVICE_TOKEN=填写你在微信公众平台服务器配置里设置的 Token
WECHAT_SERVICE_APP_ID=填写服务号 AppID
WECHAT_SERVICE_APP_SECRET=填写服务号 AppSecret
WECHAT_SERVICE_BIND_HOST=127.0.0.1
WECHAT_SERVICE_BIND_PORT=8788
WECHAT_SERVICE_CALLBACK_PATH=/wechat/service
WECHAT_SERVICE_REPLY_TIMEOUT_MS=4000
```

字段填写说明：

- `WECHAT_SERVICE_ENABLED`：是否启动微信服务号回调监听器。默认 `false`，不会影响 QQ Gateway。
- `WECHAT_SERVICE_TOKEN`：微信公众平台“服务器配置”里的 Token。这个值由部署者自定义，必须和微信后台填写的 Token 完全一致，用于 `signature` 校验；不要提交真实值。
- `WECHAT_SERVICE_APP_ID`：微信服务号 AppID。配置后慢请求可按需获取 `access_token` 并发送客服文本消息。
- `WECHAT_SERVICE_APP_SECRET`：微信服务号 AppSecret。仅用于客服文本消息补发时获取 `access_token`；不要提交真实值。
- `WECHAT_SERVICE_BIND_HOST` / `WECHAT_SERVICE_BIND_PORT`：机器人本机监听地址。推荐保持 `127.0.0.1:8788`，由 Nginx、Caddy、Cloudflare Tunnel 等反向代理暴露到公网。
- `WECHAT_SERVICE_CALLBACK_PATH`：回调路径，必须以 `/` 开头。微信公众平台填写的 URL 路径需要和它一致。
- `WECHAT_SERVICE_REPLY_TIMEOUT_MS`：被动 XML 回复安全预算，默认 `4000`，最大 `4500`。不要设置到微信平台硬限制附近。
- `WECHAT_SERVICE_API_BASE`：微信 API Base URL，默认 `https://api.weixin.qq.com`，通常不需要配置。

`/ping all` 的调试详情会展示微信入口安全摘要：入口是否启用、监听地址和端口、callback path、`token` / `app_id` / `app_secret` 是否已配置、`access_token` 是否按需获取、客服消息是否可用、同步回复预算、当前支持模式和暂不支持能力。摘要只显示 `configured` / `missing` / `disabled` 等状态，不会打印真实 Token、AppSecret、access token、OpenID 或消息正文。未启用时显示 `disabled`，不代表 QQ Gateway 异常。

微信公众平台“服务器配置”中对应填写：

```text
URL: https://你的域名/wechat/service
Token: 与 WECHAT_SERVICE_TOKEN 完全一致
EncodingAESKey: 当前未使用
消息加解密方式: 明文模式
```

如果 `WECHAT_SERVICE_BIND_HOST=127.0.0.1`、`WECHAT_SERVICE_BIND_PORT=8788`、`WECHAT_SERVICE_CALLBACK_PATH=/wechat/service`，反向代理需要把公网 `https://你的域名/wechat/service` 转发到：

```text
http://127.0.0.1:8788/wechat/service
```

Nginx 反向代理示例：

```nginx
location /wechat/service {
    proxy_pass http://127.0.0.1:8788/wechat/service;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto https;
}
```

Caddy 反向代理示例：

```caddy
你的域名 {
    reverse_proxy /wechat/service 127.0.0.1:8788
}
```

当前已支持：

- GET URL 验证：校验 `signature`、`timestamp`、`nonce`、`echostr`，通过后返回 `echostr` 原文。
- POST 明文 `text` XML 消息：校验签名后解析文本消息。
- 快路径同步文本 XML 回复：Core 在安全预算内完成时渲染 XML。
- 慢路径安全降级：未配置客服消息能力时，在预算内返回明确文本提示。
- 慢路径客服文本补发：配置 AppID / AppSecret 后，先返回 `success`，后台继续通过 Core 生成结果并调用客服消息接口发送纯文本。
- Markdown 降级为 text：微信同步回复和客服补发都只发送纯文本。
- 同一 `MsgId` 去重：微信重试不会重复进入 Core 或重复创建后台任务。

当前不支持：

- 加密 XML。
- 除客服文本补发以外的主动接口调用。
- 模板消息。
- 图片、语音、视频。
- subscribe / CLICK / VIEW 等事件。
- 菜单事件处理和主动推送。
- 流式输出。

安全注意事项：

- 不要提交真实 `WECHAT_SERVICE_TOKEN`、`WECHAT_SERVICE_APP_SECRET` 或 access token。
- 不要在日志、截图或 Issue 中打印 OpenID 和用户消息正文。
- 生产环境建议只监听 `127.0.0.1`，由 Nginx、Caddy 或 Cloudflare Tunnel 暴露公网 HTTPS。
- 微信公众平台生产回调 URL 应使用 HTTPS。
- `EncodingAESKey` 当前未使用；公众平台请先选择明文模式。

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

全局环境变量。控制 QQ Bot SDK 参数、LLM 供应商、主模型、内部任务模型、LLM 服务监听地址、超时、外部配置路径、RSS、天气、Tool Calling 和诊断开关等。包含密钥，不要提交到公开仓库。

完整字段以 [`config/.env.example`](./config/.env.example) 为准。

### `config/agent.toml`

非敏感 Agent 运行策略。该文件可以提交和随 release 分发，默认模板注册 MiMo provider 元数据，但不显式声明私聊、群聊和辅助模型路线；这些路线默认继承 `.env` 中的 `LLM_MODEL`、`PRIVATE_LLM_MODEL` 和 `GROUP_LLM_MODEL`。需要把场景模型固定到配置文件时，可以按需新增同名 route。主要包含：

- `providers`：可选的 OpenAI-compatible provider 元数据，例如 `mimo` 的 base URL、认证头和 API key 环境变量名；
- `model_routes`：可选的命名模型候选链，例如覆盖内置 `private_main`、`group_main`、`aux`；
- `search_routes`：可选的 `/查` OpenAI Web Search 模型，例如覆盖内置 `private_search`、`group_search`；
- `profiles.fast / balanced / deep`：模型路线、reasoning effort、最大 Tool Loop 轮数和输出预算；
- `scenes.private / group`：群聊 / 私聊是否启用普通 AI 聊天、选择哪个 profile、是否允许 Tool Calling。

配置合并优先级为：`agent.toml` 中显式声明的同名 `model_routes` / `search_routes`，高于 scene-specific 环境变量（`PRIVATE_LLM_MODEL`、`GROUP_LLM_MODEL`、`PRIVATE_OPENAI_SEARCH_MODEL`、`GROUP_OPENAI_SEARCH_MODEL`），再回退 `LLM_MODEL` / `OPENAI_SEARCH_MODEL`，最后使用项目原有默认值。默认模板没有声明 `private_main`、`group_main` 和 `aux`，因此普通聊天路线默认继续读取旧兼容环境变量。它不会保存 API Key、Access Token、私有 Base URL、真实 prompt、用户资料或业务材料；这些敏感 Provider 配置仍只从 `.env` 读取。进程环境变量优先于 dotenv 文件，dotenv 只补充缺失项。

例如需要固定普通聊天路线时可写成：

```toml
[model_routes.private_main]
candidates = ["openai:gpt-5.5", "deepseek:deepseek-chat"]

[model_routes.group_main]
candidates = ["openai:gpt-5.4", "deepseek:deepseek-chat"]

[model_routes.aux]
candidates = ["openai:gpt-5.4-mini", "deepseek:deepseek-chat"]
```

MiMo 按 OpenAI-compatible Chat Completions 接入。默认模板已包含：

```toml
[providers.mimo]
kind = "openai_compatible"
base_url = "https://api.xiaomimimo.com/v1"
api_key_env = "MIMO_API_KEY"
auth_header = "Authorization"
auth_scheme = "Bearer"
```

使用 MiMo 时，在 `.env` 或进程环境中配置 `MIMO_API_KEY`，再把候选链写成 `mimo:mimo-v2.5-pro` 或 `mimo:mimo-v2.5`。例如：

```toml
[model_routes.private_main]
candidates = ["mimo:mimo-v2.5-pro", "deepseek:deepseek-chat"]

[model_routes.group_main]
candidates = ["mimo:mimo-v2.5", "deepseek:deepseek-chat"]
```

此时 `.env` 仍需要配置 `LLM_PROVIDER=auto` 和实际用到的 `OPENAI_API_KEY` / `DEEPSEEK_API_KEY` / `MIMO_API_KEY` 等敏感项；`agent.toml` 不写 key。`/查` 仍走 OpenAI Responses web_search 能力，不使用 `/查` 时无需在 `agent.toml` 配置 `search_routes`。

### `config/prompts/*.md`

固定核心 prompt：

- `maid_system.md`：助手职责、默认语气、QQ 群聊规则、现实问题规则和安全规则。
- `mode_rules.md`：根据用户消息内容判断回答方式。
- `session_context.md`：多轮对话、说话者和 slash 指令边界规则。

Tool Calling 的服务端白名单、参数校验和执行边界不由 prompt 决定；prompt 只能影响模型是否提出调用意图，真实执行仍必须经过 Core 注册的 Tool。

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

配置、prompt、知识源 Markdown、日志、pid、release 二进制和 gateway WebSocket 临时状态不属于 `APP_DB_FILE` 承载范围。

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

服务器上可把真实 `.env` 放到 `runtime/config/.env`，并在其中把 `PROMPT_DIR`、`KNOWLEDGE_DIR`、`APP_DB_FILE` 指向外部私有配置或运行数据目录，再执行：

```bash
cd runtime
./botctl.sh start
```

## 开机自启动

自启动只包装现有运行目录，不改变机器人核心逻辑，也不影响 `./botctl.sh start/stop/restart/status/logs` 等手动管理方式。交给 systemd 托管后，应使用 `systemctl` 管理服务，避免同时再用 `./botctl.sh start` 启动第二个进程。配置前先确认 `config/.env` 已填写，且手动启动可用：

```bash
cd runtime
./botctl.sh start
./botctl.sh health
./botctl.sh stop
```

### Linux systemd

Linux 服务器推荐使用 systemd。脚本会根据当前运行目录生成 service，默认只打印内容，不写系统目录：

```bash
cd runtime
./qq-maid-systemd.sh render --runtime-dir "$(pwd)" --user qqmaid
```

确认 `WorkingDirectory`、`BOT_BINARY`、`BOT_ENV_FILE` 和运行用户后，再安装系统服务：

```bash
sudo ./qq-maid-systemd.sh install --runtime-dir "$(pwd)" --user qqmaid
sudo systemctl enable --now qq-maid-bot.service
sudo systemctl status qq-maid-bot.service
```

常用操作：

```bash
sudo systemctl start qq-maid-bot.service
sudo systemctl stop qq-maid-bot.service
sudo systemctl restart qq-maid-bot.service
sudo systemctl status qq-maid-bot.service
journalctl -u qq-maid-bot.service -f
```

禁用或卸载：

```bash
sudo systemctl disable --now qq-maid-bot.service
sudo ./qq-maid-systemd.sh uninstall --runtime-dir "$(pwd)"
```

如果没有专用 Linux 用户，也可以省略 `--user`，由 systemd 使用当前配置运行；生产环境更推荐创建低权限用户，并确保该用户可读取运行目录和 `config/.env`，可写 `data/`、`logs/` 等运行产物目录。脚本不会自动创建用户、不会静默 `sudo`，也不会自动启用或启动服务。

个人机器也可以使用 user service：

```bash
./qq-maid-systemd.sh install --scope user --runtime-dir "$(pwd)"
systemctl --user enable --now qq-maid-bot.service
systemctl --user status qq-maid-bot.service
journalctl --user -u qq-maid-bot.service -f
```

user service 只在用户 systemd 会话存在时运行；如需用户退出后仍继续运行，需由部署者自行配置 `loginctl enable-linger <用户名>`。

### Windows 登录后启动

Windows 示例脚本是 `windows-startup-example.bat`。它适合“用户登录后启动”，不是严格意义上的系统服务；用户未登录、注销或终端关闭时，行为取决于 Windows 会话和脚本所在环境。

推荐做法是把发布包放在固定目录，例如 `C:\qq-maid-bot\`，确认 `config\.env` 已配置后，给 `windows-startup-example.bat` 创建快捷方式，并把快捷方式放入启动文件夹：

```text
Win + R -> shell:startup
```

如果直接把 `.bat` 复制到启动文件夹，需要编辑脚本里的 `QQ_MAID_RUNTIME_DIR` 为真实发布包目录。

### 非 systemd Linux / Termux / NAS / 路由器

这些环境差异很大，本项目一期不做自动探测和全量适配。可按设备能力选择：

- `crontab @reboot`：调用 `runtime/botctl.sh start`。
- `rc.local`：系统启动末尾调用 `botctl.sh start`。
- OpenRC / runit / s6 / Supervisor：把 `runtime/botctl.sh run` 作为前台命令托管。
- Termux：结合 Termux:Boot，在启动脚本中进入运行目录后执行 `./botctl.sh start`。
- NAS / 路由器：优先使用厂商自带的启动任务、容器管理或服务管理界面。

这些 fallback 不承诺覆盖所有发行版和设备固件。排障时先确认手动 `./botctl.sh start`、`./botctl.sh health` 可用，再检查对应平台的启动日志。

## Release 包

Release 包采用白名单生成，只包含统一 `qq-maid-bot` release 二进制、`botctl.sh`、`botmon.sh`、`qq-maid-systemd.sh`、`windows-startup-example.bat`、`diagnose-network.sh`、`validate-runtime.sh`、`qq-maid-healthcheck.sh`、`static/index.html`、本文件、`config/.env.example`、`config/agent.toml`、公开 `.example` 配置模板、`VERSION` 和空的 `data/storage/` 目录。真实 `.env`、私有 prompt、私有知识资料、SQLite 数据库、日志、pid 和 `.bak` 备份不会被写入归档。

GitHub Release 自动生成 `linux-x86_64`、`linux-aarch64`、`macos-x86_64`、`macos-aarch64` 和 `windows-x86_64` 包；Linux / macOS 使用 `.tar.gz`，Windows 使用 `.zip`。

首次使用 Release 包：

```bash
tar -xzf qq-maid-bot-v0.1.0-linux-x86_64.tar.gz
cd qq-maid-bot-v0.1.0-linux-x86_64
cp config/.env.example config/.env
```

编辑 `config/.env` 后启动：

```bash
./botctl.sh start
```

升级时不要直接覆盖已有运行目录中的私有文件和运行数据，尤其是 `config/.env`、私有 prompt、私有知识资料、SQLite 数据库、日志和 pid。

### Breaking changes

新大版本已移除旧 `MEMBER_ID_MAPPING_FILE` / `member_id_mapping.json` 成员编号识别链路。普通聊天中的三位数字不再触发身份切换或未知编号拦截；旧部署目录里未跟踪的 `config/member_id_mapping.json` 不会再被读取，可以手工移除。历史 session 中残留的 `current_speaker_hint` 等废弃状态键会在 SQLite migration 中清理。

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
  └→ qq-maid-bot 统一进程
       └→ CoreService::respond
            ├→ slash 命令 / pending / 群聊普通 flow
            └→ 私聊普通聊天组装:
                 固定核心 prompt
                 + 请求时间上下文
                 + 本轮检索出的 knowledge 片段
                 + 长期记忆 / 会话上下文
                 + 会话历史和当前用户消息
                 + 可选 ToolRegistry（天气和 Todo Tool）
```
