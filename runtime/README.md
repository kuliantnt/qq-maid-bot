# runtime/ — 源码运行目录与部署应用根目录模板

服务器使用 GHCR 镜像和 Compose 时，请先看
[Docker 与 Compose 部署](../docs/deployment/docker.md)。本文继续作为配置字段、原生 Release
目录和控制脚本的权威说明；容器部署复用相同的 `runtime/` 相对路径语义。

本目录是服务器运行目录示例，部署后会放置 release 二进制、控制脚本、配置模板和运行产物。真实 `.env`、私有 prompt、知识资料、SQLite、日志和 pid 都属于本地私有配置或运行数据，不应提交到公开仓库。

## 目录结构

```text
runtime/
├── config/.env.example              # 可提交的环境变量模板
├── config/agent.example.toml        # 可提交的非敏感 Agent 场景策略模板
├── config/ops.example.toml          # 默认关闭的 `/ops` 白名单公开模板
├── config/runtime.example.toml      # 程序受管普通配置的公开格式示例
├── .env                             # 兼容环境变量文件，不提交
├── qq-maid-bot                      # 部署后的统一 Rust release 二进制，不提交
├── botctl.sh                        # 部署后的聚合控制脚本，不提交
├── qbot.ps1                         # Windows 安装、更新和配置入口，不提交
├── qbot.cmd                         # Windows qbot 便捷入口，不提交
├── botctl.ps1                       # Windows 原生 PowerShell 控制脚本，不提交
├── botctl.cmd                       # Windows CMD / PowerShell 便捷入口，不提交
├── qq-maid-systemd.sh               # systemd service 生成 / 安装脚本，不提交
├── windows-startup-example.bat      # Windows 登录后启动示例，不提交
├── validate-runtime.sh              # 部署后的运行诊断脚本，不提交
├── README.md                        # 本文件；控制台资源已嵌入 release 二进制
├── config/
│   ├── .env                         # 推荐真实环境变量文件，不提交
│   ├── runtime.toml                 # WebUI 与人工编辑共享的受管普通配置，不提交
│   ├── secrets/
│   │   └── master.key               # SQLite 敏感密文的独立主密钥，严格限权，不提交
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

编辑 `runtime/config/.env`，填写至少一个入口渠道和模型 Provider 等必要配置。QQ 官方机器人凭证现在是可选绑定；微信-only 部署可以不填写 QQ AppID/AppSecret。默认 `runtime/config/agent.example.toml` 是非敏感 Agent 策略模板，首次启动会用它生成本地 `runtime/config/agent.toml`；`.env` 继续保存 `OPENAI_API_KEY`、Base URL 和普通运行参数。如不希望模型在普通私聊中主动调用工具，修改 `[scenes.private].tool_calling_enabled=false`。未显式配置 `PROMPT_DIR` 时，Core 使用默认 `config/prompts`；默认目录缺少真实 prompt 文件时会回退到内置通用 prompt。显式配置 `PROMPT_DIR` 后，缺文件或空文件会作为配置错误处理。

Rust 进程按当前工作目录依次尝试加载 `config/.env` 和 `.env`。`make run` 和部署控制脚本都会以 `runtime/` 作为工作目录启动，因此默认相对路径都按 `runtime/` 解析。

### QQ 官方 Bot 绑定管理

- `QQ_BOT_APP_ID` 与 `QQ_BOT_APP_SECRET` 同时存在：已绑定；`QQ_BOT_ENABLED=true` 时启动 QQ Gateway。
- 两项都不存在：未绑定。进程跳过 QQ Token 获取、Gateway 连接和重连，不影响微信入口、主体及 SQLite 业务数据。
- 凭证存在且 `QQ_BOT_ENABLED=false`：已禁用。凭证保留，QQ 不初始化。
- 只配置其中一项属于错误配置，启动时会明确报错，避免使用残缺凭证。

现有安装可执行 `qbot config bot --unbind` 解绑，或执行 `qbot config bot --disable` 暂停；两者均需重启进程生效。解绑命令只从 `.env` 删除 QQ 新旧凭证键，不修改微信配置、`APP_DB_FILE`、会话、待办、记忆或 RSS 数据。重新绑定可执行：

```bash
qbot config bot --app-id 你的AppID --app-secret 你的AppSecret
qbot restart
```

控制台会分别展示未绑定（`not_configured`）、已禁用（`not_available`）、已绑定但离线（`offline`）和运行中（`online`）。未绑定与已禁用不是健康故障。当前配置管理只在启动时读取，因此不支持运行期间热解绑。

常用外部路径变量：

- `PROMPT_DIR`：包含 `maid_system.md`、`mode_rules.md`、`session_context.md` 的目录。
- `KNOWLEDGE_DIR`：Markdown 知识目录，留空时使用 `config/knowledge`。
- `APP_DB_FILE`：通用 SQLite 文件路径，承载 Session、待办、长期记忆、RSS / Atom 订阅、通知 Outbox、Ops 入站幂等领取和知识检索索引。
- `AGENT_CONFIG_FILE`：Agent 场景策略文件路径，默认 `config/agent.toml`。默认路径缺失时由首次启动自动生成；显式指定其他路径时，目标文件必须存在且完整合法。该路径只由 Bootstrap 配置决定，WebUI 不能指定其他文件。
- `OPS_CONFIG_FILE`：`/ops` 白名单运维配置路径，默认 `config/ops.toml`。默认文件缺失时功能保持关闭；显式设置后文件缺失会启动失败。
- `CHAT_COMMAND_PREFIX`：所有平台共用的单字符命令前缀，默认 `/`；也可通过 Web 控制台下拉框或 `runtime.toml` 的 `command.prefix` 设置为 `#`、`*` 等可见非空白字符，重启后生效。
- `RUNTIME_CONFIG_FILE`：程序受管普通配置，默认 `config/runtime.toml`。文件不存在时首次启动会安全创建空配置，已有文件不会覆盖。
- `MASTER_KEY_FILE`：解密主密钥文件，默认相对于受管配置目录的 `secrets/master.key`。首次缺失时安全生成；不得把主密钥原文写进 `.env`。
- `WEB_CONSOLE_ENABLED`：部署管理入口开关。未显式配置的新实例默认开启并进入受保护首次向导；在进程环境或 `.env` 中显式设为 `false` 时，即使配置中心曾保存为开启，也以部署侧关闭为准，页面、认证和配置 API 均不可访问，也不会生成 Bootstrap token。默认监听仍是回环地址，公网访问必须使用受信 TLS 反向代理。
- `WEB_CONSOLE_TRUSTED_PROXY_IPS`：受信反向代理实际连接 IP 的逗号列表；只有命中列表才读取 `X-Forwarded-For`，不直接信任客户端头。
- `WEB_CONSOLE_SECURE_COOKIES`：生产 HTTPS 或 TLS 终止代理必须显式设为 `true`，管理员与 PreAuth Cookie 将带 `Secure` 并使用 `__Host-` 前缀；本机 HTTP 开发保持 `false`。

首次启动且尚无部署管理员时，程序在运行目录创建 `config/secrets/bootstrap.token`（约 22 字符、有效期 30 分钟，Unix 权限 `0600`），并把同一个 token 向启动控制台输出一次。Windows 不使用 Unix mode，文件保护取决于安装目录继承的 ACL；当前版本尚未主动创建或校验收紧的 Windows ACL，部署者应限制该目录仅允许机器人运行身份和必要的本机管理员访问，后续加固由 [#522](https://github.com/kuliantnt/qq-maid-bot/issues/522) 跟踪。程序只在 token 新生成时输出，状态查询、尚有效 token 的复用和后续重启不会重复输出，也不会写入结构化访问日志或长期状态；打开 `/console/` 建立首位管理员后令牌文件立即删除。管理员密码至少 6 个字符且只保存 Argon2id 哈希，浏览器会话使用 HttpOnly SameSite cookie 与稳定 CSRF。

忘记管理员密码时，在登录页点击“重置管理员密码”。服务端会在同一路径生成短时单次重置 token，并同样只向控制台输出一次；生成 token 不会立即停用旧密码或旧会话，读取文件或控制台中的 token 并提交新密码后才完成重置。重置成功后 token 文件立即删除，旧 token、旧密码和全部旧 Admin 会话同时失效。

配置来源、文件/网页共同编辑规则和敏感值边界见[配置中心设计与字段清单](../docs/development/config-center.md)。备份时必须分别保护 SQLite 与 `config/secrets/master.key`；只备份数据库无法恢复其中的敏感配置。容器部署必须持久化整个配置目录或至少单独持久化主密钥文件，容器重建不得生成新密钥覆盖旧密文。

`/ops` 从私聊获取 `user_id`、准备固定脚本、配置默认关闭的 Codex 长任务、取消任务和排障的完整步骤见 [`/ops` 白名单运维命令使用指南](../docs/development/ops-command.md)。

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

## OneBot 11 反向 WebSocket 入口

```env
ONEBOT11_ENABLED=true
ONEBOT11_BIND_HOST=127.0.0.1
ONEBOT11_BIND_PORT=8789
ONEBOT11_WEBSOCKET_PATH=/onebot/v11/ws
ONEBOT11_ACCESS_TOKEN=请使用独立随机值
ONEBOT11_REQUEST_TIMEOUT_MS=10000
ONEBOT11_MAX_MESSAGE_BYTES=1048576
```

当前只支持 OneBot 11 反向 WebSocket 和 Array 消息格式。OneBot 实现应作为客户端连接 `ws://127.0.0.1:8789/onebot/v11/ws`，并携带 `Authorization: Bearer <ONEBOT11_ACCESS_TOKEN>`。推荐只监听回环或受控内网；如需跨主机连接，应同时配置防火墙或受控隧道。首个账号会锁定当前进程的 `self_id`：同账号新连接替换旧连接，不同账号拒绝，重启进程后重新学习。运行状态只保存脱敏 `self_id`、是否监听/连接、最近心跳和固定断开摘要。

QQ 凭证可留空，因此 OneBot-only 配置能正常启动；也可以与 QQ 官方入口、微信入口在同一进程并存。入站支持私聊、群聊明确 `@` 机器人，以及引用当前进程内 ref_index 可命中的机器人消息时免 `@` 继续追问；ref_index 是进程内缓存，重启后引用旧消息会安全 miss。text、image、file 和未知消息段会保持顺序映射，安全的远程图片可以进入现有图片理解链路；文件和不可读媒体只生成摘要，不读取文件正文，图文混合消息可以正常进入 Core。

OneBot 出站支持私聊、群聊纯文本与图片 segment，以及 Todo / RSS 等纯文本主动推送；图片可来自 URL 或 base64。暂不支持文件、Markdown 平台消息、平台原生引用、`@` 消息段、流式输出或其他富媒体消息段。NapCat 配置步骤、验证证据和完整限制集中写在 [用 NapCat 接入小女仆](../docs/development/onebot11-napcat.md)，其他 OneBot 11 实现当前未经实机验证。

完整变量和默认值以 [`config/.env.example`](./config/.env.example) 为准。

## 微信服务号文本回调配置

微信服务号入口是 Gateway 的可选能力，默认关闭。它实现“微信服务号收到文本消息 -> 同步 XML 文本回复”的快路径；当 Core 处理超过同步安全预算时，会先结束微信 HTTP 响应，避免微信侧超时重试导致重复执行。若已配置 AppID / AppSecret，则后台完成后通过客服文本消息补发最终结果。

最小配置示例：

```env
WECHAT_SERVICE_ENABLED=true
WECHAT_SERVICE_TOKEN=填写你在微信公众平台服务器配置里设置的 Token
WECHAT_SERVICE_ENCRYPTION_MODE=plaintext
WECHAT_SERVICE_ENCODING_AES_KEY=
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
- `WECHAT_SERVICE_ENCRYPTION_MODE`：`plaintext`（默认）或 `aes`。`aes` 对应微信公众平台“安全模式”；不支持“兼容模式”。
- `WECHAT_SERVICE_ENCODING_AES_KEY`：`aes` 模式必填，必须是微信公众平台配置的 43 位 EncodingAESKey。只用于 Gateway 内的消息加解密，不会进入 Core 或日志。
- `WECHAT_SERVICE_APP_ID`：微信服务号 AppID。`aes` 模式下用于校验解密消息尾部身份，因此必填；同时配置 AppSecret 后，慢请求还可按需获取 `access_token` 并发送客服文本消息。
- `WECHAT_SERVICE_APP_SECRET`：微信服务号 AppSecret。仅用于客服文本消息补发时获取 `access_token`；不要提交真实值。
- `WECHAT_SERVICE_BIND_HOST` / `WECHAT_SERVICE_BIND_PORT`：机器人本机监听地址。推荐保持 `127.0.0.1:8788`，由 Nginx、Caddy、Cloudflare Tunnel 等反向代理暴露到公网。
- `WECHAT_SERVICE_CALLBACK_PATH`：回调路径，必须以 `/` 开头。微信公众平台填写的 URL 路径需要和它一致。
- `WECHAT_SERVICE_REPLY_TIMEOUT_MS`：被动 XML 回复安全预算，默认 `4000`，最大 `4500`。不要设置到微信平台硬限制附近。
- `WECHAT_SERVICE_API_BASE`：微信 API Base URL，默认 `https://api.weixin.qq.com`，通常不需要配置。

`/ping all` 的调试详情会展示微信入口安全摘要：入口是否启用、监听地址和端口、callback path、消息加解密模式、`token` / `app_id` / `app_secret` / `encoding_aes_key` 是否已配置、`access_token` 是否按需获取、客服消息是否可用、同步回复预算、当前支持模式和暂不支持能力。摘要只显示 `configured` / `missing` / `disabled` 等状态，不会打印真实 Token、EncodingAESKey、AppSecret、access token、OpenID 或消息正文。未启用时显示 `disabled`，不代表 QQ Gateway 异常。

微信公众平台“服务器配置”中对应填写：

```text
URL: https://你的域名/wechat/service
Token: 与 WECHAT_SERVICE_TOKEN 完全一致
EncodingAESKey: aes 模式下与 WECHAT_SERVICE_ENCODING_AES_KEY 完全一致
消息加解密方式: plaintext 选择明文模式；aes 选择安全模式
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
- AES 安全模式：使用 `msg_signature` 校验密文，按 EncodingAESKey 解密并校验 AppID；同步文本回复重新加密、签名后返回。模式不匹配时明确拒绝，不会把密文当明文处理。
- 快路径同步文本 XML 回复：Core 在安全预算内完成时渲染 XML。
- 慢路径安全降级：未配置客服消息能力时，在预算内返回明确文本提示。
- 慢路径客服文本补发：配置 AppID / AppSecret 后，先返回 `success`，后台继续通过 Core 生成结果并调用客服消息接口发送纯文本。
- Markdown 降级为 text：微信同步回复和客服补发都只发送纯文本。
- 同一 `MsgId` 去重：微信重试不会重复进入 Core 或重复创建后台任务。

当前不支持：

- 微信“兼容模式”（同一请求同时携带明文和密文）。
- 除客服文本补发以外的主动接口调用。
- 模板消息。
- 图片、语音、视频。
- subscribe / CLICK / VIEW 等事件。
- 菜单事件处理和主动推送。
- 流式输出。

安全注意事项：

- 不要提交真实 `WECHAT_SERVICE_TOKEN`、`WECHAT_SERVICE_ENCODING_AES_KEY`、`WECHAT_SERVICE_APP_SECRET` 或 access token。
- 不要在日志、截图或 Issue 中打印 OpenID 和用户消息正文。
- 生产环境建议只监听 `127.0.0.1`，由 Nginx、Caddy 或 Cloudflare Tunnel 暴露公网 HTTPS。
- 微信公众平台生产回调 URL 应使用 HTTPS。
- `WECHAT_SERVICE_ENCRYPTION_MODE` 必须与公众平台后台选择一致；生产环境推荐使用 `aes` 安全模式。

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

非敏感 Agent 运行策略。该文件可以提交和随 release 分发，默认模板注册 MiMo provider 元数据，并显式将私聊、群聊和辅助任务的第一候选统一到 OpenAI GPT-5.6 Luna，同时保留 Gemini、MiMo 和 DeepSeek 降级候选；搜索路线默认使用 Luna。需要调整候选顺序或搜索 Provider 时，应直接修改对应 route。主要包含：

- `providers`：可选的 OpenAI-compatible provider 元数据，例如 `mimo` 的 base URL、认证头和 API key 环境变量名；
- `model_routes`：可选的命名模型候选链，例如覆盖内置 `private_main`、`group_main`、`aux`；
- `tools.web_search.routes`：`provider_native` 后端使用的 `/查` 搜索模型，例如覆盖内置 `private_search`、`group_search`；裸模型或 `openai:` 走 OpenAI Responses web_search，`gemini:` 走 Gemini Google Search 工具；
- `profiles.fast / balanced / deep`：主模型路线、可选 `aux_route`、reasoning effort、最大 Tool Loop 轮数和输出预算；
- `scenes.private / group`：群聊 / 私聊是否启用普通 AI 聊天、选择哪个 profile、是否允许 Tool Calling。

模型路线、搜索路线、Profile、Scene、Tool Calling 和 Tool 白名单全部从 `agent.toml` 加载，不在 `runtime.toml` 或 SQLite 维护第二份普通值。网页配置入口直接结构化编辑同一个 `agent.toml`；保存会规范化 TOML 格式并删除注释/自定义排版，但保留全部合法语义和未修改条目。`agent.toml` 不保存 API Key、Access Token、真实 prompt、用户资料或业务材料；敏感值认证加密后存入 SQLite，进程环境或 dotenv 只用于 Provider 凭证、连接参数、平台与普通运行配置。

会话标题、Memory 草稿、会话压缩和翻译按当前场景解析模型：使用该场景 Profile 的 `aux_route`，Profile 未配置 `aux_route` 时回退当前场景 `main_route`，不再接受环境变量专项模型覆盖。无参数 `/rename` 与普通聊天后的异步自动标题共用这条路线，手动 `/rename 标题` 不调用模型。RSS 推送模型翻译由 `RSS_TRANSLATION_ENABLED` 控制，默认关闭；开启后按订阅目标的私聊/群聊场景使用同一辅助路线。

默认普通聊天路线为：

```toml
[model_routes.private_main]
candidates = ["openai:gpt-5.6-luna", "gemini:gemini-2.5-pro", "mimo:mimo-v2.5-pro", "deepseek:deepseek-chat"]

[model_routes.group_main]
candidates = ["openai:gpt-5.6-luna", "gemini:gemini-2.5-flash", "mimo:mimo-v2.5", "deepseek:deepseek-chat"]

[model_routes.aux]
candidates = ["openai:gpt-5.6-luna", "gemini:gemini-2.5-flash", "mimo:mimo-v2.5", "deepseek:deepseek-chat"]

[tools.web_search.routes.private_search]
model = "gpt-5.6-luna"

[tools.web_search.routes.group_search]
model = "gpt-5.6-luna"
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

默认 Luna 路线需要在 `.env` 配置 `OPENAI_API_KEY`，并确认账号具备 `gpt-5.6-luna` API 访问权限。运行时始终按 `agent.toml` 候选中的 Provider 前缀自动路由；改用其他 Provider 时，`.env` 只需配置实际用到的 `DEEPSEEK_API_KEY` / `MIMO_API_KEY` 等敏感项，`agent.toml` 不写 key。Gemini 是内置 Provider，配置 `GEMINI_API_KEY` 后可直接在 `model_routes` 或 `tools.web_search.routes` 使用 `gemini:` 前缀。`/查` 可走 OpenAI Responses web_search、Gemini Google Search 或 Tavily；不使用 Provider 原生搜索时可保留 routes，后续切回时无需重新配置。

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

生成经过内容校验和 SHA-256 校验的 Unix release 发布包：

```bash
make release RELEASE_VERSION=v0.18.1
```

本地构建产物位于：

```text
target/release/qq-maid-bot
```

部署到本地 `runtime/` 或脚本配置的远端服务器：

```bash
make local
make remote
```

`make local`、`make remote` 分别是原有 `make deploy-local`、`make deploy-remote` 的便捷入口，原目标继续兼容。

服务器部署推荐在 `scripts/deploy.conf` 显式设置 `REMOTE_APP_DIR=/root/qq-maid-bot`（也可使用 `/opt/qq-maid-bot` 等专用目录）。未设置时继续兼容 `${REMOTE_PROJECT_DIR}/runtime`。把真实 `.env` 放到应用根目录的 `config/.env`，并在其中把 `PROMPT_DIR`、`KNOWLEDGE_DIR`、`APP_DB_FILE` 指向外部私有配置或运行数据目录，再执行：

```bash
cd /root/qq-maid-bot
./botctl.sh start
```

Windows 从源码编译安装时，需要先安装 Rust MSVC 工具链及其 C++ 编译环境，然后在仓库根目录执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1
Copy-Item .\runtime\config\.env.example .\runtime\config\.env
notepad .\runtime\config\.env
.\runtime\botctl.cmd start
```

脚本统一执行 `cargo build --release --workspace`，并把 `target\release\qq-maid-bot.exe` 和 Windows 控制文件安装到 `runtime\`。它不会覆盖 `runtime\config\.env`、SQLite、日志或其他私有运行数据。

更新已运行的源码部署时，应先执行 `.\runtime\botctl.cmd stop`，再运行安装脚本并重新 `start`，避免 Windows 拒绝覆盖正在运行的 `qq-maid-bot.exe`。

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
./botctl.sh stop
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
sudo ./qq-maid-systemd.sh uninstall --service-name qq-maid-bot --scope system
```

卸载只依赖服务名和安装范围定位 service 文件，不要求运行目录、二进制或 `config/.env` 仍然存在。

如果没有专用 Linux 用户，也可以省略 `--user`，由 systemd 使用当前配置运行；生产环境更推荐创建低权限用户，并确保该用户可读取运行目录和 `config/.env`，可写 `data/`、`logs/` 等运行产物目录。脚本不会自动创建用户、不会静默 `sudo`，也不会自动启用或启动服务。

个人机器也可以使用 user service：

```bash
./botctl.sh stop
./qq-maid-systemd.sh install --scope user --runtime-dir "$(pwd)"
systemctl --user enable --now qq-maid-bot.service
systemctl --user status qq-maid-bot.service
journalctl --user -u qq-maid-bot.service -f
```

user service 只在用户 systemd 会话存在时运行；如需用户退出后仍继续运行，需由部署者自行配置 `loginctl enable-linger <用户名>`。

### Windows 登录后启动

Windows 发布包提供原生 `botctl.ps1` 和便捷入口 `botctl.cmd`，支持 `start`、`run`、`stop`、`restart`、`status`、`health`、`console` 和 `logs`。先在 PowerShell 中验证手动控制：

```powershell
Copy-Item .\config\.env.example .\config\.env
notepad .\config\.env
.\botctl.cmd start
.\botctl.cmd status
.\botctl.cmd health
.\botctl.cmd stop
```

`botctl.cmd` 会以 `-ExecutionPolicy Bypass` 调用同目录的脚本，不会修改系统级 PowerShell 执行策略。需要直接调用脚本时可使用：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\botctl.ps1 status
```

后台启动时，Rust 默认写到 stderr 的主日志位于 `logs\qq-maid-bot.log`，stdout 位于 `logs\qq-maid-bot.stdout.log`；每次启动会把上一轮文件保留为 `.previous`。`botctl.cmd logs` 跟随主日志。路径可分别通过 `BOT_LOG_FILE` 和 `BOT_STDOUT_LOG_FILE` 覆盖，其余覆盖变量与 `botctl.sh` 保持一致。

登录启动示例是 `windows-startup-example.bat`。它调用 `botctl.cmd start`，适合“用户登录后启动”，不是严格意义上的系统服务；用户未登录时不会启动，注销后的行为取决于 Windows 会话。

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

Release 包采用白名单生成。Linux/macOS 包只包含 Unix 控制与诊断脚本，Windows 包只包含 `qbot.ps1`、`qbot.cmd`、`botctl.ps1`、`botctl.cmd` 和登录启动示例；两类包均包含对应平台的 `qq-maid-bot` 二进制、本文件、`config/.env.example`、`config/agent.example.toml`、其他公开 `.example` 配置模板、`VERSION` 和空的 `data/storage/` 目录。控制台静态资源已嵌入 release 二进制，不再复制独立 `static/` 目录。真实 `.env`、私有 prompt、私有知识资料、SQLite 数据库、日志、pid 和 `.bak` 备份不会被写入归档。

GitHub Release 自动生成 `linux-x86_64`、`linux-aarch64`、`macos-aarch64` 和 `windows-x86_64` 包；Linux / macOS 使用 `.tar.gz`，Windows 使用 `.zip`。macOS Intel（x86_64）不再提供官方预编译包。

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

通过 `qbot install` 新装时会询问是否启用 Web 控制台；不需要 Web 的用户可选择否，或在
非交互安装中使用 `qbot install --web false`。关闭 Web 不影响 `qbot config`、文件配置、
Release/源码启动和机器人业务入口。

Windows ZIP 解压后，在 PowerShell 中执行：

```powershell
Copy-Item .\config\.env.example .\config\.env
notepad .\config\.env
.\botctl.cmd start
```

升级时不要直接覆盖已有运行目录中的私有文件和运行数据，尤其是 `config/.env`、`config/runtime.toml`、`config/secrets/master.key`、私有 prompt、私有知识资料、SQLite 数据库、日志和 pid。

统一二进制提供 `config check`、`config sources`、`config migrate`、`migration status` 和
`backup create/verify/restore` 运维子命令；迁移与恢复默认 dry-run，恢复只写入干净实例目录。
命令、数据库与配置恢复包的 secret 边界和 schema 回滚限制见
[配置迁移、备份恢复与安全升级](../docs/deployment/migration-backup.md)。

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

Windows 原生对应使用：

```powershell
.\botctl.cmd status
.\botctl.cmd restart
.\botctl.cmd console
.\botctl.cmd health
.\botctl.cmd logs
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
