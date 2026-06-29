<div align="center">
  <img src="docs/img/logo.png" alt="QQ Maid Bot" width="180" />
  <h1>QQ Maid Bot</h1>
  <p><strong>一个会聊天、会记事，也会主动办事的自托管 QQ AI 助手。</strong></p>
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

QQ Maid Bot 使用 Rust 构建，通过 QQ 官方机器人接口运行。它不只是把消息转发给大模型，而是将长期会话、受控记忆、Todo、RSS、知识检索、联网查询、受控工具调用和主动推送整合进同一个长期在线的机器人中。

> Rust 单进程 · QQ 官方接口 · 受控长期记忆 · 主动推送 · 模型自动降级 · 模型原生 Tool Calling

## 项目亮点

### 🧠 不只是一次性聊天

会话可以新建、恢复、重命名和压缩，机器人能够持续维护上下文，而不是每条消息都从零开始。

长期记忆采用确认式流程：普通聊天不会偷偷写入记忆，只有用户明确提交并确认后才会保存。

### 📬 不只是等人发消息

内置 Todo、每日提醒、RSS / Atom 订阅和主动推送能力。

机器人既可以回答问题，也可以在任务到期、订阅更新时主动发送消息。

### 🛡️ 不把稳定性押在一个模型上

独立的 Rust LLM 层支持模型候选链、错误分类和自动降级。

当主模型或流式接口临时不可用时，可以根据配置尝试后备模型或兼容接口，而不是直接让整个机器人停止工作。

### 🧰 正在接入模型原生工具调用

最终目标是参考 Codex 的受控工具调用体验：用户在私聊中用自然语言提出任务，模型按需调用经过白名单、权限和资源限制保护的工具，再把结构化结果整理成回复。

当前首期只开放最小闭环：私聊普通聊天可通过 OpenAI Responses 原生 `function_call` 调用天气 Tool；群聊、slash 命令、pending 确认、文件处理和宿主机代码执行都不会进入 Tool Loop。

### 🦀 为长期在线运行而设计

运行时只需一个 `qq-maid-bot` 进程，主要业务状态统一保存在 SQLite。

项目提供部署脚本、服务控制、健康检查、链路诊断和运行日志，适合部署在个人服务器上持续运行。

## 为什么又造了一个 QQ 机器人？

最开始只是想写一个 Todo。

后来有了：

- RSS
- 天气
- LLM
- 多模型降级
- SQLite
- RAG
- 记忆系统
- Markdown 渲染
- 独立 LLM 抽象层

现在它有五万多行 Rust。

## 它能做什么

| 场景    | 能力                                      |
| ----- | --------------------------------------- |
| 日常聊天  | 多轮会话、自动标题、上下文压缩、历史恢复和私聊天气 Tool Calling |
| 长期记忆  | 生成记忆草稿，确认后保存，可查看和修改                     |
| 任务管理  | Todo 增删改查、自然语言时间解析、定时提醒                   |
| 信息订阅  | RSS / Atom 轮询、去重、翻译和主动推送                |
| 私人知识库 | 自动索引本地 Markdown，并按需注入相关内容               |
| 联网工具  | Web Search、显式天气命令、列车时刻和翻译                |
| 模型调用  | Provider 路由、候选链、fallback、SSE、usage 和 Tool Loop 观测 |
| 运维诊断  | `/healthz`、`/ping`、部署脚本和服务控制脚本             |

## 使用示例

```text
你：/todo add 明天下午三点检查服务器日志
机器人：已添加待办：检查服务器日志
        时间：明天 15:00

你：/rss add https://example.com/feed.xml Rust News
机器人：已添加订阅：Rust News

你：/memory 我习惯使用 Asia/Shanghai 时区
机器人：已生成长期记忆草稿，请确认后保存。

你：/查 今天 Rust 生态有什么值得关注的新闻
机器人：正在联网查询……

你：杭州明天要带伞吗
机器人：我查了杭州的天气预报……建议带伞。
```

<p align="center">
  <a href="docs/img/readme-chat-demo.png">
    <img src="docs/img/readme-chat-demo.png" alt="QQ 聊天效果" width="42%" />
  </a>
  <a href="docs/img/readme-health-demo.png">
    <img src="docs/img/readme-health-demo.png" alt="botctl 终端效果" width="42%" />
  </a>
</p>

## 快速开始

### 前置条件

* Linux 或 Windows 主机，能够正常访问 QQ 开放平台和所配置的模型 API
* QQ 官方机器人 AppID 和 AppSecret（[QQ 开放平台](https://q.qq.com/) 申请）
* 一个受支持模型的 API Key（OpenAI 兼容接口或 DeepSeek）
* 基本命令行操作经验

> 当前预构建 Release 包和一键部署脚本主要面向 Linux x86_64。Windows 可以通过 Rust 工具链从源码构建并运行，但暂未提供与 `botctl.sh` 等价的完整服务管理脚本。

### 路径一：Linux Release 包（推荐，无需安装 Rust）

从 [Releases](https://github.com/kuliantnt/qq-maid-bot/releases) 下载最新 `qq-maid-bot-vX.Y.Z-linux-x86_64.tar.gz`：

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

最少需要填写：`QQ_BOT_APP_ID`、`QQ_BOT_APP_SECRET`，以及所选模型 Provider 的 API Key。使用第三方或自建兼容接口时，再配置对应的 Base URL。
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
# 安装时勾选"使用 C++ 的桌面开发"
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
|------|------|
| 启动后立即退出 | 查看日志最后几十行。通常是 `config/.env` 缺少必填项或 API Key 无效。 |
| QQ 收不到消息 | 确认 QQ 开放平台已启用机器人事件权限；查看 Gateway 是否成功鉴权并建立 WebSocket 连接。 |
| 模型调用报错 | 确认 `LLM_PROVIDER` 与 `LLM_MODEL` 前缀匹配。用 GLM / Qwen / Ollama 等 OpenAI 兼容网关时，需要设 `OPENAI_API_MODE=chat_only`。 |
| 群聊不回复 | 默认 `mention` 模式只响应 @ 和回复机器人。主动响应需设 `QQ_MAID_GROUP_MESSAGE_MODE=active` 和 `QQ_MAID_GROUP_ACTIVE_KEYWORDS`。 |
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

| 指标       |       结果 |
| -------- | -------: |
| 常驻内存 RSS | 约 24 MiB |
| 线程数      |        3 |
| 文件描述符    |       17 |
| Swap     |        0 |

观察期间内存仅有小幅波动，线程数和文件描述符数量保持稳定，未发现明显的资源持续增长。

> 数据来自特定 Linux 环境下的实际运行快照，仅用于展示资源占用量级，不构成标准性能基准或长期稳定性保证。

写了五万多行 Rust，最终只是为了让服务器安静地睡觉。

## 架构概览

```mermaid
graph TD
    A[QQ 官方机器人平台] --> B[qq-maid-bot]
    B --> C[Gateway 模块]
    C -->|进程内 CoreService::respond| D[Core 模块]
    D --> E[qq-maid-llm 模型 Provider]
    D --> F[(SQLite APP_DB_FILE)]
    D --> G[查询与天气服务]
    D --> I[Tool Registry / WeatherTool]
    I --> G
    D --> H[RSS / Todo / Memory / Session]
    H -->|进程内 PushSink| C
    C --> A
```

Gateway 与 Core 由同一进程装配，聊天、命令、`/ping check`、RSS 和 Todo 主动推送都走进程内强类型接口；外部 HTTP 仅保留 `GET /healthz`，以及运行和 Markdown 渲染所需的少量辅助接口。

项目内部通过根目录 Cargo Workspace 统一管理，保持明确的模块边界：

* `qq-maid-gateway-rs/` — QQ 事件接收、消息转换、回复发送、`/ping` 诊断
* `qq-maid-core/` — CoreService、业务 prompt、会话、记忆、Todo、RSS、业务 Tool 和命令
* `qq-maid-llm/` — 模型协议、Provider 路由、fallback、SSE、Tool Loop 和健康观测
* `qq-maid-common/` — 时间、日期和时区等共享基础工具

同一个架构，换个说法：

```text
用户说话
  ↓
女仆长接单
  ↓
各部门互相甩锅
  ↓
SQLite 留档
  ↓
大模型背锅
```

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

/todo add 明天下午检查日志
/todo             /待办
/todo done 1

/rss add https://example.com/feed.xml 示例订阅
/rss              /订阅

/查 今天的 Rust 新闻
/火车 G1
/天气杭州
/翻译日语 你好
```

</details>

## 项目状态

项目仍在持续开发，主要面向个人部署和开发者使用。

目前没有图形化管理后台，部署者需要具备基本的命令行、环境变量和 API 配置经验。Linux 的部署与服务管理支持更完整；Windows 当前主要通过源码构建并以前台方式运行。QQ 官方机器人功能仍受平台权限、审核和接口规则限制。

## 版本升级

从 v0.3.x 升级到 v0.4.0 涉及单进程架构迁移，请先阅读 [CHANGELOG.md](./CHANGELOG.md#v040)。

## 配置和隐私提醒

* 不要提交 API Key、QQ AppSecret、Token、OpenID、群 ID、聊天记录或真实用户数据。
* 不要将真实 Prompt、Markdown 知识资料、成员映射、SQLite 数据库和日志提交到公开仓库。
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
- [x] 能在私聊里让模型自主调用天气 Tool
- [ ] 像 Codex 一样安全调用更多工具
- [ ] 真正理解人类
- [ ] 阻止作者继续重构

## ⭐ Star History

如果喜欢这个项目，请给个 Star ⭐

[![Star History Chart](https://api.star-history.com/svg?repos=kuliantnt/qq-maid-bot&type=Date)](https://star-history.com/#kuliantnt/qq-maid-bot&Date)

## 文档导航

* 部署运行说明：[runtime/README.md](./runtime/README.md)
* 配置模板：[runtime/config/.env.example](./runtime/config/.env.example)
* Core 模块文档：[qq-maid-core/README.md](./qq-maid-core/README.md)
* LLM 基础设施文档：[qq-maid-llm/README.md](./qq-maid-llm/README.md)
* Gateway 文档：[qq-maid-gateway-rs/README.md](./qq-maid-gateway-rs/README.md)
* 开发维护文档：[docs/DEVELOPMENT.md](./docs/DEVELOPMENT.md)
* Makefile：[Makefile](./Makefile)
* Issues：https://github.com/kuliantnt/qq-maid-bot/issues

## 你可能不需要它，如果：

- 你只想要一个十行 Python 自动回复脚本
- 你不想维护数据库
- 你认为五万行 Rust 不算轻量
- 你不会在凌晨三点突然重构整个 LLM 层

## License

本项目基于 [MIT License](./LICENSE) 开源。

<!--
你居然看到了这里。

运行：
qq-maid-bot --summon-maid
-->
