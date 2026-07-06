<div align="center">
  <img src="docs/img/logo.png" alt="小女仆机器人" width="180" />
  <h1>小女仆机器人</h1>
  <p><strong>一个会聊天、会记事、会调用工具，也会主动推送的轻量、自托管、多入口 AI Agent 机器人。</strong></p>
  <p>
    <a href="https://github.com/kuliantnt/qq-maid-bot/actions/workflows/ci.yml"><img src="https://github.com/kuliantnt/qq-maid-bot/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
    <a href="https://github.com/kuliantnt/qq-maid-bot/releases"><img src="https://img.shields.io/github/v/release/kuliantnt/qq-maid-bot" alt="Release" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/github/license/kuliantnt/qq-maid-bot" alt="License" /></a>
    <a href="https://deps.rs/repo/github/kuliantnt/qq-maid-bot"><img src="https://deps.rs/repo/github/kuliantnt/qq-maid-bot/status.svg" alt="Dependencies" /></a>
    <img src="https://img.shields.io/badge/memory-24%20MiB-success" alt="Memory" />
    <img src="https://img.shields.io/badge/maid-online-ff69b4" alt="Maid Status" />
    <img src="https://img.shields.io/badge/女仆浓度-100%25-ff69b4" alt="Maid Purity" />
  </p>
  <p><sub>约 22 MiB 可执行文件 · 约 24 MiB 常驻内存 · 默认空闲时 3 个线程 · 以及持续膨胀的代码量</sub></p>
</div>

> 💡 仓库早期以 QQ 机器人为主，因此仍保留 `qq-maid-bot` 名称。当前项目正在从 QQ 官方机器人演进为多入口平台型小女仆机器人；OneBot 11 仍处于架构预留和规划阶段，尚未接入。

小女仆机器人使用 Rust 构建，当前主入口是 QQ 官方机器人接口，并提供可选微信服务号文本入口；OneBot 11 只保留平台模型和文档规划，尚未实现可用接入。它不是简单地把消息转发给大模型，而是把长期会话、受控记忆、Todo、RSS、知识检索、联网查询、QQ 图片理解、引用上下文、Agent Loop、工具调用和主动推送装进同一个可维护的 Agent 底座里。

> Rust 单进程 · 多平台入口抽象 · Provider 无关 Agent Loop · 多模态输入 · 受控长期记忆 · 主动推送 · 模型自动降级

当前源码版本线：**v0.13.x：多入口、多模态与主动任务版本线**。这一版从“只会在 QQ 文本里聊天”的机器人，继续推进到能处理 QQ 图片和引用上下文、可选接入微信服务号、能主动投递 RSS / Todo 提醒，并补齐雷达查询、周期待办和多 Provider 路由的 Agent 助手。历史版本记录见 [CHANGELOG.md](./CHANGELOG.md)，实际发布包以 [Releases](https://github.com/kuliantnt/qq-maid-bot/releases) 页面为准。

## 当前版本亮点

| 重点 | 说明 |
| --- | --- |
| QQ 图片理解 | 私聊 / 群聊里的图片附件会按配置下载到本地媒体缓存，再作为多模态输入交给支持图片的模型；可以直接发截图、报错图、聊天记录图让机器人结合文字回答。 |
| 引用上下文追问 | 回复或引用上一条消息、图片、流式回复时，会把被引用内容整理进本轮上下文，适合继续追问“这张图什么意思”“刚才那条帮我改一下”。 |
| 微信服务号入口 | 可选启用微信服务号明文文本回调，支持同步文本回复和慢请求客服补发，适合把同一套 Agent 能力接到轻量微信入口。 |
| 主动提醒与订阅 | RSS 更新和 Todo 单次提醒写入 Notification Outbox 后由后台 Worker 投递；机器人不只是被动回答，也能在订阅更新、待办到期时主动找你。 |
| Todo 更像任务系统 | 支持今天 / 明天等日期查询、最近可见编号续操作、单次提醒和重复待办周期推进；“完成第一条”“刚才那条改到下周一”这类后续操作更稳定。 |
| AI 雷达查询 | 新增 `/rader`、`/radar`、`/雷达`，可查看 Codex Radar 和 Claude Code Radar 公开摘要，以及对应反馈入口。 |
| 模型路线自动降级 | `LLM_PROVIDER=auto` 配合候选链可按 `openai:`、`deepseek:`、`bigmodel:`、`mimo:` 等前缀路由；未配置 Key 或调用失败时按路线尝试后备模型。 |
| 多入口边界收敛 | QQ、微信和后续 OneBot 先进入统一入站模型，Core 只处理业务语义；平台 ID、引用、发送能力和真实投递目标留在 Gateway 边界。 |

## 赞助小店

项目接受小额赞助或相关服务合作，这里只保留少量克制展示位，具体介绍和链接后续补齐。

| 名称 | 内容 | 状态 |
| --- | --- | --- |
| <a href="https://codexauv.com/register?aff=UNKHTN42CDRT"><img src="docs/img/AUV_LOGO.png" alt="CodexAuv" width="200" /></a> | [CodexAuv](https://codexauv.com/register?aff=UNKHTN42CDRT) 是一家面向开发者和企业团队的 AI 模型 API 聚合平台，提供 Claude Code、Codex 等模型的统一中转服务 | 提供机器人托管及 AI 聚合服务 |

## 项目定位

项目当前主线是：**通用 Agent 底座 + 个人 / 办公助理能力**。当前可用入口以 QQ 官方机器人为主，微信服务号文本入口默认关闭且按需启用；OneBot 11 仍是后续规划。目标不是再造一个只能陪聊的机器人，而是让自然语言真正连接到可验证、可恢复、可持续运行的任务系统。

最开始只是想写一个 Todo。后来 Todo 长出了 LLM、RSS、SQLite、RAG、记忆系统、模型路由、Agent Loop 和一整套运维工具。事情逐渐失控，但服务器目前还算冷静。

## 项目亮点

### 🧰 不只是“模型说它做了”

私聊普通对话可以进入 Provider 无关的 Agent Loop。模型按需调用白名单工具，Core 根据真实工具结果生成可信回复，而不是仅凭模型文案判断操作是否成功。

当前天气和 Todo 已接入确定性展示；Todo 的新增、修改、完成、恢复、取消和删除都有明确执行结果。遇到目标不清楚时，会进入澄清或确认流程，后续消息可以在受限范围内恢复任务。

### 🖼️ 不只是看文字

QQ 入口可以按配置取回图片附件，把图片和用户文字一起交给支持多模态的模型。发截图、报错图、聊天记录图时，机器人不再只能看到“用户发了一张图片”。

引用上一条消息继续追问时，Gateway 会把被引用文本、图片和必要的回复索引整理进本轮请求；Core 继续按业务语义处理，不需要理解 QQ 平台字段。

### 🧠 不只是一次性聊天

会话可以新建、恢复、重命名和压缩，机器人能够持续维护上下文，而不是每条消息都从零开始。

长期记忆采用确认式流程：普通聊天不会偷偷写入记忆，只有用户明确提交并确认后才会保存。

### 📬 不只是等人发消息

内置 Todo、单次提醒、每日提醒、RSS / Atom 订阅和主动推送能力。

机器人既可以回答问题，也可以在待办提醒、每日摘要、订阅更新时主动发送消息。当前 Todo 单次提醒属于一阶段能力：支持明确时间的一次性个人提醒和失败重试，还不是完整通知平台。

### 🛡️ 不把稳定性押在一个模型上

独立的 Rust LLM 层支持 Provider 路由、模型候选链、错误分类、流式协议和自动降级。

当主模型或流式接口临时不可用时，可以根据配置尝试后备模型或兼容接口，而不是直接让整个机器人停止工作。

### 💬 更像正常聊天，而不是接口回包

私聊最终回复默认使用 QQ 原生文本流发送；Tool Loop 可发送一次受控可见进度提示，并支持独立的 QQ 原生 typing 状态。首帧不可用时会降级为普通完整回复，不会为了补发而重新运行 Agent Loop 或重复执行工具。

### 🦀 为长期在线运行而设计

运行时只需一个 `qq-maid-bot` 进程，主要业务状态统一保存在 SQLite。

项目提供部署脚本、服务控制、健康检查、链路诊断和结构化日志，适合部署在个人服务器上持续运行。

## 它能做什么

| 场景 | 当前能力 |
| --- | --- |
| 日常聊天 | 多轮会话、自动标题、上下文压缩、历史恢复、Markdown 回复、引用上下文追问 |
| 图片与多模态 | QQ 图片附件按配置取回并进入多模态模型链路；文件附件只保留元数据，不解析正文 |
| Agent 与工具 | Provider 无关 Agent Loop、同轮多工具串行执行、场景 profile、澄清 / 确认 / 受限恢复、可信结果编排 |
| 任务管理 | Todo 增删改查、自然语言时间解析、今天 / 明天等日期查询、重复待办周期推进、状态看板、列表折叠与完整展开、确定性写操作回执 |
| Todo 编号与提醒 | 最近可见编号快照、按“第一条 / 刚才那条”继续操作、明确时间的单次提醒一阶段、每日个人待办提醒 |
| 信息订阅 | RSS / Atom 轮询、去重、翻译和主动推送 |
| 长期记忆 | 生成记忆草稿，确认后保存，可查看和修改 |
| 私人知识库 | 自动索引本地 Markdown，并按需注入相关内容 |
| 联网查询 | Web Search、天气、列车时刻、翻译和 AI 雷达摘要 |
| 消息体验 | 私聊最终回复流、Tool Loop 可见进度提示、QQ 原生 typing、普通消息自动降级 |
| 模型基础设施 | Provider 路由、候选链、fallback、SSE、usage、健康观测和 Agent Loop 观测 |
| 多入口接入 | QQ 官方 Gateway；可选微信服务号明文 text-only 回调，支持同步快路径和慢请求客服文本补发，默认关闭；OneBot 11 仅为架构预留，尚未实现 |
| 运维诊断 | `/healthz`、`/ping`、部署脚本、服务控制和网络诊断；`/ping all` 会展示微信入口安全摘要 |

## 使用示例

```text
你：帮我新增待办：明天下午三点检查服务器日志
机器人：已新增待办：检查服务器日志
        时间：明天 15:00

你：明天上午九点半提醒我检查证书过期时间
机器人：已新增待办：检查证书过期时间
        提醒：明天 09:30

你：查看今天待办
机器人：📅 今天待办 · 共 2 项
        1. 检查服务器日志
        2. 更新周报

你：完成第一条
机器人：已完成待办：检查服务器日志

你：查看明天待办
机器人：📅 明天待办 · 共 1 项
        1. 续费域名

你：查看全部进行中待办
机器人：🚧 进行中 · 共 12 项
        1. ...
        还有 7 项待办，可说“查看完整结果”。

你：查看完整结果
机器人：🚧 进行中 · 共 12 项
        1. ...
        12. ...

你：查看已取消的待办
机器人：⛔ 已取消 · 共 3 项
        1. 和老公出门
        2. 和老公吃饭
        3. 买飞机票

你：把这些都删了
机器人：这些待办将被永久删除，确认继续吗？

你：杭州明天要带伞吗
机器人：小女仆正在查天气…
        明天有雨，建议带伞。

你：（发送一张服务器报错截图）这是什么问题
机器人：这张图里主要错误是 ...

你：（引用刚才那张截图）那应该先改哪里
机器人：建议先从 ...

你：/rss add https://example.com/feed.xml Rust News
机器人：已添加订阅：Rust News

你：/rader
机器人：🛰️ AI 雷达速览
        Codex Radar：...
        Claude Code Radar：...

你：/memory 我习惯使用 Asia/Shanghai 时区
机器人：已生成长期记忆草稿，请确认后保存。
```

<p align="center">
  <a href="docs/img/readme-chat-demo.png">
    <img src="docs/img/readme-chat-demo.png" alt="QQ 聊天效果" width="42%" />
  </a>
  <a href="docs/img/readme-health-demo.png">
    <img src="docs/img/readme-health-demo.png" alt="botctl 终端效果" width="42%" />
  </a>
</p>

## 配置理念

配置分成两层：`.env` 管部署和秘密，`agent.toml` 管 Agent 行为策略。

| 文件 | 负责内容 | 不负责内容 |
| --- | --- | --- |
| `runtime/config/.env` | QQ AppID / AppSecret、Provider API Key、旧兼容 Base URL、默认模型、场景模型兼容变量、数据库路径、日志和运行参数 | 私聊 / 群聊 profile、Tool Loop 轮数、工具白名单、输出预算等可公开策略 |
| `runtime/config/agent.toml` | 私聊 / 群聊策略、profile、Tool Loop 轮数、工具白名单、输出预算、reasoning effort、搜索路线名、OpenAI-compatible provider 元数据 | API Key、Access Token、私有 Base URL、真实 prompt、用户资料、聊天记录和数据库路径 |

默认 `runtime/config/agent.toml` 会随 runtime 和 Release 包分发，但它不绑定具体 Provider 或模型路线，也不要求必须使用 OpenAI。普通聊天路线默认继续继承 `.env` 中的 `PRIVATE_LLM_MODEL`、`GROUP_LLM_MODEL` 和 `LLM_MODEL`；只有你在 `agent.toml` 里显式新增同名 `model_routes` / `search_routes` 时，才会覆盖这些环境变量生成的内置路线。

这意味着：换 API Key、旧兼容内置 Provider Base URL、默认模型或部署路径，优先改 `.env`；声明 MiMo 等公开 OpenAI-compatible provider 元数据、调整私聊 / 群聊用哪个 profile、是否允许 Tool Calling、允许哪些工具、最多跑几轮工具、输出预算多少，优先改 `agent.toml`。实际密钥始终以进程环境变量优先，dotenv 文件只补充缺失项。

## 快速开始

### 前置条件

* Linux、macOS 或 Windows 主机，能够正常访问 QQ 开放平台和所配置的模型 API
* QQ 官方机器人 AppID 和 AppSecret（[QQ 开放平台](https://q.qq.com/) 申请）
* 一个受支持模型的 API Key（OpenAI 兼容接口、DeepSeek 或 BigModel）
* 基本命令行操作经验

> Release 会预构建 Linux x86_64 / ARM64、macOS Intel / Apple Silicon、Windows x86_64 包。一键部署和 `botctl.sh` 服务管理仍主要面向 Linux。

### 路径一：Linux Release 包（推荐，无需安装 Rust）

从 [Releases](https://github.com/kuliantnt/qq-maid-bot/releases) 下载与系统匹配的最新包，例如 `qq-maid-bot-vX.Y.Z-linux-x86_64.tar.gz`：

```bash
tar -xzf qq-maid-bot-vX.Y.Z-linux-x86_64.tar.gz
cd qq-maid-bot-vX.Y.Z-linux-x86_64

# 1. 配置环境变量
cp config/.env.example config/.env
vim config/.env

# 2. 启动
./botctl.sh start

# 3. 验证
./botctl.sh status
./botctl.sh health
```

最少需要填写：`QQ_BOT_APP_ID`、`QQ_BOT_APP_SECRET`，以及默认模型路线实际引用到的 Provider API Key。默认 `config/agent.toml` 声明私聊 / 群聊场景策略，但不绑定具体 Provider 或模型路线；普通聊天会继续继承 `.env` 里的 `LLM_MODEL`、`PRIVATE_LLM_MODEL`、`GROUP_LLM_MODEL`。使用第三方或自建兼容接口时，再在 `.env` 配置对应的 Base URL。需要处理 QQ 图片时，保持 `QQ_MAID_ENABLE_IMAGE=true`，并确认模型路线使用支持图片输入的 provider / model。

完整配置项说明见 [runtime/README.md](./runtime/README.md)。

### 路径二：源码构建（需要 Rust 工具链）

#### Linux

```bash
# 安装 Rust（如未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 克隆并构建
git clone https://github.com/kuliantnt/qq-maid-bot.git
cd qq-maid-bot

cp runtime/config/.env.example runtime/config/.env
vim runtime/config/.env

bash scripts/deploy-local.sh    # 构建 → 安装 → 启动

# 验证
runtime/botctl.sh status
runtime/botctl.sh health
```

#### Windows

```powershell
# 安装 Rust（如未安装）
# 从 https://rustup.rs 下载 rustup-init.exe 并运行

# 克隆并构建
git clone https://github.com/kuliantnt/qq-maid-bot.git
cd qq-maid-bot

# Windows MSVC 工具链可能需要 Visual Studio Build Tools
# 安装时勾选“使用 C++ 的桌面开发”
cargo build --release

# 准备配置
Copy-Item runtime\config\.env.example runtime\config\.env
# 编辑 runtime\config\.env

# 启动（程序从工作目录读取 config\.env）
cd runtime
..\target\release\qq-maid-bot.exe
```

Windows 下程序以前台方式运行，关闭终端即停止。如需长期后台运行，可使用 Windows 任务计划程序或第三方服务管理工具；项目目前暂未提供官方 Windows 服务脚本。

### 遇到问题？

| 问题 | 答案 |
| --- | --- |
| 启动后立即退出 | 查看日志最后几十行。通常是 `config/.env` 缺少必填项或 API Key 无效。 |
| QQ 收不到消息 | 确认 QQ 开放平台已启用机器人事件权限；查看 Gateway 是否成功鉴权并建立 WebSocket 连接。 |
| 模型调用报错 | 确认 `LLM_PROVIDER` 与 `LLM_MODEL` 前缀匹配，自定义前缀需先在 `agent.toml [providers.*]` 声明。用 GLM / Qwen / Ollama 等 OpenAI 兼容网关时，需要设 `OPENAI_API_MODE=chat_only`。 |
| 群聊不回复 | 默认 `mention` 模式只响应 @ 和回复机器人。主动响应可设 `QQ_MAID_GROUP_MESSAGE_MODE=active` 和 `QQ_MAID_GROUP_ACTIVE_KEYWORDS`；但 QQ 官方只能处理平台实际推送到 Gateway 的群消息，普通非 @ 消息平台不一定推送。 |
| 图片没有被理解 | 确认 `QQ_MAID_ENABLE_IMAGE=true`，媒体目录可写，图片大小没有超过 `QQ_MAID_MEDIA_MAX_BYTES`，并且当前模型支持图片输入。 |
| 怎么诊断 | `./botctl.sh health` 确认服务存活；`./diagnose-network.sh` 检查配置、网络和模型连通性。 |
| 升级后启动失败 | 对比新版 `config/.env.example` 是否新增必填项；检查 `PROMPT_DIR` 等路径是否仍然有效。 |
| 日志在哪 | Linux 可用 `./botctl.sh logs`，默认日志文件 `logs/qq-maid-bot.log`。Windows 前台运行时直接查看终端输出。临时排障可设 `RUST_LOG=debug`。 |

> 以上命令主要适用于 Linux Release 部署；Windows 源码运行时可直接查看终端日志。

更多帮助：

* 配置项详解：[runtime/README.md](./runtime/README.md)
* 配置模板：[runtime/config/.env.example](./runtime/config/.env.example)
* 开发调试：Linux 使用 `make run`（前台运行）；Windows 见 [docs/DEVELOPMENT.md](./docs/DEVELOPMENT.md)
* 开发维护：[docs/DEVELOPMENT.md](./docs/DEVELOPMENT.md)

## 运行表现

Gateway、Core 和 LLM 模块由同一个 `qq-maid-bot` 进程统一启动和管理。

一次约 80 分钟实际群聊连续使用后的运行快照：

| 指标 | 结果 |
| --- | ---: |
| 常驻内存 RSS | 约 24 MiB |
| 线程数 | 3 |
| 文件描述符 | 17 |
| Swap | 0 |

观察期间内存仅有小幅波动，线程数和文件描述符数量保持稳定，未发现明显的资源持续增长。

> 数据来自特定 Linux 环境下的实际运行快照，仅用于展示资源占用量级，不构成标准性能基准或长期稳定性保证。

写了很多 Rust，最终只是为了让服务器安静地睡觉。

## 架构概览

当前入口分为三条必须分清的链路：平台协议先归一化为入站模型；Core 只使用
`scope_key` / `owner_key` 做业务隔离；真实发送目标由回复和推送链路携带，不能从业务
key 反解析。

```mermaid
flowchart LR
    platform["QQ / 微信 / OneBot"] --> gateway["Gateway<br/>适配 / 过滤 / 去重 / 冷却 / 媒体取回"]
    gateway --> request["CoreRequest<br/>统一消息请求"]

    request --> agent["Agent Loop<br/>理解意图 / 调模型 / 调工具 / 汇总结果"]

    agent --> llm["LLM Router<br/>多模型 / 降级 / 多模态"]
    agent --> tools["Tool Registry<br/>待办 / RSS / 查询 / 记忆"]

    tools --> db[(SQLite)]
    agent --> db

    agent --> outbound["OutboundMessage"]
    outbound --> sender["Sender<br/>回复 / 主动推送"]
    sender --> platform
```

一次普通私聊 Agent 请求大致会经过：

```text
平台消息
  → 对应 adapter 解析平台协议字段
  → InboundMessage 归一化 Actor 与 Conversation
  → Gateway 先做 ignore / dedupe / cooldown / policy 判断
  → 如需处理图片，再按大小上限下载到本地媒体缓存并修正 MIME
  → CoreRequest 进入 Core
  → Core 按 scope_key / owner_key 装配会话、记忆与知识上下文
  → Agent Loop 请求模型
  → 模型按需调用白名单 Tool
  → Tool 返回结构化真实结果
  → Core 编排可信回复事件
  → OutboundMessage 交回 Gateway
  → Gateway 按 DeliveryTarget / ReplyCapability 选择平台 sender 投递
```

Gateway 与 Core 由同一进程装配，聊天、命令、`/ping check` 和通知投递都走进程内强类型接口；RSS / Todo 单次提醒先写入 Notification Outbox，再由后台 Worker 通过 Gateway 发送。外部 HTTP 默认仅保留 `GET /healthz`，以及运行和 Markdown 渲染所需的少量辅助接口。显式启用 `WECHAT_SERVICE_ENABLED=true` 时，Gateway 会额外启动微信服务号回调监听器，处理 GET URL 验证、POST 明文 `text` XML、同步文本 XML 快路径和慢请求客服文本补发，Markdown 会降级为 text。当前 QQ 官方图片链路只处理图片附件 URL，本地缓存前会经过处理门控与体积限制，随后通过 `MessageMedia.local_path` 交给 LLM provider 读取；文件附件仍只保留元数据，不做 OCR 或文件内容解析。当前不支持加密 XML、模板消息、图片语音视频、菜单事件、主动推送或流式输出，配置步骤见 [runtime/README.md#微信服务号文本回调配置](./runtime/README.md#微信服务号文本回调配置)。

项目内部通过根目录 Cargo Workspace 统一管理，保持明确的模块边界：

* `qq-maid-gateway-rs/` — QQ 事件接收、消息聚合、typing、流式与普通回复发送、`/ping` 诊断、图片 URL 按需下载与本地媒体缓存；可选微信服务号文本回调
* `qq-maid-core/` — CoreService、会话、记忆、知识库、Todo、RSS、业务 Tool、可信结果编排和命令
* `qq-maid-llm/` — 模型协议、Provider 路由、fallback、SSE、Agent Loop、Tool Loop 和健康观测
* `qq-maid-common/` — 时间、日期和时区等共享基础工具

同一个架构，换个说法：

```text
用户说话
  ↓
女仆长接单
  ↓
各部门互相甩锅
  ↓
工具拿真实结果说话
  ↓
SQLite 留档
  ↓
大模型继续背锅
```

### ID 分层

`scope_key` / `owner_key` 是 Session、Pending、Memory、Todo 等业务状态的隔离键；
`DeliveryTarget` 才是平台真实投递目标。RSS、Notification 和 Push 这种主动投递链路必须
保留平台原始发送目标，不能把 `target_id` 简化替换为 namespaced 业务 key。

```mermaid
flowchart TB
    raw_id["平台原始 ID / raw target<br/>QQ openid 或 group_openid<br/>OneBot user_id 或 group_id<br/>微信 FromUserName"]
    adapter["平台 adapter"]
    inbound_layer["InboundMessage<br/>Actor + Conversation"]
    business_key["业务隔离键<br/>scope_key / owner_key"]
    state["Session / Pending / Memory / Todo"]
    delivery_target["DeliveryTarget<br/>platform + raw_target_id + reply reference"]
    sender["平台 sender<br/>QQ / OneBot / 微信"]
    push["RSS / Notification / Push<br/>保存或携带 raw delivery target"]
    forbidden["禁止从 scope_key / owner_key<br/>反解析 raw_target_id"]

    raw_id --> adapter --> inbound_layer
    inbound_layer --> business_key --> state
    inbound_layer --> delivery_target
    push --> delivery_target --> sender
    state -. 不能用于投递 .-> forbidden
    forbidden -.-> delivery_target
```

维护规则：

* 平台原始 ID 只在 adapter、ReplyTarget、DeliveryTarget 和 sender 边界内解释。
* `scope_key` / `owner_key` 可包含平台命名空间，但它们只表示业务归属和隔离，不是发送地址。
* Core、LLM 和 Tool Loop 不接收 QQ `msg_seq`、stream id、微信 XML 字段、OneBot CQ 片段等协议字段。
* 新增 RSS、Notification、Todo 提醒或 Push 能力时，必须显式保存平台和 `raw_target_id`，不要从 `scope_key` 还原投递目标。

## 安全边界

Tool Calling 不等于把宿主机交给模型。

* 只有注册到 Tool Registry 的白名单工具可以被调用
* 工具拥有独立参数校验、权限和资源边界
* 高风险 Todo 操作需要确认
* 澄清恢复只允许在原任务的候选边界内继续
* 群聊默认不进入 Tool Loop；即使配置了群聊 profile，也必须显式允许群聊 Tool Calling，开启后仍按 `enabled_tools` 白名单暴露工具，默认不含 Todo
* slash 命令、文件处理和宿主机代码执行不会进入普通聊天 Tool Loop
* QQ 图片会按体积上限下载到本地媒体缓存后交给模型读取；文件附件不解析正文，日志不打印本地完整媒体路径或聊天正文
* 工具成功与否以真实执行结果为准，不以模型自述为准
* 微信服务号入口默认关闭；启用时支持明文 text-only 同步回复和慢请求客服文本补发，`access_token` 仅在客服补发时按需获取，诊断和日志不记录 Token、AppSecret、OpenID 或消息正文

## 开发调试

开发或排查问题时，可以在前台启动：

```bash
make run
```

`make run` 以前台方式启动 `qq-maid-bot`，方便直接观察输出。模块说明见 [qq-maid-core/README.md](./qq-maid-core/README.md)。

## 常用指令

完整命令列表和用法见 [开发文档](docs/DEVELOPMENT.md)。常用指令速查：

<details>
<summary>展开</summary>

```text
/new 新话题
/resume          /恢复
/state           /状态
/compact

/memory 内容     /记忆 内容
/memory show 1
/memory edit 1 新内容

帮我新增待办：明天下午检查日志
/todo             /待办
完成第一条待办

/rss add https://example.com/feed.xml 示例订阅
/rss              /订阅

/rader            /雷达
/rader codex

/查 今天的 Rust 新闻
/火车 G1
/天气杭州
/翻译日语 你好
```

</details>

## 项目状态与路线

项目仍在快速开发，主要面向个人部署和开发者使用。目前没有图形化管理后台，部署者需要具备基本的命令行、环境变量和 API 配置经验。

当前优先方向：

* 继续扩展统一 Agent Loop 和业务 Tool，而不是为每个自然语言表达堆独立分支
* 继续打磨 QQ 图片、多模态上下文和引用追问的真实聊天体验
* 将 Todo、RSS、雷达以及后续能力统一关联到主动推送与调度体系
* 完善办公 Agent 和个人助理场景
* 保留并继续打磨群聊能力，但不让娱乐功能反过来绑架底层架构
* 清理旧兼容链路、过期测试和历史包袱，为后续大版本瘦身

QQ 官方机器人功能仍受平台权限、审核和接口规则限制。Linux 的部署与服务管理支持更完整；Windows 当前主要通过源码构建并以前台方式运行。

## 参与开发

这个项目同时踩在通用 Agent、个人助理、办公自动化和群聊机器人几条线上，一个人确实容易写着写着就天亮了。

欢迎通过 Issue、PR、讨论和实际部署反馈参与。可以从文档、Provider 兼容、业务 Tool、QQ 平台适配、测试或运维脚本切入，不要求先看懂全部代码。

* 贡献指南：[CONTRIBUTING.md](./CONTRIBUTING.md)
* 鸣谢：[CONTRIBUTING.md#鸣谢](./CONTRIBUTING.md#鸣谢)
* Issues：[GitHub Issues](https://github.com/kuliantnt/qq-maid-bot/issues)

## 版本升级

当前源码版本线为 **v0.13.x：多入口、多模态与主动任务版本线**；已发布稳定包请以 [Releases](https://github.com/kuliantnt/qq-maid-bot/releases) 页面为准。版本升级前请先阅读 [CHANGELOG.md](./CHANGELOG.md)，并对比新版 `runtime/config/.env.example` 和 `runtime/config/agent.toml`。

从 v0.11.x 升级到当前版本线时，重点检查：默认 runtime 是否包含 `config/agent.toml`，旧 `.env` 中的 `LLM_MODEL` / `PRIVATE_LLM_MODEL` / `GROUP_LLM_MODEL` 是否仍符合预期，Todo 提醒是否只在明确需要时开启。需要处理 QQ 图片时，再检查 `QQ_MAID_MEDIA_DIR`、`QQ_MAID_MEDIA_MAX_BYTES` 和所选模型是否支持图片输入。较早版本从 v0.3.x 升级到 v0.4.0 涉及单进程架构迁移，仍需参考 [v0.4.0 迁移说明](./CHANGELOG.md#v040)。

## 配置和隐私提醒

* 不要提交 API Key、QQ AppSecret、Token、OpenID、群 ID、聊天记录或真实用户数据。
* 不要将真实 Prompt、Markdown 知识资料、SQLite 数据库和日志提交到公开仓库。
* 公开仓库只提供 `.example` 模板，例如 [runtime/config/.env.example](./runtime/config/.env.example)。
* 私有配置和运行数据应放在仓库外，或放在被 `.gitignore` 忽略的目录中。
* 诊断和日志默认保持脱敏；临时开启 verbose 日志后，排障结束应关闭。

## 今天女仆会不会罢工

- [x] 能聊天
- [x] 能记 Todo
- [x] 能看天气
- [x] 能读 RSS
- [x] 能自动切换模型
- [x] 能查知识库
- [x] 有 Provider 无关的统一 Agent Loop
- [x] 有 Agent 场景策略配置和模型路线 fallback
- [x] 能处理 QQ 图片并进入多模态模型链路
- [x] 能在引用上一条消息或图片时保留上下文
- [x] 能在私聊中自主调用天气、火车、RSS 和 Todo Tool
- [x] 能对 Todo 写操作生成可验证的确定性回执
- [x] 能按今天 / 明天 / 指定日期查询 Todo
- [x] 能推进重复待办的下一次周期
- [x] 能用最近可见列表快照处理“第一条”
- [x] 有 `/rader` / `/radar` / `/雷达` 公开雷达摘要命令
- [x] 有 RSS 更新、Todo 单次提醒和 Notification Outbox Worker
- [x] 能处理 Todo 澄清、确认和受限恢复
- [x] 支持 QQ 原生 typing 和私聊最终回复流
- [ ] 接入更多可验证的业务 Tool
- [ ] 把 Todo、RSS 与后续能力打磨成完整通知平台
- [ ] 真正理解人类
- [ ] 阻止作者继续重构

## ⭐ Star History

如果喜欢这个项目，请给个 Star ⭐

[![Star History Chart](https://api.star-history.com/svg?repos=kuliantnt/qq-maid-bot&type=Date)](https://star-history.com/#kuliantnt/qq-maid-bot&Date)

## 文档导航

* 版本记录：[CHANGELOG.md](./CHANGELOG.md)
* 部署运行说明：[runtime/README.md](./runtime/README.md)
* 配置模板：[runtime/config/.env.example](./runtime/config/.env.example)
* Core 模块文档：[qq-maid-core/README.md](./qq-maid-core/README.md)
* LLM 基础设施文档：[qq-maid-llm/README.md](./qq-maid-llm/README.md)
* Gateway 文档：[qq-maid-gateway-rs/README.md](./qq-maid-gateway-rs/README.md)
* 开发维护文档：[docs/DEVELOPMENT.md](./docs/DEVELOPMENT.md)
* 贡献指南：[CONTRIBUTING.md](./CONTRIBUTING.md)
* 鸣谢：[CONTRIBUTING.md#鸣谢](./CONTRIBUTING.md#鸣谢)
* Makefile：[Makefile](./Makefile)

## 你可能不需要它，如果：

- 你只想要一个十行 Python 自动回复脚本
- 你不想维护数据库
- 你认为几万行 Rust 不算轻量
- 你希望模型可以不经确认直接操作宿主机
- 你不会在凌晨三点突然重构整个 LLM 层

## License

本项目基于 [MIT License](./LICENSE) 开源。

<!--
你居然看到了这里。

运行：
qq-maid-bot --summon-maid
-->
