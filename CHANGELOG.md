# Changelog

本文档基于 [keep a changelog](https://keepachangelog.com/zh-CN/1.0.0/) 格式，记录每个已发布版本的变更。

## [v0.4.3] - 2026-06-25

### Fixed

- 修复 QQ 群事件兼容期内同时下发 `group_openid` 和旧字段 `group_id` 时，serde alias 导致 duplicate field 报错的问题：改为手动合并两个字段，优先使用 `group_openid`

## [v0.4.2] - 2026-06-25

### Changed

- 优化 `/火车` 时刻表渲染输出：始发站到达列显示 `--`、停留显示「始发」；终到站出发列显示 `--`、停留显示「终到」；中间站保持原来到发时间和停留分钟数逻辑；仅一站的异常数据保留原始到发、停留显示 `--`，不同时硬标为始发和终到
- `/火车` 时刻表新增展示 12306 `trainDetail` 可选字段：完整车次（`stationTrainCodeAll`）、担当客运段（`jiaolu_corporation_code`）、车型信息（`jiaolu_train_style`）、配属（`jiaolu_dept_train`）；字段缺失或为空时省略对应行，不推测、不补造
- 时刻表底部提示调整为「当前展示为当日计划时刻，不含实时正晚点、余票及临时停运信息，请以铁路12306或车站公告为准。」
- 空到发时间占位由 `--:--` 改为 `--`

## [v0.4.1] - 2026-06-25

### Fixed

- 修复 `/help` 首页"常用功能"区块在 QQ 纯文本渲染下命令被反引号吞掉不显示的问题：将待办、RSS、天气、查询、记忆、会话、状态等条目从 `bullet` 改为 `push_pair`，纯文本侧去掉反引号，Markdown 侧保留行内代码标记

## [v0.4.0] - 2026-06-24

### ⚠️ 破坏性变更：从双进程合并为单进程

**从此版本开始，项目不再分别运行 Gateway 和 LLM 两个独立程序，改为运行一个统一程序 `qq-maid-bot`。**

如果你从旧版（≤ v0.3.4）升级，请务必按以下步骤操作，否则会出现端口冲突导致新程序无法启动：

**升级步骤：**

```bash
# 1. 先停掉旧版两个独立进程
kill $(ps aux | grep qq-maid-gateway-rs | grep -v grep | awk '{print $2}')
kill $(ps aux | grep qq-maid-llm | grep -v grep | awk '{print $2}')
# 或如果旧版有 llmctl.sh / gatewayctl.sh：
# bash llmctl.sh stop
# bash gatewayctl.sh stop

# 2. 确认旧进程已全部退出
ps aux | grep -E 'qq-maid-(gateway-rs|llm)' | grep -v grep

# 3. 清理旧的独立二进制和脚本（新版部署时会自动清理）
rm -f runtime/qq-maid-gateway-rs runtime/qq-maid-llm
rm -f runtime/llmctl.sh runtime/gatewayctl.sh

# 4. 按新版方式构建和部署
bash scripts/deploy-local.sh
```

**为什么必须这样做：**
- 旧版 Gateway 和 LLM 各占一个端口独立运行；
- 新版 `qq-maid-bot` 单进程内部串联两模块，复用相同端口；
- 如果旧进程未退出，新版启动时端口被占用，会直接失败。

### Changed

- 将 Gateway (`qq-maid-gateway-rs`) 和 Core（原 `qq-maid-llm`）合并为一个统一可执行程序 `qq-maid-bot`
- `qq-maid-llm` crate 重命名为 `qq-maid-core`，定位更清晰
- Gateway 和 Core 改为 library crate，仅作为根包的依赖使用
- 统一入口 `src/main.rs`：先启动 Core HTTP，等待 `/healthz` 就绪后再启动 Gateway
- 所有部署、启停、诊断脚本切换为只操作 `qq-maid-bot` 统一程序
- `botctl.sh` 替代旧的 `llmctl.sh` / `gatewayctl.sh`
- Gateway 仍通过本机 HTTP 调用 Core `/v1/respond`，业务边界不变

### Fixed

- 修复 `todo_reminder` 测试在非上海时区（如 CI 的 UTC）下跨天失败：测试改用上海时区取当前日期，与调度器内部 `next_retry_after` 时区语义一致

### Removed

- 移除 `qq-maid-llm/src/main.rs`、`qq-maid-gateway-rs/src/main.rs` 两个独立入口
- 移除 `scripts/llmctl.sh`、`scripts/gatewayctl.sh` 双进程控制脚本
- 清理 Makefile 中旧的 `run-llm`、`run-gateway` 等双服务目标

## [v0.3.4] - 2026-06-24

### Added
- `/todo add` 支持火车行程识别与 12306 时刻校验：
  - LLM 解析车次/站点/日期后调用 12306 接口校验站点存在性与站序
  - 支持跨日行程时间计算（`dayDifference` 字段）
  - 校验失败或接口异常时不创建 Todo，返回针对性提示
  - 支持纯数字车次（如 1461）与字母前缀车次（G/D/C/Z/T/K）
  - `NotTrain` 识别结果自动回退普通 Todo 解析
- Todo 每日提醒后台调度（`runtime/todo_reminder.rs`）：
  - 按 Asia/Shanghai 每日定时扫描 pending 个人待办
  - 只推送可验证 private target 的 owner，群待办不主动推送
  - 同 owner 多 private scope 合并，冲突 target 脱敏跳过
  - 每日每 owner 仅发送一次，当天失败会自动补跑重试
- 通用 GatewayPushClient（`runtime/push.rs`），供 RSS 与 Todo 提醒共用
- 12306 接口 `stationNo`/`dayDifference` 兼容兜底，缺失时不阻塞 `/火车` 查询
- `day_difference_reliable` 字段：Provider 层可为展示兜底，校验层拒绝不可信跨日数据
- 火车 Todo 回看候选 `startDay` 逻辑：首个候选报站点错误时继续查询，避免误杀跨日行程
- 配置项 `TODO_DAILY_REMINDER_ENABLED` / `TODO_DAILY_REMINDER_TIME`

### Changed
- README 补充 Todo 每日提醒能力说明
- `.env.example` 补充推送入口说明

## [v0.3.3] - 2026-06-21

### Added
- Web 控制台路由安全头中间件（X-Content-Type-Options、X-Frame-Options、CSP）
- `scripts/validate-release-runtime.sh` — 待发布 runtime 目录完整性校验脚本
- `scripts/botctl.sh` — 统一启停控制脚本（start/stop/restart/status/logs/health/console）
- 群消息 `group_message_mode` 配置项，支持 `off` / `command` / `mention` / `active`
- OpenAI 兼容 GLM provider 支持
- `qq-maid-gateway-rs` 推送到 `qq-maid-llm` 内部 `/internal/push` 端点，支持群 @ 消息透传

### Changed
- `deploy.sh` 增加构建产物校验和缺失检测
- `Makefile install` target 正确拷贝 release 二进制和控制脚本到 `runtime/`
- `scripts/llmctl.sh` 增加 `LINES` 日志行数配置支持与 `console` 子命令
- `runtime/.env.example` 更新配置项
- `qq-maid-llm` Web 控制台功能：配置开关、CORS 管理、Markdown 渲染接口

### Fixed
- 移除 unused import 警告
- `cargo fmt` 格式化修复
- 修复 console 测试断言与 HTML 标题不一致

## [v0.3.2] - 2026-06-20

### Added
- 运行时目录校验与发布包校验脚本
- OpenAI `chat_only` 模式、可选群聊处理与运维脚本
- `git clone` 后本地部署快速开始指南与 `scripts/deploy-local.sh`
- todo ID 隐藏，统一使用列表序号和关键词匹配
- 命令回复独立的 Markdown 与纯文本双通道
- LLM 上游调用健康状态观测，支持 `/ping check` 诊断
- install 目标将构建产物安装到 `runtime/`

### Changed
- 天气模块拆分为 `types/qweather` 子模块，回复格式改为 Markdown
- 抽取分层帮助模块替换内联 `/help` 回复
- 简化 todo target 解析分支
- `README-dev.md` 重命名为 `DEVELOPMENT.md`

### Fixed
- 群 push 返回的 message_id 写入共享 BotOutboundCache
- 修复群聊作用域与部署控制台回归
- 安全增强、部署加固及脚本一致性优化

## [v0.3.1] - 2026-06-19

### Fixed
- 为 Windows 构建添加 zip 安装步骤

## [v0.3.0] - 2026-06-19

### Added
- 扩展多平台发布矩阵，支持 Linux/Windows/macOS/Android 六平台构建

### Changed
- `/ping` 模块拆分为子模块
- 超长文件拆分与 `markdown_cell` 换行逻辑精简

## [v0.2.0] - 2026-06-18

### Added
- `/ping` 添加摘要/详情双视图和 Markdown 支持
- GitHub Actions 依赖版本升级

## [v0.1.0] - 2026-06-18

首个公开可用版本，从私有仓库迁移而来。

### 项目基础设施
- Rust 双服务架构：Gateway 接收 QQ 事件，LLM 承载业务逻辑
- Cargo Workspace 统一管理 `qq-maid-gateway-rs`、`qq-maid-llm`、`qq-maid-common`
- QQ 官方机器人接入，处理 C2C 私聊和群聊 @ 消息
- SQLite 统一持久化 Session、Todo、Memory、RSS 状态
- OpenAI / DeepSeek 多 Provider 支持，候选链 fallback
- LLM 流式回复、空回复重试、verbose trace
- 服务控制脚本、make 诊断、部署脚本

### 会话管理
- 新建、重命名、恢复、清空会话
- 会话上下文自动压缩与标题自动生成
- Session 存储从 JSON 文件迁移至 SQLite

### 长期记忆
- `/memory` 指令生成草稿，用户确认后写入
- 记忆编辑、删除、查看，按序号管理
- 记忆存储从 JSONL 迁移至 SQLite

### Todo
- 新增、查询、完成、恢复、修改、删除待办
- 按截止时间排序，软删除语义
- `/todo done` 无参列出已完成，`/todo all` 列出全部状态
- Todo 存储从 JSON 文件迁移至 SQLite

### RSS / Atom
- 订阅管理、后台轮询、去重
- 通过 Gateway `/internal/push` 主动推送
- 外语标题/摘要自动翻译为简体中文
- RSS 专用 SQLite 迁移为通用数据库模块

### 查询与命令
- `/查`、`/查询`、`/search` 联网查询
- `/火车` 列车时刻查询
- `/天气` 和风天气查询（含预警、空气质量、生活指数）
- `/翻译` 多语言翻译
- 命令回复支持 Markdown 渲染

### 配置与运维
- 环境变量统一配置入口
- Prompt 目录外部配置与内置回退
- 成员 ID 映射、世界观文件支持
- 日志时间固定上海时区、默认脱敏
- Gateway 运行时诊断与状态快照

### 代码质量
- todo_flow、openai、respond 等模块持续拆分为子模块
- SSE 解析工具、公共 chat primitives 抽取复用
- 移除已废弃的 Python 接入层和旧 Provider
- rig-core 升级至 0.38.2

[v0.3.4]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.3...v0.3.4
[v0.3.3]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.2...v0.3.3
[v0.3.2]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.0...v0.3.2
[v0.3.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.0...v0.3.1
[v0.3.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.2.0...v0.3.0
[v0.2.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.1.0...v0.2.0
[v0.1.0]: https://github.com/kuliantnt/qq-maid-bot/commits/v0.1.0
