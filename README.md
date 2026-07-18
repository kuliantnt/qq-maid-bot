<div align="center">
  <img src="docs/img/logo.png" alt="小女仆机器人" width="180" />
  <h1>小女仆机器人</h1>
  <p><strong>一个会聊天、会记事、会调用工具，也会主动推送的轻量、自托管、多入口 AI Agent 机器人。</strong></p>
  <p>
    <a href="https://github.com/kuliantnt/qq-maid-bot/actions/workflows/ci.yml"><img src="https://github.com/kuliantnt/qq-maid-bot/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
    <a href="https://github.com/kuliantnt/qq-maid-bot/releases"><img src="https://img.shields.io/github/v/release/kuliantnt/qq-maid-bot" alt="Release" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/github/license/kuliantnt/qq-maid-bot" alt="License" /></a>
    <a href="https://deps.rs/repo/github/kuliantnt/qq-maid-bot"><img src="https://deps.rs/repo/github/kuliantnt/qq-maid-bot/status.svg" alt="Dependencies" /></a>
    <img src="https://img.shields.io/badge/maid-online-ff69b4" alt="Maid Status" />
    <img src="https://img.shields.io/badge/女仆浓度-100%25-ff69b4" alt="Maid Purity" />
  </p>
  <p><sub>Rust 单进程 · 约 25 MiB 常驻内存 · 默认空闲时 3 个线程 · Provider 无关 Agent Loop · 多模态输入 · 主动推送 · 模型自动降级</sub></p>
</div>

项目使用 Rust 构建，当前以 QQ 官方机器人为主要入口，同时支持 OneBot 11 和可选的微信服务号文本入口。它在同一个进程中提供多轮会话、受控长期记忆、Todo 与提醒、RSS 推送、本地知识检索、联网查询、多模态理解和 Tool Calling。

> 💡 仓库早期以 QQ 机器人为主，因此仍保留 `qq-maid-bot` 名称。当前项目正在从 QQ 官方机器人演进为多入口平台型小女仆机器人。

稳定版本与升级说明见 [Releases](https://github.com/kuliantnt/qq-maid-bot/releases) 和 [CHANGELOG.md](./CHANGELOG.md)。

使用、安装和配置优先看 [项目 Wiki](https://github.com/kuliantnt/qq-maid-bot/wiki)：从第一次对话、一键安装，到 NapCat、`/ops` 运维和 Codex 长任务，都按场景拆开了。仓库内 `docs/` 与各 crate README 更偏开发边界和实现细节。

## 近期更新（v0.19.0）
- **聊过的事可选自动记住**：开启 Session Dream 后，机器人会在后台从私聊 / 群聊里提取稳定长期事实；只写你的个人记忆或当前群画像，不会覆盖你明确要求「记住」的内容，默认关闭。
- **记忆找得更准、重复更少**：问到相关事时会优先拿出有用记忆；可选的确定性整理会在后台归档完全重复的条目，不会改你已确认的内容。
- **对比几个东西会分开查**：“A 和 B 哪个更适合”这类问题会独立调查各个对象再汇总；单个超时也不会让整轮白忙。
- **待办能按时间和关键词筛**：可以直接查今天、明天、本周、逾期、没截止时间，也可以加关键词组合筛选；默认一次展示更多条目。
- **管理员可配白名单运维命令**：`/ops` 默认关闭；配好管理员、允许群和固定程序后，可以在聊天里触发受控脚本，结果会异步推回来。
- **搜索更耐慢链路，超时会明说**：联网查询对国内慢链路更宽容；QQ 流式超时不会再卡在“正在想一下”，而是给出明确失败提示。
- **RSS 地址更好点了**：QQ Markdown 里不再把长 URL 挤成一堆换行空格，改为可点的短链接；纯文本通道仍保留完整地址。

更早版本的完整变更与升级说明见 [CHANGELOG.md](./CHANGELOG.md)。

## 能做什么

- **聊天与上下文**：管理多轮会话，理解图片，并结合引用消息继续追问；共享群聊历史会区分发言成员，降低昵称、偏好和身份信息串线风险。
- **Todo 与提醒**：新增、修改、完成、恢复和删除待办，支持单次提醒、重复提醒和每日摘要；列表可按今天 / 明天 / 本周 / 逾期 / 关键词组合筛选。
- **查询与订阅**：查询天气、火车时刻和网页信息，支持多对象对比式联网搜索；订阅 RSS/Atom 并主动推送更新。
- **记忆与知识库**：个人记忆、群内个人画像和群公共记忆分域管理，并按场景与可见性召回。用户明确要求“记住”时可直接保存；可选的确定性整理（`MEMORY_CONSOLIDATION_ENABLED`）与 Session Dream（`MEMORY_DREAM_ENABLED`）分开开关。Dream 只从会话消息提取安全长期事实，写入个人记忆或当前成员群画像，不覆盖已确认记忆；本地 Markdown 可自动索引并按需检索。
- **受控工具与运维命令**：模型只能调用服务端注册并按场景放行的工具；管理员可通过默认关闭的 `/ops` 白名单命令触发固定程序，结果以真实执行或持久化结果为准。
- **多模型路由**：支持 OpenAI、Gemini、MiMo、DeepSeek 和 OpenAI-compatible Provider，并可按候选链自动降级。

## 平台支持

| 平台 | 状态 | 当前能力 |
| --- | --- | --- |
| QQ 官方机器人 | 主要入口 | C2C、群聊、图片理解、引用上下文、流式回复和主动推送 |
| OneBot 11 | 可选 | 单账号反向 WebSocket，支持私聊、群聊、图片理解、文件摘要和纯文本主动推送 |
| 微信服务号 | 可选 | 明文/AES 文本回调、同步回复和慢请求客服补发 |

OneBot 11 当前主要面向 NapCat，详细限制与接入步骤见 Wiki [用 NapCat 接入小女仆](https://github.com/kuliantnt/qq-maid-bot/wiki/Napcat接入)（仓库技术版：[OneBot 11 接入文档](./docs/development/onebot11-napcat.md)）。微信服务号默认关闭，配置方式见 [runtime 运行文档](./runtime/README.md#微信服务号文本回调配置)。

## 快速开始

运行机器人至少需要启用一个入口，并配置一个可用的模型 Provider。使用 QQ 官方入口时，还需要 QQ 开放平台提供的 AppID 和 AppSecret。

### Linux 一键安装

安装脚本会根据 CPU 架构下载最新 Release，无需安装 Rust：

```bash
curl -fsSL https://github.com/kuliantnt/qq-maid-bot/raw/refs/heads/master/scripts/qbot.sh -o /tmp/qbot.sh
bash /tmp/qbot.sh deploy

qbot install
qbot config bot
qbot config ai
qbot start
qbot status
```

常用运维命令：

```bash
qbot log       # 跟随日志
qbot health    # 健康检查
qbot restart   # 重启服务
qbot update    # 更新版本
```

### Windows 一键安装

在 PowerShell 中下载安装器：

```powershell
$p="$env:TEMP\qbot.ps1"; Invoke-WebRequest https://github.com/kuliantnt/qq-maid-bot/raw/refs/heads/master/scripts/qbot.ps1 -OutFile $p -UseBasicParsing; powershell.exe -NoProfile -ExecutionPolicy Bypass -File $p install
```

然后编辑配置并启动：

```powershell
& "$HOME\qq-maid-bot\qbot.cmd" config path
notepad "$HOME\qq-maid-bot\config\.env"
& "$HOME\qq-maid-bot\qbot.cmd" start
& "$HOME\qq-maid-bot\qbot.cmd" status
```

当前 Windows Release 仅提供 x86_64 版本。更完整的安装与排障说明见 Wiki [安装手册](https://github.com/kuliantnt/qq-maid-bot/wiki/安装手册)；手动下载 Release、开机启动和更新细节也可对照 [runtime 运行文档](./runtime/README.md#release-包)。

### 从源码运行

需要已安装 Rust 工具链：

```bash
git clone https://github.com/kuliantnt/qq-maid-bot.git
cd qq-maid-bot
cp runtime/config/.env.example runtime/config/.env
vim runtime/config/.env
make local
runtime/botctl.sh status
```

开发调试、Windows 源码构建和测试命令见 Wiki [开发维护文档](https://github.com/kuliantnt/qq-maid-bot/wiki/开发维护文档) 或仓库 [docs/DEVELOPMENT.md](./docs/DEVELOPMENT.md)。

## 配置方式

配置分为两层：

| 文件 | 用途 |
| --- | --- |
| `runtime/config/.env` | 入口凭证、Provider API Key、私有 Base URL、数据库和日志路径等部署配置 |
| `runtime/config/runtime.toml` | WebUI 与人工编辑共享的程序受管普通运行配置；不存在时首次保存创建 |
| `runtime/config/agent.toml` | 场景、模型候选链、profile、Tool Loop 预算和工具白名单等 Agent 策略 |
| `runtime/config/ops.toml` | 可选的 `/ops` 管理员、允许群、固定程序及独立 Codex 长任务策略；默认不存在且全部关闭 |
| `runtime/config/secrets/master.key` | SQLite 敏感密文的独立主密钥；必须持久化、严格限权并单独备份 |

完整环境变量以 [`.env.example`](./runtime/config/.env.example) 为准，配置中心优先级与安全边界见[配置中心清单](./docs/development/config-center.md)。默认模型路线以 [`agent.toml`](./runtime/config/agent.toml) 为准；`/ops` 配置从 [`ops.example.toml`](./runtime/config/ops.example.toml) 复制为未跟踪的 `ops.toml` 后填写，具体步骤见 Wiki [用 `/ops` 在 QQ 里做运维](https://github.com/kuliantnt/qq-maid-bot/wiki/ops运维命令) 与 [用 `/ops codex` 跑长任务](https://github.com/kuliantnt/qq-maid-bot/wiki/ops-codex)。调整模型、工具、场景策略或白名单运维命令时，不需要修改业务代码。

配置文件、SQLite、日志、私有 Prompt 和知识资料都可能包含敏感信息，不要提交到公开仓库。

## 使用示例

```text
你：明天下午三点提醒我检查服务器日志
机器人：已新增待办：检查服务器日志
        提醒：明天 15:00

你：查看今天待办
机器人：📅 今天待办 · 共 2 项
        1. 检查服务器日志
        2. 更新周报

你：完成第一条
机器人：已完成待办：检查服务器日志

你：（发送一张报错截图）这是什么问题
机器人：这张图里的主要错误是……

你：/rss add https://example.com/feed.xml Rust News
机器人：已添加订阅：Rust News

管理员：/ops status
机器人：运维任务 status 已受理，完成后会通知你。

管理员：/ops codex 检查当前项目为什么构建失败，并修复相关问题
机器人：Codex 任务已受理
        任务 ID：ops-a82f31
        取消：/ops cancel ops-a82f31

你：/memory 我习惯使用 Asia/Shanghai 时区
机器人：🧠 已记住
        范围：个人记忆
        内容：你习惯使用 Asia/Shanghai 时区
```

<p align="center">
  <a href="docs/img/readme-chat-demo.png">
    <img src="docs/img/readme-chat-demo.png" alt="QQ 聊天效果" width="42%" />
  </a>
  <a href="docs/img/readme-health-demo.png">
    <img src="docs/img/readme-health-demo.png" alt="botctl 终端效果" width="42%" />
  </a>
</p>

## 架构概览

```mermaid
flowchart LR
    platform["QQ / OneBot / 微信"] --> gateway["Gateway<br/>接入与收发"]
    gateway --> core["Core<br/>会话与业务编排"]
    core --> llm["LLM<br/>模型路由与 Tool Loop"]
    core --> tools["Tools<br/>Todo / RSS / 查询 / 记忆"]
    core --> db[(SQLite)]
    tools --> db
    core --> gateway
```

根目录 Cargo Workspace 统一管理四个 crate：

| 目录 | 职责 |
| --- | --- |
| `qq-maid-gateway-rs/` | 平台接入、事件转换、过滤去重和消息发送 |
| `qq-maid-core/` | `CoreService`、会话、记忆、Todo、RSS、知识库和业务 Tool |
| `qq-maid-llm/` | 模型协议、Provider 路由、fallback、SSE 和 Tool Loop |
| `qq-maid-common/` | 身份、消息结构、时间、Markdown 和脱敏等共享基础能力 |

依赖方向保持为 `gateway -> core -> llm -> common`。更详细的模块边界、项目结构和开发约定见 [docs/DEVELOPMENT.md](./docs/DEVELOPMENT.md)。

同一个架构，换个说法：

```text
用户说话 → 女仆长接单 → 各部门互相甩锅 → 工具拿真实结果说话 → SQLite 留档 → 大模型继续背锅
```

## 安全边界

- 只有注册到 Tool Registry 并被当前场景放行的工具可以调用；群聊默认不进入 Tool Loop。
- Todo 高风险操作和记忆清空、群画像停用等破坏性操作需要二次确认；明确的新增记忆请求校验通过后直接写入。
- 个人记忆、群内个人画像和群公共记忆分别校验 actor、scope、可见性与管理员权限；共享群聊历史使用脱敏成员引用区分发言人。
- 工具执行、Todo 写入和记忆保存都以真实结果为准，模型文案不能代替执行结果。
- `/ops` 默认关闭，只执行配置中的固定程序与参数规则，不走 Shell，不让模型随意拼命令；私聊需管理员白名单，群聊还需允许群且角色为群主 / 管理员。
- 日志与诊断默认脱敏，不应输出凭证、完整平台 ID 或聊天正文；私聊 `/ping` 只在当前用户自己查看时展示稳定 `user_id`，便于填写 `/ops` 白名单。
- 本地管理面板默认关闭，仅适合本机或受控内网，不应直接暴露到公网。

## 常见问题

| 现象 | 优先检查 |
| --- | --- |
| 启动后立即退出 | 查看日志，确认入口配置完整且 Provider API Key 有效 |
| QQ 收不到消息 | 确认 QQ 开放平台事件权限和 Gateway WebSocket 连接状态 |
| 群聊不回复 | 默认 `mention` 模式只响应 @ 或对机器人消息的回复 |
| 模型调用失败 | 检查 API Key、`OPENAI_BASE_URLS` 和模型前缀；兼容网关可能需要 `OPENAI_API_MODE=chat_only` |
| `/ops` 不生效 | 确认存在 `config/ops.toml`、`enabled = true`，以及管理员 / 允许群 / 命令白名单；见 Wiki [用 `/ops` 在 QQ 里做运维](https://github.com/kuliantnt/qq-maid-bot/wiki/ops运维命令) |
| 升级后无法启动 | 对比新版 `config/.env.example` 是否新增或调整配置项；若仍使用旧 `OPENAI_BASE_URL`，请迁移到 `OPENAI_BASE_URLS` |

使用 `qbot health` 检查服务状态。网络和上游问题可运行发布包中的 `diagnose-network.sh`；完整排障方式见 [runtime 运行文档](./runtime/README.md#控制脚本和诊断)。

## 文档导航

使用、安装和配置优先看 [项目 Wiki](https://github.com/kuliantnt/qq-maid-bot/wiki)。仓库文档保留实现边界与可 review 的技术细节。

| 文档 | 适合什么时候看 |
| --- | --- |
| [项目 Wiki](https://github.com/kuliantnt/qq-maid-bot/wiki) | 使用说明、安装手册、NapCat、天气、`/ops`、Codex 等场景化教程 |
| Wiki [使用说明](https://github.com/kuliantnt/qq-maid-bot/wiki/使用说明) | 第一次和机器人说话、能力边界和新用户自测 |
| Wiki [安装手册](https://github.com/kuliantnt/qq-maid-bot/wiki/安装手册) | Linux / Windows 一键安装、配置、升级和排障 |
| Wiki [用 NapCat 接入小女仆](https://github.com/kuliantnt/qq-maid-bot/wiki/Napcat接入) | 用 OneBot 11 / NapCat 接 QQ |
| Wiki [用 `/ops` 在 QQ 里做运维](https://github.com/kuliantnt/qq-maid-bot/wiki/ops运维命令) | 配置管理员、固定程序、botmon 和异步回执 |
| Wiki [用 `/ops codex` 跑长任务](https://github.com/kuliantnt/qq-maid-bot/wiki/ops-codex) | 配置 Codex、NVM 环境和专项排障 |
| Wiki [插件开发](https://github.com/kuliantnt/qq-maid-bot/wiki/插件开发) | 自己写一个 Tool / 插件 |
| [runtime/README.md](./runtime/README.md) | 运行目录、环境变量、控制脚本和诊断细节 |
| [docs/DEVELOPMENT.md](./docs/DEVELOPMENT.md) | 开发环境、架构边界、常用命令和检查要求 |
| [自定义 Tool 指南](./docs/development/custom-tools.md) | 新增或接入业务工具的技术版 |
| [OneBot 11 接入文档](./docs/development/onebot11-napcat.md) | NapCat / OneBot 11 技术版 |
| [`/ops` 白名单运维命令](./docs/development/ops-command.md) | `/ops` 完整安全边界与配置字段 |
| [`/ops codex` 使用指南](./docs/development/ops-codex.md) | Codex 长任务技术版 |
| [Gateway README](./qq-maid-gateway-rs/README.md) | 平台事件和消息发送实现 |
| [Core README](./qq-maid-core/README.md) | 会话、命令和业务编排实现 |
| [LLM README](./qq-maid-llm/README.md) | Provider、路由、SSE 和 Tool Loop 实现 |
| [Web Console README](./web-console/README.md) | 构建只读管理面板 |

## 参与项目

项目主要面向个人部署和开发者使用，仍在持续演进。提交 Issue 或 PR 前请阅读 [CONTRIBUTING.md](./CONTRIBUTING.md)，并避免附带 API Key、平台凭证、真实用户数据、聊天记录或私有知识资料。

- [GitHub Issues](https://github.com/kuliantnt/qq-maid-bot/issues)
- [版本变更](./CHANGELOG.md)
- [贡献者与鸣谢](./CONTRIBUTING.md#鸣谢)

## 今天女仆会不会罢工

- [x] 能聊天、能看图片
- [x] 能记 Todo、能设提醒
- [x] 能看天气、查火车
- [x] 能读 RSS、主动推送
- [x] 能查知识库、能联网搜索
- [x] 能自动切换模型、自动降级
- [x] 有 Provider 无关的统一 Agent Loop
- [x] 有受控长期记忆、明确新增直写和破坏性操作确认
- [x] 可选 Session Dream 与确定性记忆整理
- [x] 能在引用上一条消息或图片时保留上下文
- [x] 有管理员白名单 `/ops` 运维入口
- [ ] 接入更多可验证的业务 Tool
- [ ] 把 Todo、RSS 与后续能力打磨成完整通知平台
- [ ] 真正理解人类
- [ ] 阻止作者继续重构

## 赞助小店

项目接受小额赞助或相关服务合作。具体介绍和链接后续补齐。

| 名称 | 说明 |
| --- | --- |
| <a href="https://codexauv.com/register?aff=UNKHTN42CDRT"><img src="docs/img/AUV_LOGO.png" alt="CodexAuv" width="200" /></a> | [CodexAuv](https://codexauv.com/register?aff=UNKHTN42CDRT) 提供面向开发者和企业团队的 AI 模型聚合与机器人托管服务 |

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
