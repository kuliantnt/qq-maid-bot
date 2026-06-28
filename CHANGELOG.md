# Changelog

本文档基于 [keep a changelog](https://keepachangelog.com/zh-CN/1.0.0/) 格式，记录每个已发布版本的变更。

## [v0.8.0] - 2026-06-28

### Added

- Provider 真实增量流贯通：OpenAI Responses、Chat Completions、DeepSeek、BigModel 全部支持真实 SSE delta 向上传递到 Core 和 Gateway
- `/查` 改为使用 `query_stream()` 真实流式搜索，不再人工切片伪装流式
- 后台异步自动标题，不再阻塞主聊天回复
- `SessionStore::update_title_if_current()` 条件更新接口，防止后台标题覆盖会话数据

### Changed

- 普通聊天改走 `LlmChatService::stream_respond()` 真实 Provider 流，不再等待完整结果后才返回
- ModelRoute 流式候选链：首个非空 delta 后不再静默切换候选模型
- 异常 EOF 正确识别为流失败，不再被吞为成功完成
- 自动标题异步化：生成不阻塞 Completed，失败不影响本轮聊天，通过 `health_observation=ignore` 避免覆盖主模型健康状态

### Fixed

- 修复后台标题旧 SessionRecord 快照覆盖新会话数据的并发问题：后台标题改为条件 SQL 更新，仅当前标题仍为默认值时才写入

### Internal

- `qq-maid-llm` 0.1.2 → 0.1.3（Provider 流式改造、SSE 跨 chunk 拼接修复、Web Search 真实流）
- `qq-maid-core` 0.1.9 → 0.1.10（CoreResponseStream TextDelta 真实传递、chat 流式路径、异步标题、会话条件更新）
- `qq-maid-gateway-rs` 0.1.3 → 0.1.4（持续消费 TextDelta，仍只发送最终 Completed）
- LlmStreamEvent / CoreResponseEvent 统一标准流事件
- ObservedProvider 接入真实流式 metrics 和健康观测
- stream=false 兼容：非流请求包装为单 TextDelta + Completed，维持进程内流边界一致
- 取消传播改进：receiver 关闭后 producer 及早停止转发，释放 Provider 流

## [v0.7.0] - 2026-06-28

### Added

- 长期记忆增加个人/群聊作用域，修复跨用户记忆泄露
- 接入 BigModel（大模型 API）provider，扩展 LLM 供应商支持

### Fixed

- 修复知识库 frontmatter 检索污染：frontmatter 属性值被 BM25 误命中问题
- 修复群聊 pending 操作发起人校验：防止跨用户操作待办和记忆
- 修复已完成待办删除语义：改为永久删除（原软删除导致残留）
- 修复已取消待办删除确认文案区分
- 修复待办序号快照逻辑和已取消待办清理
- 修复记忆管理改用列表序号显示
- 修复记忆目标解析 Clippy 告警

### Internal

- `qq-maid-core` 0.1.8 → 0.1.9（长期记忆作用域隔离、待办/记忆多项修复）
- `qq-maid-gateway-rs` 0.1.2 → 0.1.3（群聊 pending 校验修复）
- `qq-maid-llm` 0.1.1 → 0.1.2（接入 BigModel provider）

### Documentation

- 更新 README Rust 行数描述

## [v0.6.2] - 2026-06-27

### Fixed

- 修复未闭合 fenced code block 到 EOF 时最后一行代码被误当 closer 复制进每个切片，污染检索索引

### Internal

- `qq-maid-gateway-rs` 0.1.1 → 0.1.2（v0.6.1 群聊 active 关键词配置、安全拦截文案遗漏提升）
- `qq-maid-llm` 0.1.0 → 0.1.1（v0.6.1 safety_blocked 错误码与健康摘要脱敏遗漏提升）
- `qq-maid-core` 0.1.7 → 0.1.8（未闭合代码围栏分块修复）

### Documentation

- RAG V2 任务文档从 tasks/ 移入 done/

## [v0.6.1] - 2026-06-27

### Added

- 重构知识检索切片机制：升级切片版本到 V2，按目标/软/硬字符上限和段落类型（文本/列表/引用/代码/表格）分段；代码片保留语言标签和行数上限；headings 感知标题路径
- 知识模块拆分为 chunking、scan、search、text 四个子模块，增强 FTS5 多 rank 策略和评分调试诊断

### Fixed

- 修复知识库 CJK 查询 1-gram 单字噪声挤占 BM25 相关结果的问题：存在 2-gram 或 3-gram 时自动过滤单字
- 修复短 CJK 查询（如"D区""站"）因单字过滤导致无结果的问题：短查询保留 1-gram 作为唯一检索信号
- 修复知识目录无源文件时 DB 已有索引被清空的问题：保留已有索引，支持从生产环境拷贝 app.db 到新部署环境
- 修复 LLM 安全拦截（prompt_blocked）错误提示不友好：新增 `safety_blocked` 专用错误码，gateway 返回固定用户文案而不回显敏感原因
- 修复群聊 active 模式缺少可配置关键词：新增 `QQ_MAID_GROUP_ACTIVE_KEYWORDS` 配置项，默认关键词 `小女仆`；Core 端群聊 prompt 增加唤醒关键词提示

### Changed

- Prompt 示例模板去项目特定用语，改通用表述
- maid_system 示例模板新增知识库查阅规则：优先使用已有资料、不用保守套话、被指出查到了就切换整理模式

### Documentation

- README 重写：增加项目个性与趣味内容、运行快照、使用示例、快速开始指引和 badge；移除未实现的 OneBot 标识；补充本地知识检索能力说明
- 新增 RAG 切片检索 V2 任务规划文档（docs/tasks/rag-chunking-retrieval-v2.md，688 行），收窄存储与邻接方案
- DEVELOPMENT.md 与 tasks/ 目录移动至 docs/ 下，CONTRIBUTING.md 更新文档链接

### CI

- 恢复 CI 全量触发：本轮尝试了 CI paths 过滤（仅生产代码变更触发）后因 pull_request 事件匹配不稳定而撤销，恢复为所有 PR 和 push 均触发

## [v0.6.0] - 2026-06-26

### Added

- 新增 Markdown 知识 FTS5 全文检索，替换旧世界观和上下文模块，支持中文 ngram 分词、标题感知分段、slug 去重和 chunk_id 防碰撞
- 新增 `*.example.md` 模板文件跳过知识扫描机制，避免示例文档污染检索结果
- 新增 RSS 抓取开始水位记录，以阻塞同一 URL 的竞态更新，减少重复推送

### Changed

- 扩大知识搜索候选集：单文件命中后不再垄断结果，保留其他文件的相关片段
- 整理 `.env.example`，删除死变量和旧兼容变量，精简注释
- 清理 `.gitignore` 失效规则并补全部署产物忽略

### Fixed

- 修复 RSS 历史回写条目不再误触发补发，仅保留真实 incident 更新入队
- 修复 RSS 延迟更新入队判断逻辑
- 修复知识索引评论问题
- 修复群聊默认 mention 策略缺失
- 补齐私有配置忽略规则

### Documentation

- AGENTS.md 新增分支与 PR 策略章节
- 修正和收紧 opencode-go 任务描述

## [v0.5.0] - 2026-06-25

### Added

- 新增独立 `qq-maid-llm` library crate，承载模型调用协议、Provider 路由、fallback、SSE、usage、健康观测和 OpenAI Web Search 协议；不依赖 `qq-maid-core`
- 新增 `qq-maid-llm/README.md`，说明 crate 职责边界、统一入口、模块结构、配置边界和调用链
- 依赖方向固定为 `qq-maid-gateway-rs → qq-maid-core → qq-maid-llm → qq-maid-common`，禁止反向依赖

### Changed

- 将 OpenAI Responses、Chat Completions、DeepSeek、模型候选链、SSE 解析、LLM metrics、健康观测和 `/查` Web Search 协议从 `qq-maid-core` 迁入 `qq-maid-llm`
- OpenAI Chat Completions 改为基于 `reqwest` 的自研实现，支持流式与非流式、`[DONE]`、usage 与 cached token 提取、空流补非流、401/403/429/timeout/5xx 与非标准错误正文分类
- DeepSeek 复用 OpenAI 兼容 Chat Completions adapter，不再维护独立 SDK 封装，只保留 base URL、认证和模型规则差异
- 聊天 Responses 与 `/查` Web Search 共用同一套 SSE frame 解析（`qq-maid-llm/src/sse.rs`），消除重复实现
- `qq-maid-core` 改为通过 `LlmService::chat` / `web_search` / `web_search_stream` / `upstream_status` 公开入口调用模型；core 侧 `provider/` 仅作为兼容 re-export 入口
- `LlmError` 收敛为仅模型调用相关错误（Provider 配置、网络/超时、HTTP 上游、SSE/协议、空回复、候选全部失败）；core 在 LLM 调用边界完成错误转换，保持用户侧错误格式和文案兼容
- 普通聊天、标题、Todo、记忆、Compact、翻译和 `/查` 全部切换到新的 `qq-maid-llm` 调用链，Prompt 组装位置、模型选择、fallback 顺序、用户侧回复内容和格式、健康检查数据保持不变
- README 文档导航补充 `qq-maid-llm/README.md` 链接
- AGENTS.md 同步更新项目定位、依赖方向、代码修改边界和常用验证规则

### Removed

- 从 `Cargo.toml`、`Cargo.lock` 和源码中完全移除 `rig-core` 依赖
- 删除 `qq-maid-core` 中已迁移的 Provider 实现（`provider/deepseek.rs`、`provider/openai/`、`provider/status.rs`、`util/sse.rs` 等）
- 删除 `/查` 中已迁移的模型协议代码

### Fixed

- 修复聊天 Responses 与 `/查` 各自维护一套 SSE frame 解析导致的重复实现问题

## [v0.4.5] - 2026-06-25

### Changed

- 调整普通聊天消息排列：稳定 system prompt 前置，请求时间上下文移到稳定前缀之后、记忆与会话上下文之前，避免每轮请求时间变化破坏 Prompt Cache 前缀命中
- `TokenUsage` 新增 `cached_input_tokens` 字段，采集上游 `input_tokens_details.cached_tokens` / `prompt_tokens_details.cached_tokens`，字段缺失时记 `None`，不伪造 0
- `provider/openai/chat.rs` 在流式与非流式补全中从原始响应提取缓存命中 token 数，rig-core `Usage` 已提供该字段时优先复用
- `provider/openai/extract.rs::extract_response_usage` 解析 `input_tokens_details.cached_tokens`
- `LlmChatService` 在每次请求完成后输出脱敏结构化日志 `llm request completed`，记录 provider、model、purpose、input/output/cached tokens、fallback_used

### Fixed

- 修复请求时间上下文注入头部导致稳定 prompt 前缀每轮被顶位、Prompt Cache 无法命中稳定前缀的问题

## [v0.4.4] - 2026-06-25

### Added

- 新增可配置上下文模块：支持通过 `CONTEXT_MODULES_FILE` 指向 TOML 索引文件，按关键词动态注入普通聊天的 system prompt 模块
- 模块支持 `always` 常驻、`keywords` 关键词命中、`priority` 优先级排序、`max_dynamic_modules` / `max_total_chars` 预算控制、路径逃逸校验
- 新增 `context_modules.example.toml` 公开模板，新增 `context/deploy.example.md` / `context/ops.example.md` 示例模块

### Changed

- 重构 `runtime::prompt` 模块，拆分为 `prompt_files`（固定 prompt 加载）、`member_mapping`（成员编号映射）、`context_modules`（可配置上下文模块）三个子模块
- 世界观不再强绑定 `innerworld_lore.md`，改为通过 `WORLD_FILE` 环境变量独立指定

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

[v0.8.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.7.0...v0.8.0
[v0.7.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.6.2...v0.7.0
[v0.6.2]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.6.1...v0.6.2
[v0.6.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.6.0...v0.6.1
[v0.6.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.5.0...v0.6.0
[v0.5.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.4.5...v0.5.0
[v0.3.4]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.3...v0.3.4
[v0.3.3]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.2...v0.3.3
[v0.3.2]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.0...v0.3.2
[v0.3.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.0...v0.3.1
[v0.3.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.2.0...v0.3.0
[v0.2.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.1.0...v0.2.0
[v0.1.0]: https://github.com/kuliantnt/qq-maid-bot/commits/v0.1.0
